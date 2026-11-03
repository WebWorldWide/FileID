import CoreImage
import CryptoKit
import Foundation
import GRDB
import FileIDShared

/// Zero-setup Best Takes: suggested groups of tries straight from the scan
/// (capture time + CLIP + pHash via NearDuplicateGrouping), model-free scores
/// cached per file, Keep all / Skip choices, and an optional one-pass goal
/// ranking with the loaded description model. Mirrored by the Rust engine's
/// commands/catalog_take_auto.rs; keep thresholds and reasons in step.
enum CatalogTakeAuto {
    static let maxScoredFiles = 800
    static let goalTiles = 6

    static func handle(_ request: CatalogRequest, database: Database,
                       faces: TakeFaceAnalyzing = VisionTakeFaceAnalyzer()) async throws -> CatalogResponse {
        switch request.action {
        case "autoTakeGroups":
            let limit = request.limit ?? 100
            guard (1...100).contains(limit) else { throw CatalogStore.InvalidRequest() }
            let groups = try await database.pool.read { db in try discover(db, limit: limit) }
            return CatalogResponse(requestID: request.requestID, status: "ok", takeGroups: groups)
        case "scoreTakes":
            guard let ids = request.fileIDs, (1...maxScoredFiles).contains(ids.count), ids.allSatisfy({ $0 > 0 }) else {
                throw CatalogStore.InvalidRequest()
            }
            let qualities = try await score(fileIDs: ids, database: database, faces: faces)
            return CatalogResponse(requestID: request.requestID, status: "ok", takeQualities: qualities)
        case "decideTakeGroup":
            guard let decision = request.takeDecision, !decision.groupKey.isEmpty, decision.groupKey.count <= 200,
                  (1...maxScoredFiles).contains(decision.fileIDs.count), decision.fileIDs.allSatisfy({ $0 > 0 }),
                  ["keepAll", "skip", "reopen"].contains(decision.decision) else { throw CatalogStore.InvalidRequest() }
            try await database.pool.write { db in try decide(db, decision) }
            return CatalogResponse(requestID: request.requestID, status: "ok")
        case "rankTakesWithGoal":
            let goal = (request.query ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
            guard !goal.isEmpty, goal.count <= 500, let ids = request.fileIDs, (2...100).contains(ids.count) else {
                throw CatalogStore.InvalidRequest()
            }
            let ranking = try await rankWithGoal(goal: goal, primaryIDs: ids, database: database)
            return CatalogResponse(requestID: request.requestID, status: "ok", takeGoalRanking: ranking)
        default:
            throw CatalogStore.InvalidRequest()
        }
    }

    // MARK: - Suggested groups

    private struct Row1 { let id: Int64; let path: String; let time: Double; let phash: UInt64?; let kind: String }

    static func discover(_ db: GRDB.Database, limit: Int) throws -> [CatalogTakeAutoGroup] {
        let rows = try Row.fetchAll(db, sql: """
            SELECT f.id, f.path_text, f.created_at, f.phash, f.kind FROM files f
            WHERE f.kind IN ('image','video') AND f.failed=0 AND f.created_at IS NOT NULL
              AND NOT EXISTS (SELECT 1 FROM catalog_assets a WHERE a.derived_id=f.id)
              AND NOT EXISTS (SELECT 1 FROM catalog_event_files ef WHERE ef.file_id=f.id)
            ORDER BY f.created_at, f.id
            """).map { row -> Row1 in
                let phash: Int64? = row["phash"]
                return Row1(id: row["id"], path: row["path_text"], time: row["created_at"], phash: phash.map { UInt64(bitPattern: $0) }, kind: row["kind"])
            }
        var groups: [CatalogTakeAutoGroup] = []
        for (kind, thresholds) in [("image", NearDuplicateGrouping.Thresholds.takes), ("video", .clips)] {
            let subset = rows.filter { $0.kind == kind && $0.time.isFinite }
            let candidates = candidateIDs(subset, maxGap: thresholds.maxGapSeconds)
            let embeddings = try loadEmbeddings(db, ids: Array(candidates))
            let items = subset.map { NearDuplicateGrouping.Item(id: $0.id, path: $0.path, time: $0.time, phash: $0.phash, embedding: embeddings[$0.id]) }
            let aligned = Dictionary(uniqueKeysWithValues: NearDuplicateGrouping.alignSidecarTimes(items).map { ($0.id, $0) })
            for group in NearDuplicateGrouping.groups(items, thresholds: thresholds) {
                let members = group.members.compactMap { aligned[$0] }
                let shots = self.shots(members)
                guard shots.count >= 2 else { continue }
                let primaries = shots.compactMap { shot in aligned[shot.primaryFileID]?.embedding }
                var similarity: Float = 1
                for (a, b) in zip(primaries, primaries.dropFirst()) { similarity = min(similarity, NearDuplicateGrouping.cosine(a, b)) }
                let ids = members.map(\.id).sorted()
                groups.append(CatalogTakeAutoGroup(
                    groupKey: key(ids), kind: kind == "image" ? "photos" : "videos", status: "open",
                    capturedAt: members.compactMap(\.time).max() ?? 0,
                    similarity: TakeScoring.round4(Double(max(0, min(1, similarity)))), shots: Array(shots.prefix(100))))
            }
        }
        let decisions = try Row.fetchAll(db, sql: "SELECT decision,file_ids_json FROM take_group_decisions").compactMap { row -> (String, Set<Int64>)? in
            let json: String = row["file_ids_json"]
            guard let ids = try? JSONDecoder().decode([Int64].self, from: Data(json.utf8)) else { return nil }
            return (row["decision"], Set(ids))
        }
        var visible: [CatalogTakeAutoGroup] = []
        for var group in groups {
            let ids = Set(group.fileIDs)
            if decisions.contains(where: { $0.0 == "keepAll" && ids.isSubset(of: $0.1) }) { continue }
            if decisions.contains(where: { $0.0 == "skip" && ids.isSubset(of: $0.1) }) { group.status = "skipped" }
            visible.append(group)
        }
        let newest = visible.map(\.capturedAt).max() ?? 0
        return Array(rank(visible, newest: newest).prefix(limit))
    }

    /// Bigger and more recent groups first: 0.6 × size (saturating at 12 tries)
    /// + 0.4 × recency (half-life 14 days before the newest group). Skipped last.
    static func rank(_ groups: [CatalogTakeAutoGroup], newest: Double) -> [CatalogTakeAutoGroup] {
        func value(_ g: CatalogTakeAutoGroup) -> Double {
            let size = Double(min(g.shots.count, 12)) / 12
            let ageDays = max(0, newest - g.capturedAt) / 86_400
            return 0.6 * size + 0.4 * pow(0.5, ageDays / 14)
        }
        return groups.sorted { a, b in
            if (a.status == "skipped") != (b.status == "skipped") { return b.status == "skipped" }
            let va = value(a), vb = value(b)
            if va != vb { return va > vb }
            if a.capturedAt != b.capturedAt { return a.capturedAt > b.capturedAt }
            return a.groupKey < b.groupKey
        }
    }

    /// Files within `maxGap` of a neighbour, or sharing a name with another
    /// file; only these need a CLIP vector to group.
    private static func candidateIDs(_ rows: [Row1], maxGap: Double) -> Set<Int64> {
        var ids = Set<Int64>()
        for (a, b) in zip(rows, rows.dropFirst()) where b.time - a.time <= maxGap { ids.insert(a.id); ids.insert(b.id) }
        var stems: [String: [Int64]] = [:]
        for row in rows { stems[NearDuplicateGrouping.stem(row.path), default: []].append(row.id) }
        for group in stems.values where group.count > 1 { ids.formUnion(group) }
        return ids
    }

    private static func loadEmbeddings(_ db: GRDB.Database, ids: [Int64]) throws -> [Int64: [Float]] {
        var out: [Int64: [Float]] = [:]
        for start in stride(from: 0, to: ids.count, by: 500) {
            let slice = Array(ids[start..<min(ids.count, start + 500)])
            let marks = slice.map { _ in "?" }.joined(separator: ",")
            for row in try Row.fetchAll(db, sql: "SELECT file_id, embedding FROM clip_embeddings WHERE file_id IN (\(marks))",
                                        arguments: StatementArguments(slice)) {
                let blob: Data = row["embedding"]
                if let vector = NearDuplicateGrouping.decodeEmbedding(blob) { out[row["file_id"]] = vector }
            }
        }
        return out
    }

    /// One shot per file name: a RAW and its JPEG are one try. The viewable
    /// (non-RAW) file is primary. Shots in capture order.
    static func shots(_ items: [NearDuplicateGrouping.Item]) -> [CatalogTakeShot] {
        var order: [String] = []
        var byStem: [String: [NearDuplicateGrouping.Item]] = [:]
        for item in items {
            let stem = NearDuplicateGrouping.stem(item.path)
            if byStem[stem] == nil { order.append(stem) }
            byStem[stem, default: []].append(item)
        }
        return order.compactMap { stem -> CatalogTakeShot? in
            guard let files = byStem[stem] else { return nil }
            let sorted = files.sorted { a, b in
                let ra = NearDuplicateGrouping.isRaw(a.path), rb = NearDuplicateGrouping.isRaw(b.path)
                return ra != rb ? !ra : a.id < b.id
            }
            return CatalogTakeShot(primaryFileID: sorted[0].id, fileIDs: Array(sorted.map(\.id).prefix(8)), path: sorted[0].path,
                                   capturedAt: files.compactMap(\.time).min())
        }
        .sorted { ($0.capturedAt ?? 0, $0.primaryFileID) < ($1.capturedAt ?? 0, $1.primaryFileID) }
    }

    static func key(_ sortedIDs: [Int64]) -> String {
        let digest = SHA256.hash(data: Data(sortedIDs.map(String.init).joined(separator: ",").utf8))
        return "takes-" + digest.prefix(8).map { String(format: "%02x", $0) }.joined()
    }

    private static func decide(_ db: GRDB.Database, _ decision: CatalogTakeDecision) throws {
        if decision.decision == "reopen" {
            try db.execute(sql: "DELETE FROM take_group_decisions WHERE group_key=?", arguments: [decision.groupKey])
            let ids = Set(decision.fileIDs)
            let rows = try Row.fetchAll(db, sql: "SELECT group_key,file_ids_json FROM take_group_decisions")
            for row in rows {
                let json: String = row["file_ids_json"]
                if let stored = try? JSONDecoder().decode([Int64].self, from: Data(json.utf8)), ids.isSubset(of: Set(stored)) {
                    let key: String = row["group_key"]
                    try db.execute(sql: "DELETE FROM take_group_decisions WHERE group_key=?", arguments: [key])
                }
            }
            return
        }
        let json = String(decoding: try JSONEncoder().encode(Array(Set(decision.fileIDs)).sorted()), as: UTF8.self)
        try db.execute(sql: """
            INSERT INTO take_group_decisions(group_key,decision,file_ids_json,decided_at) VALUES(?,?,?,?)
            ON CONFLICT(group_key) DO UPDATE SET decision=excluded.decision,file_ids_json=excluded.file_ids_json,decided_at=excluded.decided_at
            """, arguments: [decision.groupKey, decision.decision, json, Date().timeIntervalSince1970])
    }

    // MARK: - Scoring

    private struct FileRow { let id: Int64; let path: String; let kind: String; let time: Double?; let revision: String; let cached: String? }

    static func score(fileIDs: [Int64], database: Database, faces: TakeFaceAnalyzing) async throws -> [CatalogTakeQuality] {
        let ids = Array(Set(fileIDs)).sorted()
        let version = TakeScoring.version + "/" + faces.id
        let rows: [FileRow] = try await database.pool.read { db in
            try ids.compactMap { id -> FileRow? in
                guard let row = try Row.fetchOne(db, sql: """
                    SELECT f.id,f.path_text,f.kind,f.created_at,q.signals_json,q.source_revision,q.version
                    FROM files f LEFT JOIN take_quality q ON q.file_id=f.id WHERE f.id=? AND f.kind IN ('image','video')
                    """, arguments: [id]) else { return nil }
                let revision = try CatalogStore.revision(db, fileID: id)
                let storedRevision: String? = row["source_revision"], storedVersion: String? = row["version"]
                let cached: String? = storedRevision == revision && storedVersion == version ? row["signals_json"] : nil
                return FileRow(id: id, path: row["path_text"], kind: row["kind"], time: row["created_at"], revision: revision, cached: cached)
            }
        }
        guard !rows.isEmpty else { throw CatalogStore.InvalidRequest() }
        let items = rows.map { NearDuplicateGrouping.Item(id: $0.id, path: $0.path, time: $0.time, phash: nil, embedding: nil) }
        let shots = self.shots(items)
        let byID = Dictionary(uniqueKeysWithValues: rows.map { ($0.id, $0) })
        var signals: [Int64: TakeScoring.Signals] = [:]
        var missing: [FileRow] = []
        for shot in shots {
            guard let row = byID[shot.primaryFileID] else { continue }
            if let json = row.cached, let decoded = try? JSONDecoder().decode(TakeScoring.Signals.self, from: Data(json.utf8)) {
                signals[row.id] = decoded
            } else {
                missing.append(row)
            }
        }
        typealias Computed = (FileRow, (signals: TakeScoring.Signals, complete: Bool)?)
        let computed = await withTaskGroup(of: Computed.self) { group -> [Computed] in
            var results: [Computed] = []
            var next = 0
            let width = max(1, min(4, Hardware.workerCap))
            func add() {
                let row = missing[next]
                next += 1
                group.addTask { (row, await TakeMediaAnalysis.signals(path: row.path, isVideo: row.kind == "video", faces: faces)) }
            }
            while next < min(width, missing.count) { add() }
            while let result = await group.next() {
                results.append(result)
                if next < missing.count { add() }
            }
            return results
        }
        let fresh: [(id: Int64, revision: String, json: String)] = computed.compactMap { row, result in
            guard let result, result.complete, let data = try? JSONEncoder().encode(result.signals) else { return nil }
            return (row.id, row.revision, String(decoding: data, as: UTF8.self))
        }
        if !fresh.isEmpty {
            // The cache write must not hold up the answer: the writer can be busy
            // with scan or index work for seconds.
            let now = Date().timeIntervalSince1970
            Task.detached(priority: .utility) {
                try? await database.pool.write { db in
                    for entry in fresh {
                        try db.execute(sql: """
                            INSERT INTO take_quality(file_id,source_revision,version,signals_json,computed_at) VALUES(?,?,?,?,?)
                            ON CONFLICT(file_id) DO UPDATE SET source_revision=excluded.source_revision,version=excluded.version,
                              signals_json=excluded.signals_json,computed_at=excluded.computed_at
                            """, arguments: [entry.id, entry.revision, version, entry.json, now])
                    }
                }
            }
        }
        for (row, result) in computed { if let result { signals[row.id] = result.signals } }
        let scored = TakeScoring.score(shots.map {
            TakeScoring.Shot(primaryFileID: $0.primaryFileID, fileIDs: $0.fileIDs, path: $0.path, signals: signals[$0.primaryFileID])
        })
        return scored.map(quality)
    }

    static func quality(_ q: TakeScoring.Quality) -> CatalogTakeQuality {
        let box = { (b: TakeScoring.Box) in CatalogTakeBox(x: b.x, y: b.y, width: b.width, height: b.height) }
        return CatalogTakeQuality(
            primaryFileID: q.primaryFileID, fileIDs: q.fileIDs, path: q.path, score: q.score, rank: q.rank, recommended: q.recommended,
            reasons: q.reasons, sharpness: q.sharpness, exposure: q.exposure, motionBlur: q.sharpness == nil ? nil : q.motionBlur,
            faceCount: q.sharpness == nil ? nil : q.faceCount, eyesClosed: q.sharpness == nil ? nil : q.eyesClosed,
            faceCutOff: q.sharpness == nil ? nil : q.faceCutOff, stability: q.stability, action: q.action, peakSeconds: q.peakSeconds,
            subject: q.subject.map(box),
            faces: q.faces.prefix(16).map { box(TakeScoring.Box(x: $0.x, y: $0.y, width: $0.width, height: $0.height)) })
    }

    // MARK: - Goal ranking

    static func rankWithGoal(goal: String, primaryIDs: [Int64], database: Database) async throws -> CatalogTakeGoalRanking {
        guard case .ready(let kind) = await DeepAnalyze.shared.loadState, kind.supportsVision else {
            return CatalogTakeGoalRanking(goal: goal, status: "unavailable", order: primaryIDs, metFileIDs: [],
                                          summary: "Load a description model in Describe Files to rank by a goal.")
        }
        let candidates = Array(primaryIDs.prefix(goalTiles))
        let paths: [String] = try await database.pool.read { db in
            try candidates.map { id in
                guard let path = try String.fetchOne(db, sql: "SELECT path_text FROM files WHERE id=? AND kind='image'", arguments: [id]) else {
                    throw CatalogStore.InvalidRequest()
                }
                return path
            }
        }
        guard let sheet = TakeMediaAnalysis.contactSheet(paths: paths) else { throw CatalogStore.InvalidRequest() }
        let prompt = TakeMediaAnalysis.goalPrompt(goal: goal, count: paths.count)
        let image = CIImage(cgImage: sheet)
        let sample = try await DeepAnalyze.shared.benchmarkPass(prompt: prompt, image: image, maxTokens: 160)
        guard let reply = TakeMediaAnalysis.parseGoalReply(sample.output, count: paths.count) else {
            return CatalogTakeGoalRanking(goal: goal, status: "unclear", order: primaryIDs, metFileIDs: [],
                                          summary: "The model's answer was unclear. The ranking stays on photo quality.", model: kind.rawValue)
        }
        let best = candidates[reply.best - 1]
        let met = (reply.met ?? []).map { candidates[$0 - 1] }
        var order = [best]
        for id in primaryIDs where met.contains(id) && id != best { order.append(id) }
        for id in primaryIDs where !order.contains(id) { order.append(id) }
        let why = (reply.why ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        let summary = met.isEmpty ? "No photo clearly shows the goal; this one comes closest." + (why.isEmpty ? "" : " " + String(why.prefix(200)))
                                  : (why.isEmpty ? "Best shows the goal." : String(why.prefix(240)))
        return CatalogTakeGoalRanking(goal: goal, status: "ranked", order: order, metFileIDs: met, summary: summary, model: kind.rawValue)
    }
}
