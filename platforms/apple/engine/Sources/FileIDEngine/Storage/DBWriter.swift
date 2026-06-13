// DB Writer — Stage C of the v2 pipeline.
//
// Single owner of the GRDB writer connection. Receives `TaggedFile` records
// from the tagging stage via an AsyncChannel, buffers them up to
// (100 files OR 50 ms wall, whichever fires first), then commits one
// transaction.
//
// This is the architectural fix for the v1 result-loop funnel: 14 workers
// fan into one DB Writer, but the writer transacts in batches, so SQLite
// commit overhead amortizes across 100 inserts instead of being paid per file.
import Foundation
import GRDB
import AsyncAlgorithms
import FileIDShared

/// What the tagging stage emits per file. M2 carries face/OCR/dHash/EXIF
/// only — embeddings join the struct in M3.
public struct TaggedFile: Sendable {
    public let url: URL
    public let kind: String                  // image|video|pdf|doc|audio|other
    public let `extension`: String
    public let sizeBytes: Int64
    public let createdAt: Date?
    public let modifiedAt: Date?

    /// Volume-local file identity (APFS/HFS inode, st_ino), propagated from
    /// `DiscoveredFile.fileRef`. Powers DBWriter's rename/move heal: a moved file
    /// with a matching `fileRef` whose old path is gone re-binds the existing row
    /// (id + tags/faces/OCR/embeddings) instead of orphaning it. nil when stat
    /// failed at discovery (no heal possible). The macOS analog of the Windows
    /// NTFS MFT file_ref (dbwriter.rs).
    public var fileRef: UInt64?

    // Tagging output — empty arrays for files we couldn't process.
    public var visionTags: [String]          // raw Vision classifier labels
    public var phash: UInt64?                // dHash (0 = none / failed)
    public var aestheticScore: Double?       // 0..1
    public var hasFaces: Bool
    public var facePrints: [Data]            // archived VNFaceObservation feature prints
    public var faceBBoxes: [String]          // normalized "x,y,w,h" per face
    public var faceQualities: [Double]       // 0..1, parallel to faceBBoxes; -1 = unmeasured
    public var faceYaws: [Double?]           // radians, parallel to faceBBoxes; nil = missing
    public var facePitches: [Double?]        // radians, parallel to faceBBoxes; nil = missing
    public var ocrText: String?              // empty/nil if no text or skipped
    public var cameraModel: String?
    public var locationLat: Double?
    public var locationLon: Double?
    public var failed: Bool
    public var errorMessage: String?
    public var perFileTotalMs: Double        // wall time inside the worker
    // Iteration 5 — per-stage breakdown so the batch profiler can show where
    // the per-file budget actually goes. All in ms; 0 if not measured.
    public var loadMs: Double = 0
    public var visionMs: Double = 0
    public var clipMs: Double = 0
    public var ocrMs: Double = 0

    // M3 — CLIP image embedding (raw float32 little-endian bytes). nil for
    // non-images, files where the model isn't loaded, or inference failures.
    public var clipEmbeddingBlob: Data?

    // Stage-ran gates (port of the Windows tags_evaluated / faces_evaluated /
    // ocr_stage_ran flags, dbwriter.rs). Each is TRUE only when its producing
    // stage actually ran AND returned this session — never on a Vision/ANE/OCR
    // timeout. DBWriter.insertOne keys its `DELETE` on these so a swallowed
    // timeout (empty result) cannot wipe a file's previously-persisted auto-tags,
    // OCR text + FTS, or — critically — manual person_id assignments on a
    // rescan. Default FALSE: the safe, no-delete state for any partial row.
    public var tagsEvaluated: Bool
    public var facesEvaluated: Bool
    public var ocrStageRan: Bool

    public init(
        url: URL, kind: String, extension ext: String, sizeBytes: Int64,
        createdAt: Date?, modifiedAt: Date?,
        fileRef: UInt64? = nil,
        visionTags: [String] = [], phash: UInt64? = nil,
        aestheticScore: Double? = nil, hasFaces: Bool = false,
        facePrints: [Data] = [], faceBBoxes: [String] = [],
        faceQualities: [Double] = [],
        faceYaws: [Double?] = [], facePitches: [Double?] = [],
        ocrText: String? = nil, cameraModel: String? = nil,
        locationLat: Double? = nil, locationLon: Double? = nil,
        failed: Bool = false, errorMessage: String? = nil,
        perFileTotalMs: Double = 0,
        clipEmbeddingBlob: Data? = nil,
        tagsEvaluated: Bool = false,
        facesEvaluated: Bool = false,
        ocrStageRan: Bool = false
    ) {
        self.url = url
        self.kind = kind
        self.extension = ext
        self.sizeBytes = sizeBytes
        self.createdAt = createdAt
        self.modifiedAt = modifiedAt
        self.fileRef = fileRef
        self.visionTags = visionTags
        self.phash = phash
        self.aestheticScore = aestheticScore
        self.hasFaces = hasFaces
        self.facePrints = facePrints
        self.faceBBoxes = faceBBoxes
        self.faceQualities = faceQualities
        self.faceYaws = faceYaws
        self.facePitches = facePitches
        self.ocrText = ocrText
        self.cameraModel = cameraModel
        self.locationLat = locationLat
        self.locationLon = locationLon
        self.failed = failed
        self.errorMessage = errorMessage
        self.perFileTotalMs = perFileTotalMs
        self.clipEmbeddingBlob = clipEmbeddingBlob
        self.tagsEvaluated = tagsEvaluated
        self.facesEvaluated = facesEvaluated
        self.ocrStageRan = ocrStageRan
    }
}

public actor DBWriter {
    private let db: Database
    private let sink: IPCSink
    private let coordinator: ScanCoordinator
    private let sessionID: String

    private var batchIndex = 0
    private var processedTotal = 0
    private var failedTotal = 0
    private var consecutiveCommitFailures = 0
    private var abortedForWriteFailure = false

    /// Consecutive failed batch commits before the writer cancels the scan.
    /// A persistent failure (disk full, DB unlinked) otherwise lets the full
    /// ANE/CLIP pipeline burn hours producing batches that all drop — one
    /// error event each — while the session still ends "completed".
    private let maxConsecutiveCommitFailures = 3

    /// Tunables — both sides act as a ceiling. Whichever fires first ends
    /// the buffer window and triggers a commit. Bumped from 50ms → 200ms in
    /// iteration 2: workers produce ~7 files / 60 ms steady state, so the
    /// 50ms ceiling fired before batches could grow past ~10 files. 200ms
    /// lets batches accumulate to ~30 files, cutting commit overhead 3-4×.
    private let maxBatchFiles = 100
    private let maxBatchMs    = 200

    /// startScan's `rescan` flag: true forces every file through the full
    /// child-row rebuild even when size+mtime say it's unchanged. Mirrors
    /// the Windows engine (rescan=true empties the incremental skip set).
    private let forceReprocess: Bool

    public init(db: Database, sink: IPCSink, coordinator: ScanCoordinator, sessionID: String,
                forceReprocess: Bool = false) {
        self.db = db
        self.sink = sink
        self.coordinator = coordinator
        self.sessionID = sessionID
        self.forceReprocess = forceReprocess
    }

    /// A full batch handed off to the committer task. Sendable so it can cross
    /// the rendezvous channel between the drain loop and the committer.
    private struct PendingBatch: Sendable {
        let files: [TaggedFile]
        let startedAt: Date
    }

    /// Drain `taggedChannel` until the channel closes (= tagging stage done).
    /// Called from the engine's main task; the writer keeps going until the
    /// upstream is exhausted, then performs a final flush.
    ///
    /// The DB transaction (and its WAL checkpoint) is DECOUPLED from this loop:
    /// assembled batches are handed to a single committer task over a rendezvous
    /// channel, so a commit runs CONCURRENTLY with the next batch being pulled
    /// off `channel`. The old inline `await commit` stalled every tagging worker
    /// for the full transaction duration — a busy writer stops draining the
    /// unbuffered taggedChan, so the workers' `send` parks. At most one batch
    /// commits while one assembles (bounded memory ≈ two batches); a full batch
    /// assembled before the prior commit finishes still blocks on
    /// `commitChan.send`, which preserves back-pressure and the batch bounds.
    /// (F-C6-004)
    public func drain(_ channel: AsyncChannel<TaggedFile>) async {
        let commitChan = AsyncChannel<PendingBatch>()
        let committer = Task { [self] in
            for await pending in commitChan {
                await self.commitBatch(pending.files, batchStart: pending.startedAt)
            }
        }

        var buffer: [TaggedFile] = []
        buffer.reserveCapacity(maxBatchFiles)
        var batchStart = Date()

        for await file in channel {
            buffer.append(file)
            // Trigger commit on either ceiling.
            let elapsedMs = Date().timeIntervalSince(batchStart) * 1000
            if buffer.count >= maxBatchFiles || elapsedMs >= Double(maxBatchMs) {
                let batch = buffer
                buffer.removeAll(keepingCapacity: true)
                await commitChan.send(PendingBatch(files: batch, startedAt: batchStart))
                batchStart = Date()
            }
        }
        // Tail flush — anything still buffered when upstream closes.
        if !buffer.isEmpty {
            let batch = buffer
            buffer.removeAll(keepingCapacity: true)
            await commitChan.send(PendingBatch(files: batch, startedAt: batchStart))
        }
        // Close the handoff and wait for the committer to finish the in-flight +
        // tail batches, so `await writerTask.value` upstream still observes a
        // fully-flushed, durable DB before the terminal scan event fires.
        commitChan.finish()
        await committer.value
    }

    // MARK: - Commit

    /// Commit one assembled batch. Runs on the actor from the single committer
    /// task, so batches commit serially (single-writer DB) and the counters /
    /// `batchIndex` are mutated without races, while the actual `pool.write`
    /// suspension lets the drain loop keep pulling files concurrently.
    private func commitBatch(_ batchFiles: [TaggedFile], batchStart: Date) async {
        guard !batchFiles.isEmpty else { return }

        let insertStart = Date()
        // Compute counts up-front so the closure doesn't need to mutate them.
        let insertedOK     = batchFiles.filter { !$0.failed }.count
        let insertedFailed = batchFiles.count - insertedOK
        let resumeCursor   = self.processedTotal + self.failedTotal + batchFiles.count
        let sessionID      = self.sessionID
        let forceReprocess = self.forceReprocess

        do {
            // Retry transient failures (SQLITE_BUSY/LOCKED/IOERR) with backoff
            // before giving up — a momentary WAL contention or I/O blip used to
            // drop the entire in-flight batch (up to 100 files) silently.
            // `batchFiles` is a value copy, so retrying re-runs the same inserts
            // safely (each is an idempotent upsert).
            var attempt = 0
            while true {
                do {
                    try await db.pool.write { db in
                        for file in batchFiles {
                            try Self.insertOne(file: file, forceReprocess: forceReprocess, db: db)
                        }
                        // Update scan session high-water mark in the SAME
                        // transaction so a crash mid-batch can't leave the
                        // resume cursor pointing past the last committed file.
                        try db.cachedStatement(sql: """
                            UPDATE scan_sessions SET last_file_index = ? WHERE id = ?
                            """).execute(arguments: [resumeCursor, sessionID])
                    }
                    break
                } catch {
                    attempt += 1
                    guard attempt < 3, Self.isTransient(error) else { throw error }
                    JSONLog.shared.warn(ev: "db_write_retry", sess: sessionID,
                                        error: "attempt \(attempt): \(error)")
                    try? await Task.sleep(nanoseconds: UInt64(attempt) * 100_000_000)
                }
            }
            self.processedTotal += insertedOK
            self.failedTotal    += insertedFailed
            self.consecutiveCommitFailures = 0
            await self.coordinator.bumpProcessed(by: insertedOK)
            await self.coordinator.bumpFailed(by: insertedFailed)
        } catch {
            // A write failure is serious — surface as an event, log to JSONL,
            // and increment the failure counter so the user notices.
            JSONLog.shared.error(ev: "db_write_failed", sess: sessionID,
                                 error: "\(error)")
            // Translate raw SQLite errors into actionable advice. The most
            // common case in practice is "the .sqlite file got unlinked
            // out from under us" — happens when the user re-runs run.sh
            // while a previous engine is still scanning. Surfacing the raw
            // SQL string isn't useful; tell them what to actually do.
            // Categorize on the SQLite result code (robust against error-string
            // wording changes across SQLite versions); fall back to the message.
            let primary = (error as? DatabaseError)?.resultCode.primaryResultCode
            let userMessage: String
            switch primary {
            case .SQLITE_IOERR:
                userMessage = "The database is no longer reachable mid-scan (SQLite I/O error). This usually means another FileID process held the .sqlite file. Quit FileID completely (⌘Q), then re-run ./run.sh — it now kills stale processes before wiping."
            case .SQLITE_BUSY, .SQLITE_LOCKED:
                userMessage = "Another FileID process is holding the database. Quit FileID (⌘Q) and re-run ./run.sh."
            case .SQLITE_FULL:
                userMessage = "Disk full — free space and re-run."
            default:
                userMessage = "Batch \(self.batchIndex) write failed: \(error)"
            }
            self.consecutiveCommitFailures += 1
            if self.abortedForWriteFailure {
                // Already cancelled — suppress further UI events for batches
                // that were still in flight when the abort fired.
            } else if self.consecutiveCommitFailures >= self.maxConsecutiveCommitFailures {
                self.abortedForWriteFailure = true
                await self.coordinator.requestCancel()
                await sink.emit(.error(EngineError(
                    kind: "db_write_failed",
                    message: userMessage + " Scan stopped — nothing was being saved."
                )))
            } else {
                await sink.emit(.error(EngineError(
                    kind: "db_write_failed",
                    message: userMessage
                )))
            }
        }

        let insertDur = Date().timeIntervalSince(insertStart)
        let wall = Date().timeIntervalSince(batchStart)
        self.batchIndex += 1

        // Per-stage timing aggregation: aggregate the per-file timings the
        // workers recorded so the batch summary tells us where time went.
        let perFileTimes = batchFiles.map { $0.perFileTotalMs }.sorted()
        let perFileP50 = percentile(perFileTimes, 0.50)
        let perFileP95 = percentile(perFileTimes, 0.95)
        let perFileSum = perFileTimes.reduce(0, +)
        // Per-stage P50s — only over images (non-zero entries). Tells us
        // where the per-file budget actually goes: load (NAS I/O), Vision
        // (ANE primary pass), CLIP (ANE embedder), OCR (text-only ANE).
        let loadTimes = batchFiles.map(\.loadMs).filter { $0 > 0 }.sorted()
        let visionTimes = batchFiles.map(\.visionMs).filter { $0 > 0 }.sorted()
        let clipTimes = batchFiles.map(\.clipMs).filter { $0 > 0 }.sorted()
        let ocrTimes = batchFiles.map(\.ocrMs).filter { $0 > 0 }.sorted()
        // Worker utilization: what fraction of the 14-worker × wall time was
        // actually spent doing per-file work. <50% = workers idle (we have
        // headroom for more parallelism); >80% = ANE/IO bound.
        let utilization = wall > 0
            ? min(1.0, (perFileSum / 1000.0) / (wall * Double(Hardware.workerCap)))
            : 0

        // Per-batch profiler line. The Batch 12 profiler proved its worth —
        // structured JSONL replaces freeform `scan.log`. Workers can sample
        // their own timings; this records the WRITE side.
        JSONLog.shared.info(
            ev: "batch",
            sess: sessionID,
            extra: [
                "batchIndex":    AnyCodable(self.batchIndex),
                "files":         AnyCodable(batchFiles.count),
                "wallMs":        AnyCodable(wall * 1000),
                "insertMs":      AnyCodable(insertDur * 1000),
                "filesPerSec":   AnyCodable(wall > 0 ? Double(batchFiles.count) / wall : 0),
                "processedTotal":AnyCodable(self.processedTotal + self.failedTotal),
                "residentMB":    AnyCodable(Hardware.residentMB()),
                "availableMB":   AnyCodable(Hardware.availableMemoryMB()),
                "perFileP50Ms":  AnyCodable(perFileP50),
                "perFileP95Ms":  AnyCodable(perFileP95),
                "utilization":   AnyCodable(utilization),
                "loadP50Ms":     AnyCodable(percentile(loadTimes, 0.50)),
                "loadP95Ms":     AnyCodable(percentile(loadTimes, 0.95)),
                "visionP50Ms":   AnyCodable(percentile(visionTimes, 0.50)),
                "visionP95Ms":   AnyCodable(percentile(visionTimes, 0.95)),
                "clipP50Ms":     AnyCodable(percentile(clipTimes, 0.50)),
                "clipP95Ms":     AnyCodable(percentile(clipTimes, 0.95)),
                "ocrP50Ms":      AnyCodable(percentile(ocrTimes, 0.50)),
                "ocrP95Ms":      AnyCodable(percentile(ocrTimes, 0.95)),
                "imagesInBatch": AnyCodable(visionTimes.count)
            ]
        )

        // XPC event — UI uses BatchSummary to show the throughput chip + a
        // "Last batch: N files in Xms" badge once we wire the read-side view.
        // visionP50/clipP50 not separately tracked yet; perFileP50 is the
        // total per-file time and is the most actionable single metric.
        await sink.emit(.batchSummary(BatchSummary(
            batchIndex: self.batchIndex,
            filesInBatch: batchFiles.count,
            processedTotal: self.processedTotal + self.failedTotal,
            wallSeconds: wall,
            filesPerSecond: wall > 0 ? Double(batchFiles.count) / wall : 0,
            utilization: utilization,
            visionP50Ms: perFileP50, visionP95Ms: perFileP95,
            clipP50Ms: 0, clipP95Ms: 0,
            storeInsertP50Ms: insertDur * 1000 / Double(max(batchFiles.count, 1)),
            storeInsertP95Ms: insertDur * 1000,
            residentMB:  Hardware.residentMB(),
            availableMB: Hardware.availableMemoryMB()
        )))

        // Periodic PASSIVE WAL checkpoint, OUTSIDE the just-closed batch
        // transaction, so the -wal file is trimmed incrementally instead of
        // hitting the 10000-page autocheckpoint ceiling and doing one large
        // synchronous checkpoint copy inside a future commit. Best-effort and
        // non-blocking; mirrors the Windows per-32-batch cadence. (F-C6-004)
        if self.batchIndex % Self.walCheckpointBatches == 0 {
            await self.db.checkpointPassive()
        }
    }

    /// Commits between periodic PASSIVE WAL checkpoints. Windows-parity (32).
    private static let walCheckpointBatches = 32

    /// Transient SQLite failures worth retrying (vs. a hard schema/constraint
    /// error). Keys on the primary result code, not the message string.
    private nonisolated static func isTransient(_ error: Error) -> Bool {
        guard let code = (error as? DatabaseError)?.resultCode.primaryResultCode else { return false }
        return code == .SQLITE_BUSY || code == .SQLITE_LOCKED || code == .SQLITE_IOERR
    }

    private nonisolated static func percentile(_ sorted: [Double], _ p: Double) -> Double {
        guard !sorted.isEmpty else { return 0 }
        let idx = min(sorted.count - 1, Int(Double(sorted.count - 1) * p))
        return sorted[idx]
    }
    private nonisolated func percentile(_ sorted: [Double], _ p: Double) -> Double {
        Self.percentile(sorted, p)
    }

    /// Insert one TaggedFile across `files` + `tags` + `ocr_text` (+FTS5) +
    /// `face_prints`. All under the caller's open transaction so a single
    /// failure rolls back this file but not the rest of the batch.
    /// Static + nonisolated so the GRDB write closure can call it without
    /// crossing the actor's executor (Swift 6 strict concurrency).
    private static func insertOne(file: TaggedFile, forceReprocess: Bool, db: GRDB.Database) throws {
        // NOT String.hashValue — that's per-process seeded, so every engine
        // launch would mint a different value, breaking the stable
        // cross-platform path_hash contract (path_safety.rs stable_path_hash).
        let pathHash = StablePathHash.hash(file.url.path)

        // Look up any existing row for this path. We need its id (to preserve
        // it) and its size/mtime/failed state (to decide whether the file
        // actually changed). The old `INSERT OR REPLACE` deleted+reinserted the
        // row on every re-scan, which — via ON DELETE CASCADE — wiped
        // face_prints (including the user's MANUAL person assignments), CLIP
        // embeddings, and FTS rows, and minted a brand-new rowid each time.
        // GRDB's `execute(sql:)` / `fetchOne(_:sql:)` compile a fresh statement
        // every call; `cachedStatement` reuses one compiled plan per SQL for the
        // life of the (pool-reused) writer connection, so the hot per-file path
        // re-binds instead of re-parsing — the macOS analogue of the Windows
        // prepared/cached-statement path (dbwriter.rs). (F-C6-010)
        var existing = try Row.fetchOne(
            db.cachedStatement(sql: """
                SELECT id, size_bytes, modified_at, failed FROM files WHERE path_text = ?
                """),
            arguments: [file.url.path])

        // Rename/move heal — port of dbwriter.rs (heal_candidate_moved). Run ONLY
        // when no row yet sits at THIS path (a genuinely new path = a move's
        // destination). If another row carries the same volume-local `file_ref`
        // at a DIFFERENT path whose OLD location is now GONE from disk, re-bind
        // that row to this path — preserving its id and every FK-linked tag /
        // face (incl. manual person_id) / OCR / embedding — instead of orphaning
        // it and inserting a brand-new row that loses all of it. The old-path-gone
        // gate is the exact-duplicate guard: two COEXISTING hardlinks share an
        // inode but both paths still exist, so neither heals — they stay two rows.
        // After a heal the row lives at this path, so re-fetch `existing`: the
        // upsert below then takes its DO UPDATE branch and `fileID` resolves to
        // the preserved id. A pure move keeps size+mtime, so `unchanged` is true
        // and the carried-over children are left intact (no re-detect).
        if existing == nil, let ref = file.fileRef,
           try Self.healMovedRow(
               fileRef: ref, newPath: file.url.path, newPathHash: pathHash,
               newPathSearch: file.url.path.precomposedStringWithCanonicalMapping,
               db: db) != nil {
            existing = try Row.fetchOne(
                db.cachedStatement(sql: """
                    SELECT id, size_bytes, modified_at, failed FROM files WHERE path_text = ?
                    """),
                arguments: [file.url.path])
        }

        // A failure on a file that scanned before (transient decode error, NAS
        // hiccup) must not clobber the prior metadata or its child rows — incl.
        // manual person_id assignments. Record only the failure.
        if file.failed, let existing {
            let fileID: Int64 = existing["id"]
            try db.cachedStatement(sql: """
                UPDATE files SET failed = 1, error_message = ?, scanned_at = ? WHERE id = ?
                """).execute(arguments: [file.errorMessage, Date().timeIntervalSince1970, fileID])
            return
        }

        let newModified = file.modifiedAt?.timeIntervalSince1970
        // "Unchanged" only when this isn't a forced rescan, the prior scan
        // succeeded, this scan succeeded, and both size and mtime match. A
        // previously-failed file is always reprocessed.
        let unchanged: Bool = !forceReprocess && { () -> Bool in
            guard let existing else { return false }
            let oldFailed: Int64 = existing["failed"] ?? 0
            guard oldFailed == 0, !file.failed else { return false }
            let oldSize: Int64 = existing["size_bytes"] ?? -1
            guard oldSize == file.sizeBytes else { return false }
            let oldModified: Double? = existing["modified_at"]
            switch (oldModified, newModified) {
            case let (a?, b?): return abs(a - b) < 0.000_001
            case (nil, nil):   return true
            default:           return false
            }
        }()

        // 1. files row — id-preserving UPSERT (never delete+reinsert),
        // mirroring the Windows engine's INSERT_FILE_RETURNING_ID_SQL
        // (platforms/windows/src/engine/src/pipeline/dbwriter.rs)
        // column-for-column. The explicit `ON CONFLICT(path_text) DO UPDATE`
        // overrides the v1 column's `ON CONFLICT REPLACE` action, so the row
        // id and its FK-linked children survive a rescan.
        //   created_at + aesthetic are inserted but intentionally NOT in the
        //   DO UPDATE set (matches Windows): a rescan must not clobber the
        //   originally-recorded creation time, and aesthetic is scored elsewhere.
        //   file_ref binds the volume-local inode (st_ino) computed at discovery,
        //   stored bit-for-bit as the Windows `r as i64` (Int64(bitPattern:)) for
        //   cross-platform byte-parity; content_hash stays NULL (no BLAKE3 on the
        //   macOS scan path — a separate deferred decision). COALESCE preserves a
        //   previously-stored identity when the incoming value is NULL.
        try db.cachedStatement(sql: """
            INSERT INTO files
              (path_text, path_hash, path_search, size_bytes, created_at,
               modified_at, scanned_at, kind, extension, phash, aesthetic,
               has_faces, has_text, camera_model, location_lat, location_lon,
               failed, error_message, content_hash, file_ref)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(path_text) DO UPDATE SET
                path_hash     = excluded.path_hash,
                path_search   = excluded.path_search,
                size_bytes    = excluded.size_bytes,
                modified_at   = excluded.modified_at,
                scanned_at    = excluded.scanned_at,
                kind          = excluded.kind,
                extension     = excluded.extension,
                phash         = excluded.phash,
                has_faces     = excluded.has_faces,
                has_text      = excluded.has_text,
                camera_model  = excluded.camera_model,
                location_lat  = excluded.location_lat,
                location_lon  = excluded.location_lon,
                failed        = excluded.failed,
                error_message = excluded.error_message,
                content_hash  = COALESCE(excluded.content_hash, content_hash),
                file_ref      = COALESCE(excluded.file_ref, file_ref)
            """).execute(arguments: [
                file.url.path,
                pathHash,
                file.url.path.precomposedStringWithCanonicalMapping,
                Int(file.sizeBytes),
                file.createdAt?.timeIntervalSince1970,
                newModified,
                Date().timeIntervalSince1970,
                file.kind,
                file.extension,
                file.phash.map { Int(bitPattern: UInt(truncatingIfNeeded: $0)) },
                file.aestheticScore,
                file.hasFaces ? 1 : 0,
                (file.ocrText?.isEmpty == false) ? 1 : 0,
                file.cameraModel,
                file.locationLat,
                file.locationLon,
                file.failed ? 1 : 0,
                file.errorMessage,
                nil,
                file.fileRef.map { Int64(bitPattern: $0) }
            ])
        // last_insert_rowid() is NOT updated on the UPDATE branch of an upsert,
        // so resolve the id from the existing row when there was one. This
        // mirrors the row-id stability the Windows `RETURNING id` provides on
        // both branches, and is required so the child-row writes below (tags,
        // faces, OCR, CLIP) attach to the correct, surviving row on a rescan.
        let fileID: Int64 = (existing?["id"]) ?? db.lastInsertedRowID

        // Unchanged file: leave every child row exactly as-is. Re-detecting
        // would either duplicate rows or destroy manual person assignments for
        // a file that didn't change. One exception: a CLIP model installed
        // AFTER the original scan means this scan produced an embedding the
        // DB doesn't have — backfill it without rebuilding the other children.
        // For this branch to be REACHABLE on a normal incremental rescan, the
        // discovery skip set must NOT drop an embeddable image that still lacks
        // a clip_embeddings row — it keeps such files in the pipeline by AND-ing
        // `skipSetClipBackfillExclusionSQL` (below) into its WHERE. Without that
        // coordination F-C6-001's size+mtime skip filters these files out
        // upstream and this backfill path is dead code (re-audit R-14).
        if unchanged {
            if let blob = file.clipEmbeddingBlob {
                let hasEmbedding = try Bool.fetchOne(
                    db.cachedStatement(sql: """
                        SELECT EXISTS(SELECT 1 FROM clip_embeddings WHERE file_id = ?)
                        """),
                    arguments: [fileID]) ?? false
                if !hasEmbedding {
                    try insertClipEmbedding(fileID: fileID, blob: blob, db: db)
                }
            }
            return
        }

        // 2. tags — auto (classifier output). Gate the delete-then-reinsert on
        // whether the tagging stage actually ran AND returned this session
        // (tagsEvaluated). A Vision/ANE timeout emits an EMPTY visionTags; without
        // this gate the unconditional DELETE would wipe a file's previously-
        // persisted auto-tags on a transient timeout, with nothing re-inserted
        // (data loss). When the stage DID run, delete any prior `source='auto'`
        // rows and re-insert the fresh set atomically; user tags (`source='user'`)
        // are untouched either way. Mirrors the Windows `tags_evaluated` gate
        // (dbwriter.rs) — F-C3-001.
        if file.tagsEvaluated {
            try db.cachedStatement(sql: """
                DELETE FROM tags WHERE file_id = ? AND source = 'auto'
                """).execute(arguments: [fileID])
            for tag in file.visionTags {
                let trimmed = tag.trimmingCharacters(in: .whitespacesAndNewlines)
                if trimmed.isEmpty { continue }
                try db.cachedStatement(sql: """
                    INSERT OR REPLACE INTO tags (file_id, tag, source) VALUES (?, ?, ?)
                    """).execute(arguments: [fileID, trimmed, "auto"])
            }
        }

        // 3. OCR text. The ocr_fts external-content index is maintained by the
        // AFTER INSERT/DELETE/UPDATE sync triggers (fts_sync_triggers
        // migration), so we only touch ocr_text here. Gate the delete-then-insert
        // on whether the OCR stage actually ran this session (ocrStageRan): a
        // swallowed OCR timeout (or a primary-pass timeout that never reaches the
        // OCR branch) must NOT wipe valid prior OCR text + FTS postings. When the
        // stage DID run, explicit DELETE + INSERT — not INSERT OR REPLACE, whose
        // implicit delete fires the AFTER DELETE trigger only with recursive
        // triggers enabled, stranding the old text's FTS postings — clears any
        // now-empty text (covering a changed file that no longer yields OCR).
        // Mirrors the Windows `ocr_stage_ran` gate (dbwriter.rs) — F-C3-001.
        if file.ocrStageRan {
            try db.cachedStatement(sql: "DELETE FROM ocr_text WHERE file_id = ?")
                .execute(arguments: [fileID])
            if let text = file.ocrText, !text.isEmpty {
                try db.cachedStatement(sql: """
                    INSERT INTO ocr_text (file_id, text) VALUES (?, ?)
                    """).execute(arguments: [fileID, text])
            }
        }

        // 4. face_prints — write one row per detected face. ArcFace
        // embeddings + Vision feature prints are populated lazily during
        // Stage D (`extractPendingPrints`) so the per-file Vision pass
        // doesn't thrash ANE with N additional calls per detected face.
        // Quality + pose come from the inline detection pass and feed
        // the clustering quality filter (excluded=1 means "don't cluster
        // this face, but keep the row for display in PersonDetailSheet").
        // Gate the stale-face DELETE on whether the face stage actually ran AND
        // returned this session (facesEvaluated), NOT on `faceBBoxes.isEmpty`: an
        // edited/zero-face re-process must clear orphaned face_prints (else they
        // keep polluting clusters), while a Vision/ANE timeout (empty result)
        // must leave still-valid rows — and, critically, their manual person_id
        // assignments — intact. The files row survives a rescan (ON CONFLICT DO
        // UPDATE, no cascade delete), so without this gate a swallowed timeout
        // would silently wipe a named person's faces. Mirrors the Windows
        // `faces_evaluated` gate (dbwriter.rs) — F-C3-001.
        if file.facesEvaluated {
            try db.cachedStatement(sql: """
                DELETE FROM face_prints WHERE file_id = ?
                """).execute(arguments: [fileID])
            let bboxes = file.faceBBoxes
            if !bboxes.isEmpty {
                for i in 0..<bboxes.count {
                    let print: Data = i < file.facePrints.count ? file.facePrints[i] : Data()
                    let quality: Double? = i < file.faceQualities.count
                        ? (file.faceQualities[i] >= 0 ? file.faceQualities[i] : nil)
                        : nil
                    let yaw: Double? = i < file.faceYaws.count ? file.faceYaws[i] : nil
                    let pitch: Double? = i < file.facePitches.count ? file.facePitches[i] : nil
                    let bboxArea = Self.bboxArea(bboxes[i])
                    let excluded = Self.isExcluded(quality: quality, yaw: yaw,
                                                   pitch: pitch, bboxArea: bboxArea)
                    try db.cachedStatement(sql: """
                        INSERT INTO face_prints
                          (file_id, print_data, bbox, face_quality, excluded)
                        VALUES (?, ?, ?, ?, ?)
                        """).execute(arguments: [fileID, print, bboxes[i],
                                                 quality, excluded ? 1 : 0])
                }
            }
        }

        // 5. CLIP embedding (M3) — only present for images where the model
        // was loaded and inference succeeded.
        if let blob = file.clipEmbeddingBlob {
            try insertClipEmbedding(fileID: fileID, blob: blob, db: db)
        }
    }

    /// SQL boolean fragment (no leading `AND`, references the unaliased `files`
    /// table) the discovery incremental skip-set query must AND into its WHERE so
    /// an embeddable image that still LACKS a clip_embeddings row is NEVER added
    /// to the skip set. Keeping such files in the pipeline is what makes
    /// `insertOne`'s unchanged-file CLIP-backfill branch reachable after a CLIP
    /// model is installed post-scan — co-located with that branch so the two stay
    /// in sync. Keyed on `kind = 'image'` + NOT EXISTS, so ONLY images are forced,
    /// and only until they have an embedding (a backfilled image becomes skippable
    /// again on the next scan). Re-audit R-14; analogous in spirit to the Windows
    /// skip-set's content_hash carve-out (C1-013, scan_session.rs), which likewise
    /// keeps a file whose derived data is still missing IN the pipeline.
    static let skipSetClipBackfillExclusionSQL = """
        NOT (files.kind = 'image' AND NOT EXISTS (
            SELECT 1 FROM clip_embeddings WHERE clip_embeddings.file_id = files.id))
        """

    private static func insertClipEmbedding(fileID: Int64, blob: Data, db: GRDB.Database) throws {
        // `blob` is the worker's already-finalized Data; bind it straight to the
        // cached statement (no re-copy on the writer side). The remaining
        // tensor→loop→normalize→Data copies live in the embedding producer
        // (MobileCLIPService), which is outside this writer's scope. (F-C6-010)
        try db.cachedStatement(sql: """
            INSERT OR REPLACE INTO clip_embeddings (file_id, embedding, model)
            VALUES (?, ?, ?)
            """).execute(arguments: [fileID, blob, "mobileclip_s2"])
    }

    // MARK: - Rename/move heal

    /// Lookup + gate + re-bind for a moved file. Returns the healed row id, or
    /// nil if nothing healed. Port of the Windows HEAL_LOOKUP_SQL +
    /// heal_candidate_moved + HEAL_UPDATE_SQL (dbwriter.rs). file_ref-only on
    /// macOS — content_hash isn't computed by the scan path, and file_ref alone
    /// covers same-volume rename/move, the dominant case.
    private static func healMovedRow(
        fileRef: UInt64, newPath: String, newPathHash: Int64,
        newPathSearch: String, db: GRDB.Database
    ) throws -> Int64? {
        // Stored bit-for-bit as Windows binds it (`r as i64`) so the lookup keys
        // match across a cross-platform DB round-trip.
        let refInt = Int64(bitPattern: fileRef)
        // Candidate rows: same volume-local identity, DIFFERENT path. NULL
        // file_ref never matches (a row without identity can't be healed). More
        // than one match only on hardlinks; the gate below keeps coexisting links
        // distinct and re-binds only the genuinely-gone original.
        let candidates = try Row.fetchAll(
            db.cachedStatement(sql: """
                SELECT id, path_text FROM files
                WHERE path_text != ? AND file_ref IS NOT NULL AND file_ref = ?
                """),
            arguments: [newPath, refInt])
        for row in candidates {
            let oldPath: String = row["path_text"]
            // Heal ONLY a genuine move: the candidate's old path must be GONE.
            // A still-present old path means a coexisting copy/hardlink — skip, so
            // both stay distinct rows (the exact-duplicate guard). Mirrors the
            // Windows heal_candidate_moved old-path-gone gate exactly.
            if pathExistsOnDisk(oldPath) { continue }
            let id: Int64 = row["id"]
            do {
                try db.cachedStatement(sql: """
                    UPDATE files SET path_text = ?, path_hash = ?, path_search = ?
                    WHERE id = ?
                    """).execute(arguments: [newPath, newPathHash, newPathSearch, id])
                JSONLog.shared.info(ev: "rename_heal", path: redactPathForLog(newPath))
                return id
            } catch let e as DatabaseError
                where e.resultCode.primaryResultCode == .SQLITE_CONSTRAINT {
                // A content-identical row already occupies newPath (a copy scanned
                // there independently). Skip the heal rather than colliding on the
                // UNIQUE(path_text) constraint; the upsert below updates that
                // occupant and the moved-away orphan is left for orphan-pruning.
                // Mirrors the Windows ConstraintViolation skip (dbwriter.rs).
                return nil
            }
        }
        return nil
    }

    /// Old-path-gone probe for the rename-heal gate. `lstat` (not `stat`) so a
    /// dangling symlink still counts as PRESENT and is never treated as a move,
    /// mirroring the Windows `symlink_metadata` gate.
    private static func pathExistsOnDisk(_ path: String) -> Bool {
        var st = stat()
        return URL(fileURLWithPath: path).withUnsafeFileSystemRepresentation { rep in
            guard let rep else { return false }
            return lstat(rep, &st) == 0
        }
    }

    // MARK: - Face quality filter
    //
    // Decides whether a detected face participates in clustering. Excluded
    // faces still get a `face_prints` row + crop so the user can see them
    // attached to a person later (they're "candidates" — clustering just
    // doesn't anchor on them).

    /// Minimum face capture quality required to anchor a cluster. Apple
    /// Vision's `faceCaptureQuality` rates "is this portrait-studio-
    /// quality?" not "is this a recognizable face?" — real candid photos
    /// score well below 0.4 even when clearly recognizable. We trust
    /// ArcFace + Chinese Whispers' cosine threshold to handle weak
    /// embeddings; the only quality cases we filter are catastrophic
    /// (extremely blurry frames, occluded eyes). 0.02 is the bottom of
    /// the "still has a face" range; below this it's typically a false
    /// positive from the detector.
    static let qualityFloor: Double = 0.02

    /// Minimum bbox area (fraction of image) for clustering. Faces
    /// under 0.2% of the frame produce noisy ArcFace embeddings —
    /// crowd extras don't carry enough identity signal. ArcFace's
    /// 8-pixel crop minimum + 112×112 rescale handles small faces
    /// in high-res group photos.
    static let minBBoxAreaFraction: Double = 0.002

    /// |yaw| beyond this is "heavy profile" — same identity at frontal vs
    /// 60° profile lands far apart in embedding space, polluting clusters.
    static let maxYawRadians: Double = 50.0 * .pi / 180.0

    /// |pitch| beyond this is "looking up/down hard" — similarly noisy.
    static let maxPitchRadians: Double = 30.0 * .pi / 180.0

    /// Parse the "x,y,w,h" normalized bbox string and return w*h.
    static func bboxArea(_ s: String) -> Double {
        let parts = s.split(separator: ",").compactMap { Double($0) }
        guard parts.count == 4 else { return 0 }
        return parts[2] * parts[3]
    }

    /// Combined quality / size / pose filter. Returns true if the
    /// face should be excluded from clustering. nil quality means
    /// Vision couldn't measure it — usually low-confidence detection,
    /// so we exclude (admitting them lets noise into clusters).
    static func isExcluded(quality: Double?, yaw: Double?, pitch: Double?,
                           bboxArea: Double) -> Bool {
        guard let q = quality else { return true }
        if q < qualityFloor { return true }
        if bboxArea < minBBoxAreaFraction { return true }
        if let y = yaw, abs(y) > maxYawRadians { return true }
        if let p = pitch, abs(p) > maxPitchRadians { return true }
        return false
    }
}

// MARK: - ScanCoordinator additions for batched bumps

extension ScanCoordinator {
    /// Bump processed by N at once. Avoids N actor-hops per file in the DB
    /// writer's tight commit loop.
    public func bumpProcessed(by n: Int) {
        guard n > 0 else { return }
        for _ in 0..<n { bumpProcessed() }
    }

    public func bumpFailed(by n: Int) {
        guard n > 0 else { return }
        for _ in 0..<n { bumpFailed() }
    }
}
