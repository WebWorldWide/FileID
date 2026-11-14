import Foundation
import CoreGraphics
import GRDB
import FileIDShared
import FileIDImaging

/// Work a scan owes a file after its main pass: the RAM++ tags (deferred so the
/// scan runs at Neural Engine speed) and the careful text read (when Vision's
/// accurate tier was not ready). `scan_catchup` holds one row per owing file;
/// the row is the cursor, so a cancel or crash leaves the rest for the next scan.
enum ScanCatchUp {
    static let tagsOwed = 1
    static let carefulTextOwed = 2
    /// The fast text read ran during the scan.
    static let textReadAtScan = 4
    static let owedMask = tagsOwed | carefulTextOwed
    static let maxAttempts = 3

    struct Marker: Sendable, Equatable {
        var stages: Int
        var vision: String?
        var faceCount: Int
    }

    // MARK: - RAM++ during the scan

    enum RamPlusMode: Sendable, Equatable {
        /// RAM++ runs on every image inside the scan (the behaviour before catch-up).
        case inline
        /// The scan stores the classifier's tags and owes the RAM++ ones.
        case deferred

        /// `FILEID_RAMPLUS_INLINE=1` keeps RAM++ inside the scan.
        static func resolve(environment: [String: String], taggerReady: Bool) -> RamPlusMode {
            guard taggerReady else { return .inline }
            let raw = environment["FILEID_RAMPLUS_INLINE"]?.trimmingCharacters(in: .whitespaces).lowercased() ?? ""
            return ["1", "true", "on", "yes"].contains(raw) ? .inline : .deferred
        }
    }

    struct ScanOptions: Sendable {
        var mode: RamPlusMode
        var tag: @Sendable (CGImage) -> [(tag: String, score: Float)]
        var carefulRead: (@Sendable (CGImage) -> String?)?

        static var live: ScanOptions {
            ScanOptions(
                mode: .resolve(environment: ProcessInfo.processInfo.environment,
                               taggerReady: RamPlusService.shared.isReady),
                tag: { RamPlusService.shared.tag($0) },
                carefulRead: nil)
        }
    }

    // MARK: - What the classifier saw, kept for the later RAM++ pass

    /// Exactly the classifier output `TagAgreement.combine` and the text gate read:
    /// the stored tags, every label at 0.30 or above (the agreement score), the
    /// vetoable labels at 0.01 or above, and whether the classifier answered at
    /// all. Confidences are stored as bit patterns so they round-trip exactly.
    struct VisionContext: Codable, Sendable, Equatable {
        struct Label: Codable, Sendable, Equatable {
            var n: String
            var b: UInt32
        }
        var k: [String]
        var c: [Label]
        var u: Bool
        /// Classifier plus RAM++ tags, once known; the careful read needs them.
        var t: [String]?

        init(classifyTags: [String], confidences: [String: Float], textTags: [String]? = nil) {
            k = classifyTags
            c = confidences
                .filter { $0.value >= 0.30 || ($0.value >= TagAgreement.vetoBelow && Self.vetoKeys.contains($0.key)) }
                .map { Label(n: $0.key, b: $0.value.bitPattern) }
                .sorted { $0.n < $1.n }
            u = !confidences.isEmpty
            t = textTags
        }

        private static let vetoKeys = Set(TagAgreement.vetoable.map { $0.replacingOccurrences(of: " ", with: "_") })

        var confidences: [String: Float] {
            var out: [String: Float] = [:]
            for label in c { out[label.n] = Float(bitPattern: label.b) }
            // combine() treats an empty table as "the classifier did not answer".
            if u, out.isEmpty { out[""] = 0 }
            return out
        }

        var encoded: String? {
            (try? JSONEncoder().encode(self)).flatMap { String(data: $0, encoding: .utf8) }
        }

        static func decode(_ text: String?) -> VisionContext? {
            guard let data = text?.data(using: .utf8) else { return nil }
            return try? JSONDecoder().decode(VisionContext.self, from: data)
        }
    }

    // MARK: - Tag and text rules shared by the scan and the catch-up

    struct ImageTags: Sendable {
        var stored: [String]
        var scores: [String: Double]?
        var primary: [String]
        var textTags: [String]
    }

    static func imageTags(ram: [(tag: String, score: Float)], classifyTags: [String], vision: [String: Float],
                          faceCount: Int, cameraModel: String?, creationDate: Date?) -> ImageTags {
        let ramTags = TagAgreement.combine(ram: ram, vision: vision, faceCount: faceCount,
                                           peopleWords: RamPlusService.peopleWords)
        var scores: [String: Double]? = nil
        let primary: [String]
        if !ramTags.isEmpty {
            primary = ramTags.map { $0.tag }
            scores = Dictionary(ramTags.map { ($0.tag, Double($0.score)) }, uniquingKeysWith: { first, _ in first })
        } else {
            primary = classifyTags
        }
        let stored = Tagging.cappedTags(primary + Tagging.extraTags(cameraModel: cameraModel, creationDate: creationDate))
        return ImageTags(stored: stored, scores: scores, primary: primary, textTags: classifyTags + ramTags.map(\.tag))
    }

    static func textGate(classifyTags: [String], textTags: [String], documentShape: Bool) -> Bool {
        classifyTags.contains(where: { Tagging.docHints.contains($0.lowercased()) })
            || TextReading.isTextBearing(textTags) || documentShape
    }

    struct TextOutcome: Sendable {
        var text: String?
        var handwritten = false
        var fastMs: Double = 0
        var carefulMs: Double = 0
        /// A careful read was called for and the reader was not available.
        var carefulOwed = false
    }

    /// Fast read, then the careful one when `TextReading` asks. `careful` returns
    /// nil when the accurate tier is not ready or is paused by its breaker.
    static func readText(_ image: CGImage, textTags: [String], documentShape: Bool, hasCamera: Bool,
                         fast: (CGImage) -> String, careful: (CGImage) -> String?) -> TextOutcome {
        var outcome = TextOutcome()
        let fastStart = CFAbsoluteTimeGetCurrent()
        let fastText = fast(image)
        outcome.fastMs = (CFAbsoluteTimeGetCurrent() - fastStart) * 1000
        var text = fastText
        if TextReading.needsCarefulRead(fastText: fastText, tags: textTags, textBearing: documentShape) {
            let carefulStart = CFAbsoluteTimeGetCurrent()
            let read = careful(image)
            outcome.carefulMs = (CFAbsoluteTimeGetCurrent() - carefulStart) * 1000
            outcome.carefulOwed = read == nil
            let carefulText = read ?? ""
            outcome.handwritten = TextReading.isHandwritten(
                fastText: fastText, carefulText: carefulText, tags: textTags,
                paperLike: TextReading.isPaperLike(tags: textTags, hasCamera: hasCamera))
            text = TextReading.preferred(fast: fastText, careful: carefulText)
        }
        outcome.text = text.isEmpty ? nil : text
        if outcome.text == nil { outcome.handwritten = false }
        return outcome
    }

    /// The text decision reads tags only through these three questions.
    private static func textSignals(_ tags: [String]) -> [Bool] {
        [TextReading.hasHandwritingHint(tags), TextReading.isTextBearing(tags),
         TextReading.isPaperLike(tags: tags, hasCamera: true)]
    }

    // MARK: - Catch-up pass

    struct Readers: Sendable {
        var taggerReady: @Sendable () -> Bool
        var tag: @Sendable (CGImage) -> [(tag: String, score: Float)]
        var fastRead: @Sendable (CGImage) -> String
        /// nil when the careful reader is not available right now.
        var carefulRead: @Sendable (CGImage) -> String?
        var carefulReady: @Sendable () -> Bool
        var carefulWaitSeconds: Double

        static var live: Readers {
            let worker = VisionWorker()
            return Readers(
                taggerReady: { RamPlusService.shared.load() },
                tag: { RamPlusService.shared.tag($0) },
                fastRead: { worker.ocrFast($0) },
                carefulRead: { worker.ocrAccurateIfAvailable($0) },
                carefulReady: { AccurateOCRWarmup.shared.isReady && AccurateOCRBreaker.shared.allowed },
                carefulWaitSeconds: AccurateOCRWarmup.isEnabled ? 180 : 0)
        }
    }

    enum Progress: Sendable {
        case total(Int)
        case advance(Int)
    }

    struct Summary: Sendable, Equatable {
        var owedTags = 0
        var owedCareful = 0
        var tagged = 0
        var textRead = 0
        var carefulRead = 0
        var left = 0
        var failed = 0
        var cancelled = false
    }

    struct Counts: Sendable, Equatable {
        var tags: Int
        var careful: Int
        var any: Bool { tags + careful > 0 }
    }

    private static func scope(_ root: String?) -> (sql: String, arguments: [String]) {
        guard let root, !root.isEmpty else { return ("", []) }
        let prefix = root.hasSuffix("/") ? root : root + "/"
        var upper = prefix
        upper.removeLast()
        upper += "0"
        return (" AND (f.path_text = ? OR (f.path_text >= ? AND f.path_text < ?))", [root, prefix, upper])
    }

    static func owed(database: Database, root: String?) async -> Counts {
        let scope = scope(root)
        return (try? await database.pool.read { db in
            let row = try Row.fetchOne(db, sql: """
                SELECT COALESCE(SUM((c.stages & 1) != 0), 0) AS tags, COALESCE(SUM((c.stages & 2) != 0), 0) AS careful
                FROM scan_catchup c JOIN files f ON f.id = c.file_id WHERE 1 = 1\(scope.sql)
                """, arguments: StatementArguments(scope.arguments))
            return Counts(tags: row?["tags"] ?? 0, careful: row?["careful"] ?? 0)
        }) ?? Counts(tags: 0, careful: 0)
    }

    private struct Pending: Sendable {
        var fileID: Int64
        var path: String
        var kind: String
        var sizeBytes: Int64
        var modifiedAt: Double?
        var contentHash: Data?
        var stages: Int
        var markSize: Int64
        var markModified: Double?
        var vision: String?
        var faceCount: Int
    }

    private struct Update: Sendable {
        var tags: ImageTags?
        var text: TextOutcome?
        var stages: Int
        var vision: String?
    }

    private enum Outcome: Sendable {
        /// Changed, missing or unreachable: the next scan deals with it.
        case leave
        /// The row no longer describes the analysis the marker was written for.
        case stale
        case failed
        /// The careful reader went away; stop and keep what is owed.
        case blocked
        case done(Update)
    }

    private enum Phase: Sendable {
        case tags, careful
        var bit: Int { self == .tags ? ScanCatchUp.tagsOwed : ScanCatchUp.carefulTextOwed }
    }

    /// RAM++ output for byte-identical files, so a set of copies is tagged once.
    private final class TagCache: @unchecked Sendable {
        private let lock = NSLock()
        private var map: [TwinKey: [(tag: String, score: Float)]] = [:]

        func tags(for file: Pending, compute: () -> [(tag: String, score: Float)]) -> [(tag: String, score: Float)] {
            guard let hash = file.contentHash, TwinReuse.enabled, TwinReuse.eligible(sizeBytes: file.sizeBytes) else {
                return compute()
            }
            let key = TwinKey(hash: hash, size: file.sizeBytes)
            lock.lock()
            let known = map[key]
            lock.unlock()
            if let known { return known }
            let fresh = compute()
            guard !fresh.isEmpty else { return fresh }
            lock.lock()
            if map.count >= 4096 { map.removeAll(keepingCapacity: true) }
            map[key] = fresh
            lock.unlock()
            return fresh
        }
    }

    static let chunkSize = max(4, min(Hardware.workerCap, 16))

    /// Runs the tag pass, then the careful-read pass, over files under `root`
    /// (everything when nil). Safe to call with nothing owed.
    @discardableResult
    static func run(
        database: Database, root: String?, readers: Readers = .live,
        isCancelled: @escaping @Sendable () -> Bool = { false },
        isPaused: @escaping @Sendable () -> Bool = { false },
        useAdmission: Bool = true,
        progress: @escaping @Sendable (Progress) async -> Void = { _ in }
    ) async -> Summary {
        var summary = Summary()
        let before = await owed(database: database, root: root)
        guard before.any else { return summary }
        summary.owedTags = before.tags
        summary.owedCareful = before.careful
        let started = CFAbsoluteTimeGetCurrent()
        JSONLog.shared.info(ev: "catchup_started", extra: [
            "tags": AnyCodable(before.tags), "carefulText": AnyCodable(before.careful)])
        await progress(.total(before.tags + before.careful))

        var visited = 0
        if before.tags > 0 {
            if readers.taggerReady() {
                visited += await pass(.tags, database: database, root: root, readers: readers,
                                      isCancelled: isCancelled, isPaused: isPaused, useAdmission: useAdmission,
                                      summary: &summary, progress: progress)
            } else {
                JSONLog.shared.info(ev: "catchup_tags_skipped", extra: ["reason": AnyCodable("tagger not available")])
            }
        }

        let careful = await owed(database: database, root: root).careful
        if careful > 0, !summary.cancelled, !isCancelled() {
            await progress(.total(visited + careful))
            let deadline = CFAbsoluteTimeGetCurrent() + readers.carefulWaitSeconds
            while !readers.carefulReady(), CFAbsoluteTimeGetCurrent() < deadline, !isCancelled() {
                try? await Task.sleep(for: .milliseconds(250))
            }
            if readers.carefulReady() {
                _ = await pass(.careful, database: database, root: root, readers: readers,
                               isCancelled: isCancelled, isPaused: isPaused, useAdmission: useAdmission,
                               summary: &summary, progress: progress)
            } else if !isCancelled() {
                JSONLog.shared.info(ev: "catchup_careful_text_skipped", extra: [
                    "reason": AnyCodable("careful reader not ready"), "owed": AnyCodable(careful)])
            }
        }
        if isCancelled() { summary.cancelled = true }
        let after = await owed(database: database, root: root)
        JSONLog.shared.info(ev: "catchup_finished", extra: [
            "tagged": AnyCodable(summary.tagged), "textRead": AnyCodable(summary.textRead),
            "carefulRead": AnyCodable(summary.carefulRead), "left": AnyCodable(summary.left),
            "failed": AnyCodable(summary.failed), "cancelled": AnyCodable(summary.cancelled),
            "stillOwedTags": AnyCodable(after.tags), "stillOwedCarefulText": AnyCodable(after.careful),
            "ms": AnyCodable((CFAbsoluteTimeGetCurrent() - started) * 1000)])
        return summary
    }

    private static func pass(
        _ phase: Phase, database: Database, root: String?, readers: Readers,
        isCancelled: @escaping @Sendable () -> Bool, isPaused: @escaping @Sendable () -> Bool,
        useAdmission: Bool, summary: inout Summary,
        progress: @escaping @Sendable (Progress) async -> Void
    ) async -> Int {
        let scope = scope(root)
        let cache = TagCache()
        let began = CFAbsoluteTimeGetCurrent()
        var cursor: Int64 = 0
        var visited = 0
        var lastLogged = 0
        var blocked = false
        while !blocked {
            while isPaused(), !isCancelled() { try? await Task.sleep(for: .milliseconds(200)) }
            if isCancelled() || Task.isCancelled { summary.cancelled = true; break }
            let after = cursor
            let batch: [Pending] = (try? await database.pool.read { db in
                try Row.fetchAll(db, sql: """
                    SELECT c.file_id, c.stages, c.size_bytes AS mark_size, c.modified_at AS mark_modified, c.vision,
                           c.face_count, f.path_text, f.kind, f.size_bytes, f.modified_at, f.content_hash
                    FROM scan_catchup c JOIN files f ON f.id = c.file_id
                    WHERE c.file_id > ? AND (c.stages & \(phase.bit)) != 0 AND f.failed = 0\(scope.sql)
                    ORDER BY c.file_id LIMIT \(chunkSize)
                    """, arguments: StatementArguments([after]) + StatementArguments(scope.arguments)).map { row in
                    Pending(fileID: row["file_id"], path: row["path_text"], kind: row["kind"],
                            sizeBytes: row["size_bytes"] ?? 0, modifiedAt: row["modified_at"],
                            contentHash: row["content_hash"], stages: row["stages"],
                            markSize: row["mark_size"] ?? 0, markModified: row["mark_modified"],
                            vision: row["vision"], faceCount: row["face_count"] ?? 0)
                }
            }) ?? []
            guard let last = batch.last else { break }
            cursor = last.fileID

            let outcomes = await withTaskGroup(of: (Pending, Outcome).self) { group -> [(Pending, Outcome)] in
                for file in batch {
                    group.addTask {
                        let work: @Sendable () async -> Outcome = {
                            await withCheckedContinuation { continuation in
                                Tagging.visionQueue.async {
                                    let outcome = autoreleasepool {
                                        isCancelled() ? Outcome.leave
                                            : process(file, phase: phase, readers: readers, cache: cache)
                                    }
                                    continuation.resume(returning: outcome)
                                }
                            }
                        }
                        guard useAdmission else { return (file, await work()) }
                        let admitted = try? await ScanTaggingAdmission.withBackgroundCPU(isCancelled: isCancelled) { _ in
                            await work()
                        }
                        return (file, admitted ?? .leave)
                    }
                }
                var out: [(Pending, Outcome)] = []
                for await result in group { out.append(result) }
                return out
            }

            let cancelledMidway = isCancelled()
            do {
                let applied = try await database.writeUncancellable { db in
                    var counts = Summary()
                    for (file, outcome) in outcomes {
                        try apply(outcome, to: file, phase: phase, counts: &counts, db: db)
                    }
                    return counts
                }
                summary.tagged += applied.tagged
                summary.textRead += applied.textRead
                summary.carefulRead += applied.carefulRead
                summary.left += applied.left
                summary.failed += applied.failed
            } catch {
                JSONLog.shared.warn(ev: "catchup_write_failed", error: "\(error)")
                break
            }
            if outcomes.contains(where: { if case .blocked = $0.1 { return true } else { return false } }) {
                blocked = true
                JSONLog.shared.info(ev: "catchup_careful_text_paused", extra: ["reason": AnyCodable("careful reader stopped answering")])
            }
            if cancelledMidway { summary.cancelled = true; break }
            visited += batch.count
            await progress(.advance(batch.count))
            if visited - lastLogged >= 500 {
                lastLogged = visited
                let seconds = CFAbsoluteTimeGetCurrent() - began
                JSONLog.shared.info(ev: "catchup_progress", extra: [
                    "phase": AnyCodable(phase == .tags ? "tags" : "carefulText"),
                    "files": AnyCodable(visited),
                    "filesPerSec": AnyCodable(seconds > 0 ? Double(visited) / seconds : 0),
                    "residentMB": AnyCodable(Hardware.residentMB())])
            }
        }
        let seconds = CFAbsoluteTimeGetCurrent() - began
        JSONLog.shared.info(ev: "catchup_pass_finished", extra: [
            "phase": AnyCodable(phase == .tags ? "tags" : "carefulText"),
            "files": AnyCodable(visited), "ms": AnyCodable(seconds * 1000),
            "filesPerSec": AnyCodable(seconds > 0 ? Double(visited) / seconds : 0)])
        return visited
    }

    private static func sameTime(_ a: Double?, _ b: Double?) -> Bool {
        switch (a, b) {
        case let (a?, b?): return abs(a - b) < 0.000_001
        case (nil, nil): return true
        default: return false
        }
    }

    private static func process(_ file: Pending, phase: Phase, readers: Readers, cache: TagCache) -> Outcome {
        guard file.markSize == file.sizeBytes, sameTime(file.markModified, file.modifiedAt) else { return .stale }
        let url = URL(fileURLWithPath: file.path)
        guard let values = try? url.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey, .creationDateKey]),
              Int64(values.fileSize ?? 0) == file.sizeBytes,
              sameTime(values.contentModificationDate?.timeIntervalSince1970, file.modifiedAt) else { return .leave }

        if file.kind == "pdf" {
            guard phase == .careful else { return .stale }
            guard let pdf = CGPDFDocument(url as CFURL) else { return .failed }
            let read = Tagging.readScannedPDF(pdf, fast: readers.fastRead, careful: readers.carefulRead)
            if read.carefulOwed { return .blocked }
            return .done(Update(tags: nil, text: read, stages: 0, vision: file.vision))
        }
        guard file.kind == "image" else { return .stale }
        guard let (image, exif) = Tagging.loadImageAndEXIF(url: url, sizeBytes: file.sizeBytes) else { return .failed }
        let hasCamera = exif.cameraModel != nil
        let documentShape = TextReading.isDocumentShape(width: image.width, height: image.height, hasCamera: hasCamera)
        guard var context = VisionContext.decode(file.vision) else { return .stale }

        switch phase {
        case .tags:
            let tags = imageTags(
                ram: cache.tags(for: file) { readers.tag(image) },
                classifyTags: context.k, vision: context.confidences, faceCount: file.faceCount,
                cameraModel: exif.cameraModel, creationDate: exif.capturedAt ?? values.creationDate)
            var stages = file.stages & ~tagsOwed
            var text: TextOutcome? = nil
            if textGate(classifyTags: context.k, textTags: tags.textTags, documentShape: documentShape) {
                let readAtScan = file.stages & textReadAtScan != 0
                if !readAtScan || file.stages & carefulTextOwed != 0
                    || textSignals(context.k) != textSignals(tags.textTags) {
                    let read = readText(image, textTags: tags.textTags, documentShape: documentShape,
                                        hasCamera: hasCamera, fast: readers.fastRead, careful: readers.carefulRead)
                    text = read
                    stages = read.carefulOwed ? stages | carefulTextOwed : stages & ~carefulTextOwed
                    stages |= textReadAtScan
                }
            }
            context.t = tags.textTags
            return .done(Update(tags: tags, text: text, stages: stages, vision: context.encoded))
        case .careful:
            let textTags = context.t ?? context.k
            let read = readText(image, textTags: textTags, documentShape: documentShape, hasCamera: hasCamera,
                                fast: readers.fastRead, careful: readers.carefulRead)
            if read.carefulOwed { return .blocked }
            return .done(Update(tags: nil, text: read, stages: file.stages & ~carefulTextOwed, vision: file.vision))
        }
    }

    private static func apply(_ outcome: Outcome, to file: Pending, phase: Phase,
                              counts: inout Summary, db: GRDB.Database) throws {
        let current = try Row.fetchOne(db.cachedStatement(sql: """
            SELECT c.stages, f.size_bytes, f.modified_at FROM scan_catchup c JOIN files f ON f.id = c.file_id
            WHERE c.file_id = ?
            """), arguments: [file.fileID])
        guard let current, (current["stages"] as Int?) == file.stages,
              (current["size_bytes"] as Int64?) == file.sizeBytes,
              sameTime(current["modified_at"], file.modifiedAt) else { return }
        switch outcome {
        case .leave, .blocked:
            counts.left += 1
        case .stale:
            try db.cachedStatement(sql: "DELETE FROM scan_catchup WHERE file_id = ?").execute(arguments: [file.fileID])
            counts.left += 1
        case .failed:
            try db.cachedStatement(sql: "UPDATE scan_catchup SET attempts = attempts + 1 WHERE file_id = ?")
                .execute(arguments: [file.fileID])
            try db.cachedStatement(sql: "DELETE FROM scan_catchup WHERE file_id = ? AND attempts >= \(maxAttempts)")
                .execute(arguments: [file.fileID])
            counts.failed += 1
        case .done(let update):
            if let tags = update.tags {
                try db.cachedStatement(sql: "DELETE FROM tags WHERE file_id = ? AND source = 'auto'")
                    .execute(arguments: [file.fileID])
                for tag in tags.stored {
                    let trimmed = tag.trimmingCharacters(in: .whitespacesAndNewlines)
                    if trimmed.isEmpty { continue }
                    try db.cachedStatement(sql: """
                        INSERT OR REPLACE INTO tags (file_id, tag, source, score) VALUES (?, ?, 'auto', ?)
                        """).execute(arguments: [file.fileID, trimmed, tags.scores?[trimmed]])
                }
                counts.tagged += 1
            }
            if let text = update.text {
                try db.cachedStatement(sql: "DELETE FROM ocr_text WHERE file_id = ? AND source = 'ocr'")
                    .execute(arguments: [file.fileID])
                if let value = text.text, !value.isEmpty {
                    try db.cachedStatement(sql: """
                        INSERT INTO ocr_text (file_id, text, handwritten)
                        SELECT ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM ocr_text WHERE file_id = ?)
                        """).execute(arguments: [file.fileID, value, text.handwritten ? 1 : 0, file.fileID])
                }
                try db.cachedStatement(sql: """
                    UPDATE files SET has_text = EXISTS(SELECT 1 FROM ocr_text WHERE file_id = ?) WHERE id = ?
                    """).execute(arguments: [file.fileID, file.fileID])
                if phase == .careful { counts.carefulRead += 1 } else { counts.textRead += 1 }
            }
            if update.stages & owedMask == 0 {
                try db.cachedStatement(sql: "DELETE FROM scan_catchup WHERE file_id = ?").execute(arguments: [file.fileID])
            } else {
                try db.cachedStatement(sql: "UPDATE scan_catchup SET stages = ?, vision = ?, attempts = 0 WHERE file_id = ?")
                    .execute(arguments: [update.stages, update.vision, file.fileID])
            }
        }
    }
}
