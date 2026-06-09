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

    public init(
        url: URL, kind: String, extension ext: String, sizeBytes: Int64,
        createdAt: Date?, modifiedAt: Date?,
        visionTags: [String] = [], phash: UInt64? = nil,
        aestheticScore: Double? = nil, hasFaces: Bool = false,
        facePrints: [Data] = [], faceBBoxes: [String] = [],
        faceQualities: [Double] = [],
        faceYaws: [Double?] = [], facePitches: [Double?] = [],
        ocrText: String? = nil, cameraModel: String? = nil,
        locationLat: Double? = nil, locationLon: Double? = nil,
        failed: Bool = false, errorMessage: String? = nil,
        perFileTotalMs: Double = 0,
        clipEmbeddingBlob: Data? = nil
    ) {
        self.url = url
        self.kind = kind
        self.extension = ext
        self.sizeBytes = sizeBytes
        self.createdAt = createdAt
        self.modifiedAt = modifiedAt
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

    /// Drain `taggedChannel` until the channel closes (= tagging stage done).
    /// Called from the engine's main task; the writer keeps going until the
    /// upstream is exhausted, then performs a final flush.
    public func drain(_ channel: AsyncChannel<TaggedFile>) async {
        var buffer: [TaggedFile] = []
        buffer.reserveCapacity(maxBatchFiles)
        var batchStart = Date()

        for await file in channel {
            buffer.append(file)
            // Trigger commit on either ceiling.
            let elapsedMs = Date().timeIntervalSince(batchStart) * 1000
            if buffer.count >= maxBatchFiles || elapsedMs >= Double(maxBatchMs) {
                await commit(&buffer, batchStart: batchStart)
                batchStart = Date()
            }
        }
        // Tail flush — anything still buffered when upstream closes.
        if !buffer.isEmpty {
            await commit(&buffer, batchStart: batchStart)
        }
    }

    // MARK: - Commit

    private func commit(_ buffer: inout [TaggedFile], batchStart: Date) async {
        guard !buffer.isEmpty else { return }
        let batchFiles = buffer
        buffer.removeAll(keepingCapacity: true)

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
                        try db.execute(sql: """
                            UPDATE scan_sessions SET last_file_index = ? WHERE id = ?
                            """, arguments: [resumeCursor, sessionID])
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
            await sink.emit(.error(EngineError(
                kind: "db_write_failed",
                message: userMessage
            )))
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
    }

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
        let pathHash = Int(bitPattern: UInt(truncatingIfNeeded: file.url.path.hashValue))

        // Look up any existing row for this path. We need its id (to preserve
        // it) and its size/mtime/failed state (to decide whether the file
        // actually changed). The old `INSERT OR REPLACE` deleted+reinserted the
        // row on every re-scan, which — via ON DELETE CASCADE — wiped
        // face_prints (including the user's MANUAL person assignments), CLIP
        // embeddings, and FTS rows, and minted a brand-new rowid each time.
        let existing = try Row.fetchOne(db, sql: """
            SELECT id, size_bytes, modified_at, failed FROM files WHERE path_text = ?
            """, arguments: [file.url.path])

        // A failure on a file that scanned before (transient decode error, NAS
        // hiccup) must not clobber the prior metadata or its child rows — incl.
        // manual person_id assignments. Record only the failure.
        if file.failed, let existing {
            let fileID: Int64 = existing["id"]
            try db.execute(sql: """
                UPDATE files SET failed = 1, error_message = ?, scanned_at = ? WHERE id = ?
                """, arguments: [file.errorMessage, Date().timeIntervalSince1970, fileID])
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
        //   content_hash/file_ref aren't computed by the macOS scan path yet, so
        //   they bind NULL and COALESCE preserves any previously-stored value.
        try db.execute(sql: """
            INSERT INTO files
              (path_text, path_hash, size_bytes, created_at, modified_at,
               scanned_at, kind, extension, phash, aesthetic, has_faces,
               has_text, camera_model, location_lat, location_lon,
               failed, error_message, content_hash, file_ref)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(path_text) DO UPDATE SET
                path_hash     = excluded.path_hash,
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
            """, arguments: [
                file.url.path,
                pathHash,
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
                nil
            ]
        )
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
        if unchanged {
            if let blob = file.clipEmbeddingBlob {
                let hasEmbedding = try Bool.fetchOne(db, sql: """
                    SELECT EXISTS(SELECT 1 FROM clip_embeddings WHERE file_id = ?)
                    """, arguments: [fileID]) ?? false
                if !hasEmbedding {
                    try insertClipEmbedding(fileID: fileID, blob: blob, db: db)
                }
            }
            return
        }

        // 2. tags — auto (classifier output). Delete any prior `source='auto'`
        // rows first so a rescan replaces stale tags atomically; user tags
        // (`source='user'`) are untouched. Mirrors the Windows `tag_delete` +
        // `INSERT OR REPLACE` in dbwriter.rs.
        try db.execute(sql: """
            DELETE FROM tags WHERE file_id = ? AND source = 'auto'
            """, arguments: [fileID])
        for tag in file.visionTags {
            let trimmed = tag.trimmingCharacters(in: .whitespacesAndNewlines)
            if trimmed.isEmpty { continue }
            try db.execute(sql: """
                INSERT OR REPLACE INTO tags (file_id, tag, source) VALUES (?, ?, ?)
                """, arguments: [fileID, trimmed, "auto"])
        }

        // 3. OCR text. The ocr_fts external-content index is maintained by the
        // AFTER INSERT/DELETE/UPDATE sync triggers (fts_sync_triggers
        // migration), so we only touch ocr_text here. Explicit DELETE + INSERT
        // rather than INSERT OR REPLACE: REPLACE's implicit delete fires the
        // AFTER DELETE trigger only when recursive triggers are enabled, which
        // would strand the old text's FTS postings. The bare DELETE also
        // covers a changed file that no longer yields OCR text.
        try db.execute(sql: "DELETE FROM ocr_text WHERE file_id = ?", arguments: [fileID])
        if let text = file.ocrText, !text.isEmpty {
            try db.execute(sql: """
                INSERT INTO ocr_text (file_id, text) VALUES (?, ?)
                """, arguments: [fileID, text])
        }

        // 4. face_prints — write one row per detected face. ArcFace
        // embeddings + Vision feature prints are populated lazily during
        // Stage D (`extractPendingPrints`) so the per-file Vision pass
        // doesn't thrash ANE with N additional calls per detected face.
        // Quality + pose come from the inline detection pass and feed
        // the clustering quality filter (excluded=1 means "don't cluster
        // this face, but keep the row for display in PersonDetailSheet").
        // The files row survives a rescan (ON CONFLICT DO UPDATE, no cascade
        // delete), so clear this file's prior face rows even when this scan
        // found none — otherwise a rescan accumulates duplicate or stale
        // faces. Mirrors the Windows `face_delete` that precedes its inserts.
        try db.execute(sql: """
            DELETE FROM face_prints WHERE file_id = ?
            """, arguments: [fileID])
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
                try db.execute(sql: """
                    INSERT INTO face_prints
                      (file_id, print_data, bbox, face_quality, excluded)
                    VALUES (?, ?, ?, ?, ?)
                    """, arguments: [fileID, print, bboxes[i],
                                     quality, excluded ? 1 : 0])
            }
        }

        // 5. CLIP embedding (M3) — only present for images where the model
        // was loaded and inference succeeded.
        if let blob = file.clipEmbeddingBlob {
            try insertClipEmbedding(fileID: fileID, blob: blob, db: db)
        }
    }

    private static func insertClipEmbedding(fileID: Int64, blob: Data, db: GRDB.Database) throws {
        try db.execute(sql: """
            INSERT OR REPLACE INTO clip_embeddings (file_id, embedding, model)
            VALUES (?, ?, ?)
            """, arguments: [fileID, blob, "mobileclip_s2"])
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
