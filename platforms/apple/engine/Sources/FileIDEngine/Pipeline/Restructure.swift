// Proposed folder-hierarchy generator. Reads each file's metadata
// (faces, GPS, date, VLM caption) and produces (old_path, new_path)
// pairs the UI renders as a diff. Rule-based by design — at 50K
// files the LLM cost would dominate, and a deterministic layout is
// what the user can trust.
//
// Rule cascade, first match wins (Windows restructure::classify is canonical):
//   1. Named person → People/<Name>/<Year>/      (category "People/<Name>")
//   2. GPS location → Places/<lat,lon>/<Year>/    (category "Places/<bucket>")
//   3. Document     → Documents/<Year>/           (category "document")
//   4. Image        → Photos/<Year>/<MonthName>/  (category "photo")
//   5. Video        → Videos/<Year>/              (category "video")
//   6. Audio        → Audio/<Year>/                (category "audio"; flat when no date signal)
//   7. Fallback     → Misc/                       (category "misc")
//
// `vlm_proposed_name` becomes the new filename within whichever
// folder the heuristic picks. A missing timestamp coerces to 1970.
import Foundation
import CryptoKit
import GRDB
import FileIDShared

public struct RestructureProposal: Sendable {
    public let fileID: Int64
    public let oldPath: String
    public let newPath: String
    /// Wire category — the Windows lowercase vocabulary ("photo"/"document"/
    /// "video"/"audio"/"misc") or a "People/<name>" / "Places/<bucket>" /
    /// semantic-group label. Drives the Sankey grouping AND the source-folder
    /// homogeneity classification, so it must be the category (NOT the full
    /// destination path). (audit F-C3-019)
    public let bucket: String
    /// Butler confidence band — "auto" / "review" / "ask" (RESTRUCTURE.md §6).
    public let confidence: String
    /// Plain-language "why filed here".
    public let reason: String?

    public init(fileID: Int64, oldPath: String, newPath: String, bucket: String,
                confidence: String = "", reason: String? = nil) {
        self.fileID = fileID
        self.oldPath = oldPath
        self.newPath = newPath
        self.bucket = bucket
        self.confidence = confidence
        self.reason = reason
    }
}

public enum Restructure {

    private static func rootBounds(
        _ raw: String
    ) -> (root: String, prefix: String, upper: String) {
        var root = raw
        while root.count > 1, root.hasSuffix("/") { root.removeLast() }
        let prefix = root == "/" ? "/" : root + "/"
        // The prefix always ends in ASCII '/', whose immediate binary successor
        // is '0'. SQLite's BINARY range [prefix, upper) therefore includes
        // exactly descendants and excludes siblings such as `/library-old`.
        let upper = String(prefix.dropLast()) + "0"
        return (root, prefix, upper)
    }

    static let largePlanThreshold = 50_000
    private static let largePlanChunk = 4_096

    /// Million-file planning path. Metadata is classified in bounded chunks
    /// into an on-disk sidecar, folder tiers are aggregated in SQL, and only a
    /// bounded preview crosses IPC. Smaller libraries keep the richer semantic
    /// planner below.
    static func proposeLargeStoredIfNeeded(
        database: Database,
        libraryRoot: URL,
        directory override: URL? = nil,
        threshold: Int = largePlanThreshold
    ) async throws -> RestructurePlan? {
        let bounds = rootBounds(libraryRoot.path)
        let count = try await database.pool.read { db in
            try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM files f WHERE f.failed=0
                  AND (?='' OR f.path_text=? OR (f.path_text>=? AND f.path_text<?))
                """, arguments: [
                    bounds.root, bounds.root, bounds.prefix, bounds.upper]) ?? 0
        }
        guard count > threshold else { return nil }
        guard let directory = override ?? storedPlansDirectory else {
            throw CocoaError(.fileNoSuchFile)
        }
        try FileManager.default.createDirectory(
            at: directory, withIntermediateDirectories: true)
        // An engine kill mid-plan orphans the planning scratch DB (the defer
        // below never runs); sweep stale ones before creating a new scratch.
        if let files = try? FileManager.default.contentsOfDirectory(
            at: directory, includingPropertiesForKeys: nil)
        {
            for file in files where file.lastPathComponent.contains(".planning.sqlite") {
                try? FileManager.default.removeItem(at: file)
            }
        }
        let planningURL = directory.appendingPathComponent(
            "\(UUID().uuidString.lowercased()).planning.sqlite")
        defer { try? FileManager.default.removeItem(at: planningURL) }
        let planDB = try DatabaseQueue(path: planningURL.path)
        try await planDB.write { db in
            try db.execute(sql: """
                CREATE TABLE raw_moves(
                    seq INTEGER PRIMARY KEY,
                    file_id INTEGER NOT NULL,
                    source TEXT NOT NULL,
                    source_folder TEXT NOT NULL,
                    destination TEXT NOT NULL,
                    category TEXT NOT NULL,
                    confidence TEXT NOT NULL,
                    reason TEXT);
                CREATE TABLE folder_stats(
                    folder TEXT NOT NULL,
                    category TEXT NOT NULL,
                    count INTEGER NOT NULL,
                    PRIMARY KEY(folder,category));
                CREATE TABLE folder_tiers(
                    folder TEXT PRIMARY KEY,
                    tier TEXT NOT NULL);
                """)
        }

        var lastID = Int64.min
        var sequence = 0
        while true {
            try Task.checkCancellation()
            // Keyset pages release the pool read transaction between chunks, so
            // a million-file plan cannot pin one long-lived SQLite snapshot and
            // prevent WAL checkpoints while scanning continues.
            let cursorID = lastID
            let chunk = try await database.pool.read { sourceDB -> [FileForClassify] in
                let rows = try Row.fetchAll(sourceDB, sql: """
                    SELECT
                      f.id, f.path_text, f.kind, f.created_at, f.modified_at,
                      f.location_lat, f.location_lon, f.has_text, f.vlm_proposed_name,
                      (SELECT GROUP_CONCAT(name, char(31))
                         FROM (SELECT DISTINCT p.name
                                 FROM persons p
                                 JOIN face_prints fp ON fp.person_id = p.id
                                WHERE fp.file_id = f.id
                                  AND p.name IS NOT NULL AND p.name <> ''
                                ORDER BY p.name)) AS names
                    FROM files f
                    WHERE f.failed=0
                      AND (?='' OR f.path_text=? OR (f.path_text>=? AND f.path_text<?))
                      AND f.id > ?
                    ORDER BY f.id
                    LIMIT ?
                    """, arguments: [
                        bounds.root, bounds.root, bounds.prefix, bounds.upper,
                        cursorID, largePlanChunk])
                return rows.map { row in
                    let names: String? = row["names"]
                    return FileForClassify(
                        fileID: row["id"] ?? 0,
                        source: row["path_text"] ?? "",
                        kind: row["kind"] ?? "other",
                        modifiedUnix: row["modified_at"] ?? 0,
                        createdUnix: row["created_at"],
                        personName: firstPersonName(names),
                        lat: row["location_lat"], lon: row["location_lon"],
                        hasText: (row["has_text"] ?? 0) != 0,
                        vlmProposed: row["vlm_proposed_name"])
                }
            }
            guard !chunk.isEmpty else { break }
            lastID = chunk.last?.fileID ?? lastID
            let proposals = ruleClassify(chunk, libraryRoot: libraryRoot)
            let chunkBase = sequence
            try await planDB.write { plan in
                for (offset, proposal) in proposals.enumerated() {
                    let folder = (proposal.oldPath as NSString).deletingLastPathComponent
                    try plan.execute(sql: """
                        INSERT INTO raw_moves
                          (seq,file_id,source,source_folder,destination,category,confidence,reason)
                        VALUES (?,?,?,?,?,?,?,?)
                        """, arguments: [
                            chunkBase + offset, proposal.fileID, proposal.oldPath, folder,
                            proposal.newPath, proposal.bucket,
                            proposal.confidence, proposal.reason])
                    try plan.execute(sql: """
                        INSERT INTO folder_stats(folder,category,count) VALUES (?,?,1)
                        ON CONFLICT(folder,category) DO UPDATE SET count=count+1
                        """, arguments: [folder, proposal.bucket])
                }
            }
            sequence = chunkBase + proposals.count
            if chunk.count < largePlanChunk { break }
        }

        try await planDB.write { db in
            try db.execute(sql: """
                INSERT INTO folder_tiers(folder,tier)
                SELECT folder,
                  CASE
                    WHEN total <= 2
                      OR lower(folder) LIKE '%/downloads'
                      OR lower(folder) LIKE '%/downloaded'
                      OR lower(folder) LIKE '%/new folder'
                      OR lower(folder) LIKE '%/untitled'
                      OR lower(folder) LIKE '%/temp'
                      OR lower(folder) LIKE '%/tmp'
                      OR lower(folder) LIKE '%/misc'
                      OR lower(folder) LIKE '%/other'
                      OR lower(folder) LIKE '%/stuff'
                      OR lower(folder) LIKE '%/things'
                      OR lower(folder) LIKE '%/files' THEN 'Junk'
                    WHEN top_count * 100 >= total * 80 THEN 'Anchor'
                    ELSE 'Mixed'
                  END
                FROM (
                  SELECT folder, SUM(count) AS total, MAX(count) AS top_count
                  FROM folder_stats GROUP BY folder
                )
                """)
        }

        let summary = try await planDB.read { db -> (
            total: Int, categories: [RestructureCategoryCount], folders: FolderClassificationCounts
        ) in
            let total = try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM raw_moves r
                JOIN folder_tiers t ON t.folder=r.source_folder
                WHERE t.tier <> 'Anchor'
                """) ?? 0
            let categories = try Row.fetchAll(db, sql: """
                SELECT r.category, COUNT(*) AS n FROM raw_moves r
                JOIN folder_tiers t ON t.folder=r.source_folder
                WHERE t.tier <> 'Anchor'
                GROUP BY r.category ORDER BY n DESC, r.category ASC
                """).map {
                    RestructureCategoryCount(
                        category: $0["category"] ?? "", count: $0["n"] ?? 0)
                }
            func tierCount(_ tier: String) throws -> Int {
                try Int.fetchOne(
                    db, sql: "SELECT COUNT(*) FROM folder_tiers WHERE tier=?",
                    arguments: [tier]) ?? 0
            }
            return (total, categories, FolderClassificationCounts(
                anchorFolders: try tierCount("Anchor"),
                mixedFolders: try tierCount("Mixed"),
                junkFolders: try tierCount("Junk")))
        }

        let stored = try await planDB.read { db in
            let cursor = try Row.fetchCursor(db, sql: """
                SELECT r.file_id,r.source,r.destination,r.category,t.tier,
                       r.confidence,r.reason
                FROM raw_moves r JOIN folder_tiers t ON t.folder=r.source_folder
                WHERE t.tier <> 'Anchor' ORDER BY r.seq
                """)
            return try storePlanStream(
                libraryRoot: libraryRoot.path,
                totalMoves: summary.total,
                directory: directory,
                nextMove: {
                    guard let row = try cursor.next() else { return nil }
                    return RestructureMove(
                        fileID: row["file_id"] ?? 0,
                        source: row["source"] ?? "",
                        destination: row["destination"] ?? "",
                        category: row["category"] ?? "",
                        tier: row["tier"],
                        confidence: row["confidence"] ?? "",
                        reason: row["reason"])
                })
        }
        let truncated = summary.total > storedPlanPreviewCap
        if !truncated {
            try? FileManager.default.removeItem(
                at: directory.appendingPathComponent("\(stored.planID).ndjson"))
        }
        return RestructurePlan(
            libraryRoot: libraryRoot.path,
            moves: stored.preview,
            categoryCounts: summary.categories,
            folderClassifications: summary.folders,
            planID: truncated ? stored.planID : nil,
            totalMoves: truncated ? summary.total : nil,
            truncated: truncated)
    }

    /// Build proposals for every image in the library. The caller (UI)
    /// renders, the user filters/checks, then apply runs the moves.
    public static func proposeAll(
        database: Database,
        libraryRoot: URL
    ) async throws -> PlanResult {
        let bounds = rootBounds(libraryRoot.path)
        let signalCap: Int = switch Hardware.memoryTier {
        case .low: 20_000
        case .balanced: 50_000
        case .high: 100_000
        }
        struct Source: Sendable {
            let id: Int64
            let path: String
            let kind: String
            let createdAt: Double?
            let modifiedAt: Double?
            let lat: Double?
            let lon: Double?
            let hasText: Int
            let vlmProposed: String?
            let personNames: String?     // comma-joined
        }
        let loaded = try await database.pool.read {
            db -> (rows: [Source], embeddings: [Int64: [Float]], tags: [Int64: [String]], docEmbeddings: [Int64: [Float]]) in
            // Per-file named-person strings, then split back in Swift
            // (avoids a per-file second query).
            //
            // Names come from a deduped, ordered correlated subquery — NOT
            // `GROUP_CONCAT(DISTINCT p.name, char(31))`, which SQLite rejects
            // at run with "DISTINCT aggregates must have exactly one argument"
            // (the separator arg is illegal under DISTINCT). The old form
            // prepared but threw at execution, crashing the Restructure plan.
            //
            // Separator is the ASCII unit-separator (\u{1F}). Comma would
            // silently shred names like "Smith, John" into two fragments and
            // emit an incorrect bucket — `\u{1F}` never appears in a person
            // name so the round-trip is lossless.
            let r = try GRDB.Row.fetchAll(db, sql: """
                SELECT
                  f.id, f.path_text, f.kind, f.created_at, f.modified_at,
                  f.location_lat, f.location_lon, f.has_text, f.vlm_proposed_name,
                  (SELECT GROUP_CONCAT(name, char(31))
                     FROM (SELECT DISTINCT p.name
                             FROM persons p
                             JOIN face_prints fp ON fp.person_id = p.id
                            WHERE fp.file_id = f.id
                              AND p.name IS NOT NULL AND p.name <> ''
                            ORDER BY p.name)) AS names
                FROM files f
                WHERE f.failed = 0
                  AND (? = '' OR f.path_text = ? OR (f.path_text >= ? AND f.path_text < ?))
                ORDER BY f.id
                """, arguments: [bounds.root, bounds.root, bounds.prefix, bounds.upper])
            let rows = r.map { row in
                Source(
                    id: row["id"] ?? 0,
                    path: row["path_text"] ?? "",
                    kind: row["kind"] ?? "other",
                    createdAt: row["created_at"],
                    modifiedAt: row["modified_at"],
                    lat: row["location_lat"],
                    lon: row["location_lon"],
                    hasText: row["has_text"] ?? 0,
                    vlmProposed: row["vlm_proposed_name"],
                    personNames: row["names"]
                )
            }
            // CLIP image embeddings (512-d f32 LE) drive the semantic clusterer.
            var embeddings: [Int64: [Float]] = [:]
            let ecursor = try GRDB.Row.fetchCursor(db, sql: """
                SELECT ce.file_id, ce.embedding FROM clip_embeddings ce
                JOIN files f ON f.id = ce.file_id
                WHERE f.failed = 0 AND f.kind IN ('image', 'video', 'model')
                  AND (? = '' OR f.path_text = ? OR (f.path_text >= ? AND f.path_text < ?))
                """, arguments: [bounds.root, bounds.root, bounds.prefix, bounds.upper])
            while embeddings.count < signalCap, let row = try ecursor.next() {
                let id: Int64 = row["file_id"] ?? 0
                if let data: Data = row["embedding"], !data.isEmpty, data.count % 4 == 0 {
                    embeddings[id] = Self.floatsLE(data)
                }
            }
            // Content tags for distinctive-term naming + fusion.
            var tags: [Int64: [String]] = [:]
            let trows = try GRDB.Row.fetchAll(db, sql: """
                SELECT DISTINCT t.file_id, t.tag FROM tags t
                JOIN files f ON f.id = t.file_id
                WHERE t.source IN ('auto','vlm','user') AND f.failed = 0
                  AND (? = '' OR f.path_text = ? OR (f.path_text >= ? AND f.path_text < ?))
                LIMIT ?
                """, arguments: [
                    bounds.root, bounds.root, bounds.prefix, bounds.upper,
                    signalCap * 8])
            for row in trows {
                let id: Int64 = row["file_id"] ?? 0
                if let t: String = row["tag"] { tags[id, default: []].append(t) }
            }
            // BGE document text embeddings (384-d f32 LE), cached at scan. The doc pass
            // reads these directly; a doc without one is embedded at plan time (older scan).
            var docEmbeddings: [Int64: [Float]] = [:]
            let dcursor = try GRDB.Row.fetchCursor(db, sql: """
                SELECT te.file_id, te.embedding FROM text_embeddings te
                JOIN files f ON f.id = te.file_id
                WHERE f.failed = 0 AND f.kind IN ('doc', 'pdf')
                  AND (? = '' OR f.path_text = ? OR (f.path_text >= ? AND f.path_text < ?))
                """, arguments: [bounds.root, bounds.root, bounds.prefix, bounds.upper])
            while docEmbeddings.count < signalCap, let row = try dcursor.next() {
                let id: Int64 = row["file_id"] ?? 0
                if let data: Data = row["embedding"], !data.isEmpty, data.count % 4 == 0 {
                    docEmbeddings[id] = Self.floatsLE(data)
                }
            }
            return (rows, embeddings, tags, docEmbeddings)
        }
        let rows = loaded.rows

        // Butler P1: semantic + learn-your-style placement for image, video AND 3D-model
        // files that have a CLIP embedding (a video's is its keyframe's, a model's is its
        // rendered-shape thumbnail — see Tagging.processVideo/processModel); everything else
        // (and density noise) falls back to the rule cascade. Mirrors the Windows engine.
        let semanticFiles: [RestructureSemantic.SemanticFile] = rows.compactMap { s in
            guard s.kind == "image" || s.kind == "video" || s.kind == "model",
                  let clip = loaded.embeddings[s.id] else { return nil }
            // created_at/modified_at are seconds since the Unix epoch (byte-faithful
            // with the Windows engine), so they feed day-of-year directly.
            let timeUnix = (s.createdAt ?? s.modifiedAt) ?? 0
            return RestructureSemantic.SemanticFile(
                fileID: s.id, source: s.path, clip: clip,
                tags: loaded.tags[s.id] ?? [], timeUnix: timeUnix)
        }

        var proposals: [RestructureProposal] = []
        proposals.reserveCapacity(rows.count)
        var movedIDs = Set<Int64>()
        // Source folders the semantic butler actively claimed (every file
        // relocated into a content group). They classify Anchor on destination
        // homogeneity but are real relocations, not in-place anchors — exempt
        // them from the anchor strip so their best moves survive. (F-C1-004)
        var semanticSourceFolders = Set<String>()
        if semanticFiles.count >= 2 {
            let protos = RestructureSemantic.folderPrototypes(semanticFiles, minFiles: 4)
            let moves = RestructureSemantic.classify(
                files: semanticFiles, prototypes: protos, libraryRoot: libraryRoot.path)
            for m in moves {
                let name = (m.source as NSString).lastPathComponent
                let newPath = (m.destinationDir as NSString).appendingPathComponent(name)
                proposals.append(RestructureProposal(
                    fileID: m.fileID, oldPath: m.source, newPath: newPath,
                    bucket: m.category, confidence: m.confidence.rawValue, reason: m.reason))
                movedIDs.insert(m.fileID)
                semanticSourceFolders.insert((m.source as NSString).deletingLastPathComponent)
            }
        }

        // Butler R3: document-content pass. Cluster documents by their BGE text embedding
        // (the content) — far stronger than the filename fallback (owner A/B: nearest-
        // neighbour-same-folder 49%→57%). Prefer the embedding cached at SCAN (instant);
        // fall back to embedding at plan time for a doc scanned before BGE was installed.
        // Docs with no extractable text fall through to the bag-of-words pass. Mirrors the
        // Windows engine's classify_documents (which always reads its scan-time store).
        if RestructureSemantic.nonImageEnabled {
            let bgeDir = BGETextService.defaultModelDir
            let docFiles: [RestructureSemantic.SemanticFile] = rows.compactMap { s in
                guard !movedIDs.contains(s.id), s.kind == "doc" || s.kind == "pdf" else { return nil }
                let emb: [Float]?
                if let stored = loaded.docEmbeddings[s.id] {
                    emb = stored
                } else if BGETextService.shared.load(modelDir: bgeDir),
                          let text = DocText.extract(path: s.path) {
                    emb = BGETextService.shared.embed(text)
                } else {
                    emb = nil
                }
                guard let emb else { return nil }
                let timeUnix = (s.createdAt ?? s.modifiedAt) ?? 0
                return RestructureSemantic.SemanticFile(
                    fileID: s.id, source: s.path, clip: emb,
                    tags: loaded.tags[s.id] ?? [], timeUnix: timeUnix)
            }
            let docMoves = RestructureSemantic.classifyDocuments(
                files: docFiles, libraryRoot: libraryRoot.path)
            for m in docMoves {
                let name = (m.source as NSString).lastPathComponent
                let newPath = (m.destinationDir as NSString).appendingPathComponent(name)
                proposals.append(RestructureProposal(
                    fileID: m.fileID, oldPath: m.source, newPath: newPath,
                    bucket: m.category, confidence: m.confidence.rawValue, reason: m.reason))
                movedIDs.insert(m.fileID)
                semanticSourceFolders.insert((m.source as NSString).deletingLastPathComponent)
            }
        }

        // Butler R1: non-image semantic pass. Cluster everything the doc + image passes
        // didn't claim (video, audio, docs without extractable text, and any
        // embedding-less file) by a filename+tag bag-of-words signature, so a mixed library
        // groups by content instead of dumping every file into <Year>. Additive + separately
        // tuned (nonImageProfile); the rule cascade below still catches the
        // remainder. Owner kill-switch: FILEID_RESTRUCTURE_NONIMAGE=0.
        if RestructureSemantic.nonImageEnabled {
            let nonImageInput: [RestructureSemantic.SemanticFile] = rows.compactMap { s in
                guard !movedIDs.contains(s.id) else { return nil }
                let timeUnix = (s.createdAt ?? s.modifiedAt) ?? 0
                return RestructureSemantic.SemanticFile(
                    fileID: s.id, source: s.path, clip: [],
                    tags: loaded.tags[s.id] ?? [], timeUnix: timeUnix)
            }
            let niMoves = RestructureSemantic.classifyNonImage(
                files: nonImageInput, libraryRoot: libraryRoot.path)
            for m in niMoves {
                let name = (m.source as NSString).lastPathComponent
                let newPath = (m.destinationDir as NSString).appendingPathComponent(name)
                proposals.append(RestructureProposal(
                    fileID: m.fileID, oldPath: m.source, newPath: newPath,
                    bucket: m.category, confidence: m.confidence.rawValue, reason: m.reason))
                movedIDs.insert(m.fileID)
                semanticSourceFolders.insert((m.source as NSString).deletingLastPathComponent)
            }
        }

        // Rule cascade for everything the semantic butler didn't claim.
        let ruleFiles: [FileForClassify] = rows.compactMap { s in
            guard !movedIDs.contains(s.id) else { return nil }
            return FileForClassify(
                fileID: s.id, source: s.path, kind: s.kind,
                modifiedUnix: s.modifiedAt ?? 0, createdUnix: s.createdAt,
                personName: Self.firstPersonName(s.personNames),
                lat: s.lat, lon: s.lon, hasText: s.hasText != 0, vlmProposed: s.vlmProposed)
        }
        proposals.append(contentsOf: ruleClassify(ruleFiles, libraryRoot: libraryRoot))

        // Learn-from-corrections: upgrade any planned move toward a folder the user
        // has previously filed similar files into (the v18 restructure_feedback
        // memory, written on each apply). Additive — only raises confidence on moves
        // the planner already produced, never re-routes — so it can't regress the
        // calibrated passes. Runs on the full proposal set, before the anchor strip
        // preserves the upgraded confidence into the emitted plan. Mirrors the
        // Windows engine (commands/restructure.rs). (R3 → learn-your-style)
        proposals = await RestructureFeedback.boost(database: database, proposals: proposals)

        // Engine-authoritative folder classification on the FULL proposal set
        // (Windows A1/A3): classify each source folder, then strip every move out
        // of an Anchor folder so files the UI promised would "stay put" are never
        // silently relocated. Semantic-claimed folders are exempt — their
        // homogeneity is a real relocation, not an in-place anchor. (F-C3-016)
        //
        // The tiers + Keep/Tidy/Junk counts are computed HERE, on the full PRE-strip
        // set with the same exemption — NOT later on the stripped set. The strip
        // removes every move out of a (non-exempt) Anchor folder, so a post-strip
        // recompute can't see those folders at all and the "Keep" tile would silently
        // undercount the folders actually being left alone. Mirrors the Windows engine,
        // which computes folder_class on the full proposed set before stripping. (audit)
        let folderClass = classifyFolders(proposals)
        let tiers = folderTiersAndCounts(classified: folderClass, exempt: semanticSourceFolders)
        let stripped = stripAnchorFolderMovesExcept(
            proposals, classified: folderClass, exempt: semanticSourceFolders)
        return PlanResult(
            proposals: stripped, tierByFolder: tiers.tierByFolder,
            anchorFolders: tiers.anchor, mixedFolders: tiers.mixed, junkFolders: tiers.junk)
    }

    /// The engine-authoritative plan: the anchor-stripped moves to apply, plus the
    /// folder classification computed on the FULL pre-strip set (with the semantic-
    /// claim exemption) so the Keep/Tidy/Junk tile counts + per-move tier badges match
    /// the Windows engine's `handle_plan_restructure`.
    public struct PlanResult: Sendable {
        public let proposals: [RestructureProposal]
        public let tierByFolder: [String: String]
        public let anchorFolders: Int
        public let mixedFolders: Int
        public let junkFolders: Int
        public init(proposals: [RestructureProposal], tierByFolder: [String: String],
                    anchorFolders: Int, mixedFolders: Int, junkFolders: Int) {
            self.proposals = proposals
            self.tierByFolder = tierByFolder
            self.anchorFolders = anchorFolders
            self.mixedFolders = mixedFolders
            self.junkFolders = junkFolders
        }
    }

    /// Per-source-folder tier labels + rolled-up Anchor/Mixed/Junk counts from the
    /// FULL pre-strip classification. A folder that classified Anchor but is in
    /// `exempt` (the semantic butler actively relocating its files into a content
    /// group — NOT kept in place) is remapped to Mixed so it neither inflates the
    /// "Keep" tile nor labels its surviving moves Anchor. Byte-faithful with the
    /// Windows engine's handle_plan_restructure loop (F-C1-004). (audit — lockstep)
    static func folderTiersAndCounts(
        classified: [ClassifiedFolder], exempt: Set<String>
    ) -> (tierByFolder: [String: String], anchor: Int, mixed: Int, junk: Int) {
        var tierByFolder: [String: String] = [:]
        var anchor = 0, mixed = 0, junk = 0
        for f in classified {
            let effective: FolderClassification =
                (f.classification == .anchor && exempt.contains(f.sourceFolder))
                ? .mixed : f.classification
            switch effective {
            case .anchor: tierByFolder[f.sourceFolder] = "Anchor"; anchor += 1
            case .mixed:  tierByFolder[f.sourceFolder] = "Mixed";  mixed += 1
            case .junk:   tierByFolder[f.sourceFolder] = "Junk";   junk += 1
            }
        }
        return (tierByFolder, anchor, mixed, junk)
    }

    // MARK: - Rule cascade (faithful port of Windows restructure::classify)

    /// One file's signals for the rule cascade. Mirrors the Windows
    /// `FileForClassify`; `vlmProposed` is the macOS-only smart-rename override.
    public struct FileForClassify: Sendable {
        public let fileID: Int64
        public let source: String
        public let kind: String
        public let modifiedUnix: Double
        public let createdUnix: Double?
        public let personName: String?
        public let lat: Double?
        public let lon: Double?
        public let hasText: Bool
        public let vlmProposed: String?

        public init(fileID: Int64, source: String, kind: String,
                    modifiedUnix: Double, createdUnix: Double?,
                    personName: String?, lat: Double?, lon: Double?,
                    hasText: Bool, vlmProposed: String? = nil) {
            self.fileID = fileID
            self.source = source
            self.kind = kind
            self.modifiedUnix = modifiedUnix
            self.createdUnix = createdUnix
            self.personName = personName
            self.lat = lat
            self.lon = lon
            self.hasText = hasText
            self.vlmProposed = vlmProposed
        }
    }

    /// Priority cascade, first match wins (Windows is canonical):
    ///   1. Named person  → People/<Name>/<Year>/      (category "People/<Name>")
    ///   2. GPS location   → Places/<lat,lon>/<Year>/   (category "Places/<b>")
    ///   3. Document       → Documents/<Year>/          (category "document")
    ///   4. Image          → Photos/<Year>/<MonthName>/ (category "photo")
    ///   5. Video          → Videos/<Year>/             (category "video")
    ///   6. Audio          → Audio/<Year>/               (category "audio"; flat when no date signal)
    ///   7. Fallback       → Misc/                      (category "misc")
    /// A zero/missing timestamp produces Ask confidence and a flat (dateless) folder.
    public static func ruleClassify(
        _ files: [FileForClassify], libraryRoot: URL
    ) -> [RestructureProposal] {
        var out: [RestructureProposal] = []
        out.reserveCapacity(files.count)
        for f in files {
            let ts = f.createdUnix ?? f.modifiedUnix
            // A zero or near-zero timestamp is not a real date (FAT32 zero-epoch,
            // corrupt mtime). Flag with Ask confidence and skip year sub-folders
            // so the user isn't silently placed into a 1970 folder.
            let tsValid = ts > 86_400  // > 1970-01-02 avoids zero/near-zero epoch artefacts
            let (y, m) = yearMonth(ts)
            let mname = monthName(m)

            let category: String
            let confidence: String
            let reason: String
            let dir: URL
            if let name = f.personName, !name.isEmpty {
                let safe = FilesystemNameSafe.componentSafe(name)
                dir = libraryRoot.appendingPathComponent("People", isDirectory: true)
                    .appendingPathComponent(safe, isDirectory: true)
                    .appendingPathComponent("\(y)", isDirectory: true)
                category = "People/\(safe)"
                confidence = "auto"
                reason = "Named person: \(safe)"
            } else if let lat = f.lat, let lon = f.lon {
                let latB = (lat * 2).rounded() / 2
                let lonB = (lon * 2).rounded() / 2
                let b = String(format: "%.1f_%.1f", latB, lonB)
                dir = libraryRoot.appendingPathComponent("Places", isDirectory: true)
                    .appendingPathComponent(b, isDirectory: true)
                    .appendingPathComponent("\(y)", isDirectory: true)
                category = "Places/\(b)"
                confidence = "review"
                reason = "Taken at a shared location"
            } else if f.hasText || f.kind == "pdf" || f.kind == "doc" {
                let docs = libraryRoot.appendingPathComponent("Documents", isDirectory: true)
                dir = tsValid ? docs.appendingPathComponent("\(y)", isDirectory: true) : docs
                category = "document"
                confidence = tsValid ? "review" : "ask"
                reason = tsValid ? "Document from \(y)" : "Document — no date signal"
            } else if f.kind == "image" {
                let photos = libraryRoot.appendingPathComponent("Photos", isDirectory: true)
                dir = tsValid
                    ? photos.appendingPathComponent("\(y)", isDirectory: true)
                            .appendingPathComponent(mname, isDirectory: true)
                    : photos
                category = "photo"
                confidence = tsValid ? "review" : "ask"
                reason = tsValid ? "Photo from \(mname) \(y)" : "Photo — no capture date"
            } else if f.kind == "video" {
                let videos = libraryRoot.appendingPathComponent("Videos", isDirectory: true)
                dir = tsValid ? videos.appendingPathComponent("\(y)", isDirectory: true) : videos
                category = "video"
                confidence = tsValid ? "review" : "ask"
                reason = tsValid ? "Video from \(y)" : "Video — no date signal"
            } else if f.kind == "audio" {
                let audio = libraryRoot.appendingPathComponent("Audio", isDirectory: true)
                dir = tsValid ? audio.appendingPathComponent("\(y)", isDirectory: true) : audio
                category = "audio"
                confidence = "review"
                reason = tsValid ? "Audio file from \(y)" : "Audio file"
            } else if f.kind == "model" {
                dir = libraryRoot.appendingPathComponent("3D Models", isDirectory: true)
                category = "model"
                confidence = "review"
                reason = "3D model"
            } else {
                dir = libraryRoot.appendingPathComponent("Misc", isDirectory: true)
                category = "misc"
                confidence = "ask"
                reason = "No strong signal — left for you to decide"
            }

            // Filename: keep original or use the VLM suggestion. The VLM name is
            // already slug-sanitized; the extension is sanitized here in case
            // the source filename was malformed.
            let oldURL = URL(fileURLWithPath: f.source)
            let ext = FilesystemNameSafe.componentSafe(oldURL.pathExtension, maxLength: 16)
            let newName: String
            if let p = f.vlmProposed, !p.isEmpty {
                newName = ext.isEmpty || ext == "_" ? p : "\(p).\(ext)"
            } else {
                newName = FilesystemNameSafe.componentSafe(oldURL.lastPathComponent)
            }
            let target = dir.appendingPathComponent(newName)
            out.append(RestructureProposal(
                fileID: f.fileID, oldPath: f.source, newPath: target.path,
                bucket: category, confidence: confidence, reason: reason))
        }
        return out
    }

    /// First named person from the `\u{1F}`-joined names string, or nil when
    /// there's no named person (Windows filters empty → None → next branch).
    static func firstPersonName(_ names: String?) -> String? {
        guard let names, !names.isEmpty else { return nil }
        let first = names.split(separator: "\u{1F}").first
            .map { String($0).trimmingCharacters(in: .whitespaces) }
        guard let f = first, !f.isEmpty else { return nil }
        return f
    }

    // MARK: - Folder classification (Windows restructure::classify_folders)

    enum FolderClassification: Sendable, Equatable { case anchor, mixed, junk }

    struct ClassifiedFolder: Sendable {
        let sourceFolder: String
        let classification: FolderClassification
        let moveCount: Int
        let dominantCategory: String
    }

    private static let genericFolderNames: Set<String> = [
        "downloads", "downloaded", "new folder", "untitled", "temp", "tmp",
        "misc", "other", "stuff", "things", "files",
    ]

    /// Classify each source folder by destination-category homogeneity. The
    /// dominant category is the most frequent (so a folder of one person's
    /// photos is dominated by "People/<that person>" — homogeneity is measured
    /// against the DOMINANT person, F-C3-035). ≤2 files or a generic name →
    /// Junk; ≥80% one category → Anchor; else Mixed.
    static func classifyFolders(_ moves: [RestructureProposal]) -> [ClassifiedFolder] {
        var byFolder: [String: [RestructureProposal]] = [:]
        for m in moves {
            let parent = (m.oldPath as NSString).deletingLastPathComponent
            byFolder[parent, default: []].append(m)
        }
        var out: [ClassifiedFolder] = []
        out.reserveCapacity(byFolder.count)
        // Deterministic order (folder) so the result is stable across runs.
        for folder in byFolder.keys.sorted() {
            let items = byFolder[folder]!
            var hist: [String: Int] = [:]
            for m in items { hist[m.bucket, default: 0] += 1 }
            let total = items.count
            let dominant = hist.max { a, b in
                a.value != b.value ? a.value < b.value : a.key > b.key
            }
            let dominantCategory = dominant?.key ?? ""
            let top = dominant?.value ?? 0
            let homogeneity = total > 0 ? Float(top) / Float(total) : 0

            let name = (folder as NSString).lastPathComponent.lowercased()
            let generic = genericFolderNames.contains(name)
            let classification: FolderClassification
            if generic || total <= 2 {
                classification = .junk
            } else if homogeneity >= 0.80 {
                classification = .anchor
            } else {
                classification = .mixed
            }
            out.append(ClassifiedFolder(
                sourceFolder: folder, classification: classification,
                moveCount: total, dominantCategory: dominantCategory))
        }
        return out
    }

    /// Drop every move whose source folder classified Anchor — those files stay
    /// put — except folders in `exempt` (the semantic butler's real
    /// relocations). (Windows strip_anchor_folder_moves_except, F-C3-016)
    static func stripAnchorFolderMovesExcept(
        _ moves: [RestructureProposal],
        classified: [ClassifiedFolder],
        exempt: Set<String>
    ) -> [RestructureProposal] {
        let anchorFolders = Set(
            classified
                .filter { $0.classification == .anchor && !exempt.contains($0.sourceFolder) }
                .map { $0.sourceFolder })
        return moves.filter { m in
            let parent = (m.oldPath as NSString).deletingLastPathComponent
            return !anchorFolders.contains(parent)
        }
    }

    /// (year, month) from a Unix-seconds timestamp in UTC (byte-faithful with
    /// the Windows chrono `Utc` path). An out-of-range timestamp coerces to
    /// (1970, 1), so a file with no capture time still gets a deterministic
    /// year bucket instead of being silently omitted. (F-C3-020)
    static func yearMonth(_ unix: Double) -> (year: Int, month: Int) {
        guard unix.isFinite else { return (1970, 1) }
        let date = Date(timeIntervalSince1970: unix)
        let comps = utcCalendar.dateComponents([.year, .month], from: date)
        guard let y = comps.year, let m = comps.month else { return (1970, 1) }
        return (y, m)
    }

    private static let utcCalendar: Calendar = {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "UTC")!
        return c
    }()

    /// Decode a little-endian Float32 blob (the CLIP-embedding storage format,
    /// shared with the Windows engine) into `[Float]`. `loadUnaligned` reads
    /// each value in host order — correct on Apple's little-endian targets.
    private static func floatsLE(_ data: Data) -> [Float] {
        let count = data.count / 4
        return data.withUnsafeBytes { raw -> [Float] in
            var out = [Float](repeating: 0, count: count)
            for i in 0..<count {
                out[i] = raw.loadUnaligned(fromByteOffset: i * 4, as: Float32.self)
            }
            return out
        }
    }

    /// Apply the user-selected proposals on disk + update the DB. For each move:
    /// re-reads the live `files.path_text` and requires it to still name the
    /// proposal's `oldPath` (B4 stale-plan guard, F-C3-010); uniquifies a
    /// colliding destination to `name (n).ext` instead of skipping (F-C3-011);
    /// and on a move whose DB update fails, records a recovery sidecar and
    /// counts the move once — never double-counts moved+failed (F-C3-012).
    public struct ApplyResult: Sendable {
        public let moved: Int
        public let skipped: Int
        public let failed: Int
        public let conflicts: [String]
    }

    struct DestinationClaim: Hashable {
        let high: UInt64
        let low: UInt64

        init(_ path: String) {
            let digest = SHA256.hash(data: Data(path.lowercased().utf8))
            var high: UInt64 = 0
            var low: UInt64 = 0
            for (index, byte) in digest.prefix(16).enumerated() {
                if index < 8 { high = (high << 8) | UInt64(byte) }
                else { low = (low << 8) | UInt64(byte) }
            }
            self.high = high
            self.low = low
        }
    }

    private struct StoredPlanHeader: Codable {
        let version: Int
        let libraryRoot: String
        let totalMoves: Int
    }

    private final class NDJSONLineReader {
        private let handle: FileHandle
        private var buffer = Data()
        private var eof = false
        private static let chunkSize = 64 * 1024
        private static let maxLineSize = 1024 * 1024

        init(url: URL) throws { handle = try FileHandle(forReadingFrom: url) }
        deinit { try? handle.close() }

        func nextLine() throws -> Data? {
            while true {
                if let newline = buffer.firstIndex(of: 0x0A) {
                    let line = Data(buffer[..<newline])
                    buffer.removeSubrange(...newline)
                    return line
                }
                if eof {
                    guard !buffer.isEmpty else { return nil }
                    let line = buffer
                    buffer.removeAll(keepingCapacity: false)
                    return line
                }
                let chunk = try handle.read(upToCount: Self.chunkSize) ?? Data()
                if chunk.isEmpty { eof = true }
                else { buffer.append(chunk) }
                guard buffer.count <= Self.maxLineSize else {
                    throw CocoaError(.fileReadCorruptFile)
                }
            }
        }
    }

    static let storedPlanPreviewCap = 5_000
    private static let storedPlanVersion = 1

    private static var storedPlansDirectory: URL? {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first?
            .appendingPathComponent("FileID/restructure_plans", isDirectory: true)
    }

    static func storePlan(
        libraryRoot: String, moves: [RestructureMove], directory override: URL? = nil
    ) throws -> (planID: String, preview: [RestructureMove]) {
        var iterator = moves.makeIterator()
        return try storePlanStream(
            libraryRoot: libraryRoot,
            totalMoves: moves.count,
            directory: override,
            nextMove: { iterator.next() })
    }

    private static func storePlanStream(
        libraryRoot: String,
        totalMoves: Int,
        directory override: URL? = nil,
        nextMove: () throws -> RestructureMove?
    ) throws -> (planID: String, preview: [RestructureMove]) {
        guard let directory = override ?? storedPlansDirectory else {
            throw CocoaError(.fileNoSuchFile)
        }
        let fm = FileManager.default
        try fm.createDirectory(at: directory, withIntermediateDirectories: true)
        if let files = try? fm.contentsOfDirectory(
            at: directory, includingPropertiesForKeys: nil)
        {
            for file in files where file.pathExtension == "ndjson" || file.pathExtension == "tmp" {
                try? fm.removeItem(at: file)
            }
        }

        let planID = UUID().uuidString.lowercased()
        let finalURL = directory.appendingPathComponent("\(planID).ndjson")
        let temporaryURL = directory.appendingPathComponent("\(planID).tmp")
        guard fm.createFile(atPath: temporaryURL.path, contents: nil) else {
            throw CocoaError(.fileWriteUnknown)
        }
        let handle = try FileHandle(forWritingTo: temporaryURL)
        let encoder = JSONEncoder()
        var preview: [RestructureMove] = []
        preview.reserveCapacity(min(storedPlanPreviewCap, totalMoves))
        var written = 0
        do {
            func append<T: Encodable>(_ value: T) throws {
                var data = try encoder.encode(value)
                data.append(0x0A)
                try handle.write(contentsOf: data)
            }
            try append(StoredPlanHeader(
                version: storedPlanVersion,
                libraryRoot: libraryRoot,
                totalMoves: totalMoves))
            while let move = try nextMove() {
                if preview.count < storedPlanPreviewCap { preview.append(move) }
                try append(move)
                written += 1
            }
            guard written == totalMoves else { throw CocoaError(.fileWriteUnknown) }
            try handle.synchronize()
            try handle.close()
            try fm.moveItem(at: temporaryURL, to: finalURL)
        } catch {
            try? handle.close()
            try? fm.removeItem(at: temporaryURL)
            throw error
        }
        return (planID, preview)
    }

    static func applyStoredPlan(
        planID: String,
        expectedRoot: String,
        database: Database,
        libraryRoot: URL,
        isCancelled: @Sendable () -> Bool = { Task.isCancelled },
        directory override: URL? = nil
    ) async throws -> ApplyResult {
        guard let uuid = UUID(uuidString: planID),
              let directory = override ?? storedPlansDirectory else {
            throw CocoaError(.fileReadInvalidFileName)
        }
        let canonicalID = uuid.uuidString.lowercased()
        let url = directory.appendingPathComponent("\(canonicalID).ndjson")
        let reader = try NDJSONLineReader(url: url)
        let decoder = JSONDecoder()
        guard let headerData = try reader.nextLine() else {
            throw CocoaError(.fileReadCorruptFile)
        }
        let header = try decoder.decode(StoredPlanHeader.self, from: headerData)
        guard header.version == storedPlanVersion,
              pathsEqual(header.libraryRoot, expectedRoot) else {
            throw CocoaError(.fileReadCorruptFile)
        }
        return await applyStream(
            total: header.totalMoves,
            nextProposal: {
                guard let line = try reader.nextLine() else { return nil }
                let move = try decoder.decode(RestructureMove.self, from: line)
                return RestructureProposal(
                    fileID: move.fileID, oldPath: move.source,
                    newPath: move.destination, bucket: move.category,
                    confidence: move.confidence, reason: move.reason)
            },
            database: database,
            libraryRoot: libraryRoot,
            isCancelled: isCancelled,
            undoJournal: nil,
            recordUndo: true)
    }

    public static func apply(
        proposals: [RestructureProposal],
        database: Database,
        libraryRoot: URL,
        isCancelled: @Sendable () -> Bool = { Task.isCancelled },
        undoJournal: URL? = nil,
        recordUndo: Bool = true
    ) async -> ApplyResult {
        var iterator = proposals.makeIterator()
        return await applyStream(
            total: proposals.count,
            nextProposal: { iterator.next() },
            database: database,
            libraryRoot: libraryRoot,
            isCancelled: isCancelled,
            undoJournal: undoJournal,
            recordUndo: recordUndo)
    }

    private static func applyStream(
        total: Int,
        nextProposal: () throws -> RestructureProposal?,
        database: Database,
        libraryRoot: URL,
        isCancelled: @Sendable () -> Bool,
        undoJournal: URL?,
        recordUndo: Bool
    ) async -> ApplyResult {
        let fm = FileManager.default
        let journalURL = undoJournal ?? Self.defaultUndoJournalURL
        // Inverse of every successful move (current → original), appended to the
        // undo journal AS IT HAPPENS so "Undo last run" can reverse this batch — and
        // so a crash mid-apply still leaves every COMPLETED move undoable (the prior
        // design buffered in memory and wrote once after the loop, losing the whole
        // batch's undo on a crash). nil disables undo, best-effort as before.
        // (R2 → crash-safe)
        let undoHandle: FileHandle? = recordUndo
            ? Self.openUndoJournalTruncating(at: journalURL) : nil
        defer { try? undoHandle?.close() }
        var undoCount = 0
        // (source, final destination) of every successful move, fed to the
        // learn-from-corrections memory in ONE write after the loop so a future plan
        // can boost a move toward a folder the user has filed here before. Populated
        // alongside the undo journal, so it is forward-applies-only (stays empty on an
        // undo run, recordUndo=false). (R3 → learn-your-style)
        var appliedPairs: [(source: String, destination: String)] = []
        var moved = 0
        var skipped = 0
        var failed = 0
        // Destinations whose planned basename already existed and were resolved to
        // a ` (2)`-style sibling by uniqueDestination — surfaced so the app can
        // tell the user which moves were renamed rather than placed verbatim.
        // Was a dead `let [] ` despite the uniquify path below. (audit F-A4)
        var conflicts: [String] = []
        let resolvedRoot = libraryRoot.resolvingSymlinksInPath().path
        // B3: destinations claimed by an earlier move in THIS batch, so two
        // distinct sources mapping to the same basename don't collide before
        // either touches disk.
        var claimed = Set<DestinationClaim>()
        // F-C6-013: the apply loop was a silent, unstoppable serial walk — at
        // 100k+ moves the user got no feedback and no stop. Poll the cancel
        // signal at the TOP of every iteration (each completed move is already
        // durable, so stopping BETWEEN moves preserves per-move atomicity) and
        // emit throttled progress to the engine log so the run isn't feedbackless.
        var processed = 0

        while true {
            let p: RestructureProposal
            do {
                guard let next = try nextProposal() else { break }
                p = next
            } catch {
                failed += 1
                JSONLog.shared.warn(ev: "restructure_plan_read_failed", error: "\(error)")
                break
            }
            if isCancelled() {
                JSONLog.shared.info(ev: "restructure_apply_cancelled",
                                    extra: ["processed": AnyCodable(processed),
                                            "total": AnyCodable(total),
                                            "moved": AnyCodable(moved),
                                            "skipped": AnyCodable(skipped),
                                            "failed": AnyCodable(failed)])
                break
            }
            processed += 1
            if Self.shouldEmitApplyProgress(processed: processed, total: total,
                                            interval: Self.applyProgressInterval) {
                JSONLog.shared.info(ev: "restructure_apply_progress",
                                    extra: ["processed": AnyCodable(processed),
                                            "total": AnyCodable(total),
                                            "moved": AnyCodable(moved),
                                            "skipped": AnyCodable(skipped),
                                            "failed": AnyCodable(failed)])
            }
            let oldURL = URL(fileURLWithPath: p.oldPath)
            let plannedURL = URL(fileURLWithPath: p.newPath)

            // B4 stale-plan / identity guard: the payload `oldPath` is not
            // authoritative on its own. Re-read the live row for this fileID and
            // require it still names `oldPath`, so a plan that went stale (the
            // file was renamed/moved/replaced since planning) can't move the
            // wrong bytes. (F-C3-010)
            let live: (path: String, fileRef: Int64?)? =
                try? await database.pool.read { db -> (path: String, fileRef: Int64?)? in
                    guard let row = try GRDB.Row.fetchOne(
                        db, sql: "SELECT path_text, file_ref FROM files WHERE id = ?",
                        arguments: [p.fileID]) else { return nil }
                    return (row["path_text"], row["file_ref"])
                }
            guard let live, Self.pathsEqual(live.path, p.oldPath) else {
                // Undo journals are deliberately retained after cancellation or
                // a partial failure. A retry sees already-restored entries first;
                // treat a live DB row + file at the original destination as an
                // idempotent skip so the remaining entries can finish and the
                // journal can finally be cleared.
                if !recordUndo,
                   let live,
                   Self.pathsEqual(live.path, p.newPath),
                   FileManager.default.fileExists(atPath: p.newPath),
                   !Self.fileRefSwapped(
                       dbRef: live.fileRef,
                       currentRef: Discovery.inode(of: URL(fileURLWithPath: p.newPath))) {
                    skipped += 1
                    continue
                }
                failed += 1
                JSONLog.shared.warn(ev: "restructure_stale_plan",
                                    path: redactPathForLog(p.oldPath))
                continue
            }
            // R-#14 same-path SWAP guard: the path check above only proves the DB row
            // still NAMES this source — not that the file currently AT that path is the
            // one we planned to move. If a different file was dropped at the same path in
            // the plan→apply window (a sync client re-downloading, an app re-saving),
            // moving it would relocate the wrong bytes and stamp this fileID onto an
            // unrelated file. Compare the planned file's stored file_ref (inode) to the
            // one on disk now; skip on a positive mismatch. Conservative — a NULL stored
            // ref or an unreadable inode leaves the move to proceed (no false skips).
            // Mirrors the Windows engine's file_ref_swapped guard. (R-#14)
            if Self.fileRefSwapped(dbRef: live.fileRef, currentRef: Discovery.inode(of: oldURL)) {
                failed += 1
                JSONLog.shared.warn(ev: "restructure_swapped_source",
                                    path: redactPathForLog(p.oldPath))
                continue
            }

            // No-op (file already sits at its PLANNED destination) — skip BEFORE
            // uniquifying, else unique_destination would see the file itself
            // occupying the slot and bump it to a ` (2)` sibling, churning an
            // already-correctly-placed file. (ENG-42, F-C3-011)
            if oldURL == plannedURL { skipped += 1; continue }

            // SEC-7 port: the destination's resolved parent must stay inside
            // the resolved library root — a symlinked bucket component must
            // not let a move escape the tree the user authorized.
            guard pathIsContained(plannedURL.deletingLastPathComponent(),
                                  inResolvedRoot: resolvedRoot) else {
                failed += 1
                JSONLog.shared.warn(ev: "restructure_move_escapes_root",
                                    path: redactPathForLog(p.newPath))
                continue
            }
            do {
                try fm.createDirectory(at: plannedURL.deletingLastPathComponent(),
                                       withIntermediateDirectories: true)
            } catch {
                failed += 1
                // Mirror the moveItem/DB-update sites: a swallowed mkdir failure
                // left a `failed` count with no breadcrumb. Domain+code only — the
                // NSError text embeds the full path. (audit F-A5)
                let ns = error as NSError
                JSONLog.shared.warn(ev: "restructure_mkdir_failed",
                                    path: redactPathForLog(
                                        plannedURL.deletingLastPathComponent().path),
                                    error: "\(ns.domain) \(ns.code)")
                continue
            }
            // SEC-5 port: re-verify after createDirectory (an attacker can
            // plant a symlink between check and use; cheap defense in depth).
            guard pathIsContained(plannedURL.deletingLastPathComponent(),
                                  inResolvedRoot: resolvedRoot) else {
                failed += 1
                JSONLog.shared.warn(ev: "restructure_move_escapes_root",
                                    path: redactPathForLog(p.newPath))
                continue
            }

            // B3: never clobber. Resolve a collision-free name within the SAME
            // parent (so the containment checks above still hold), claim it, and
            // move there. moveItem never overwrites, so a remaining collision
            // fails safe rather than destroying data. (F-C3-011)
            let finalURL = Self.uniqueDestination(plannedURL, claimed: claimed, fm: fm)
            // Store case-folded: APFS (and NTFS) are case-INSENSITIVE by default, so
            // "Photo.jpg" and "photo.jpg" are the SAME on-disk slot. A case-sensitive
            // claim would miss that collision and let the second move overwrite the
            // first (data loss). Erring toward uniquify is safe even on case-sensitive
            // volumes — byte-faithful with the Windows restructure_apply fix.
            claimed.insert(DestinationClaim(finalURL.path))

            do {
                try fm.moveItem(at: oldURL, to: finalURL)
            } catch {
                failed += 1
                // NSError text embeds both full paths — log domain+code only.
                let ns = error as NSError
                JSONLog.shared.warn(ev: "restructure_move_failed",
                                    path: redactPathForLog(oldURL.path),
                                    error: "\(ns.domain) \(ns.code)")
                continue
            }
            // The file is now relocated — count it once. A DB-update failure does
            // NOT also count it failed (no double-count); it's recorded for
            // recovery (and self-heals on the next scan). (F-C3-012)
            moved += 1
            // Record the inverse (final → original) for undo, durably. Captured after
            // the on-disk move succeeded but BEFORE (and regardless of) the DB update
            // below, so undo can always move the bytes back — appended + periodically
            // fsync'd so a crash on a later move can't lose this one's undoability.
            // (R2 → crash-safe)
            if let h = undoHandle {
                Self.appendUndoEntry(
                    UndoEntry(fileID: p.fileID, from: finalURL.path, to: oldURL.path), to: h)
                undoCount += 1
                if undoCount % Self.applyProgressInterval == 0 { try? h.synchronize() }
            }
            if recordUndo {
                // Credit every successful move to the feedback memory regardless
                // of whether the undo journal opened (disk-full / sandbox failure).
                appliedPairs.append((source: oldURL.path, destination: finalURL.path))
                if appliedPairs.count >= Self.applyProgressInterval {
                    await RestructureFeedback.record(
                        database: database, moves: appliedPairs,
                        now: Date().timeIntervalSince1970)
                    appliedPairs.removeAll(keepingCapacity: true)
                }
            }
            if finalURL.path != plannedURL.path, conflicts.count < 1_000 {
                conflicts.append(plannedURL.path)
            }
            do {
                let finalPath = finalURL.path
                // ENG-91: refresh path_hash too (notNull, indexed StablePathHash
                // column) so cross-run/cross-platform path identity stays valid —
                // a move that touched only path_text/path_search left it stale.
                // (F-C3-009)
                let pathHash = StablePathHash.hash(finalPath)
                try await database.pool.write { db in
                    try db.execute(
                        sql: "UPDATE files SET path_text = ?, path_hash = ?, path_search = ? WHERE id = ?",
                        arguments: [finalPath, pathHash,
                                    finalPath.precomposedStringWithCanonicalMapping,
                                    p.fileID])
                }
            } catch {
                let ns = error as NSError
                JSONLog.shared.error(ev: "restructure_db_update_failed_after_move",
                                     path: redactPathForLog(finalURL.path),
                                     error: "\(ns.domain) \(ns.code)")
                Self.recordPathUpdateFailure(
                    fileID: p.fileID, src: oldURL.path, dst: finalURL.path)
            }
        }
        // Final durability barrier — fsync the journal so it's complete on a clean
        // finish. (nil during an undo run, recordUndo=false, so a CANCELLED undo
        // leaves the ORIGINAL journal intact and the user can re-run it.)
        // (R2 → crash-safe)
        try? undoHandle?.synchronize()
        // Learn-from-corrections: each applied move is an approved example, so credit
        // its filename tokens toward its destination folder for future plans. One write
        // for the whole batch; best-effort, never fails an apply. Forward applies only
        // — `appliedPairs` is empty on an undo run (recordUndo=false). Mirrors the
        // Windows engine (restructure_apply.rs).
        if recordUndo && !appliedPairs.isEmpty {
            await RestructureFeedback.record(
                database: database, moves: appliedPairs, now: Date().timeIntervalSince1970)
        }
        JSONLog.shared.info(ev: "restructure_applied",
                            extra: ["moved": AnyCodable(moved),
                                    "skipped": AnyCodable(skipped),
                                    "failed": AnyCodable(failed)])
        return ApplyResult(moved: moved, skipped: skipped, failed: failed, conflicts: conflicts)
    }

    // MARK: - Undo (R2 — reversible "Undo last run")

    /// One reversal: move the file currently at `from` back to `to`.
    struct UndoEntry: Codable, Sendable {
        let fileID: Int64
        let from: String
        let to: String

        enum CodingKeys: String, CodingKey {
            case fileID = "file_id"
            case from, to
        }
    }

    /// `~/Library/Application Support/FileID/restructure_undo.ndjson` — the last
    /// apply run's inverse moves. nil only if Application Support is unresolvable
    /// (then undo is silently unavailable).
    static var defaultUndoJournalURL: URL? {
        FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first?
            .appendingPathComponent("FileID/restructure_undo.ndjson")
    }

    /// Open the undo journal truncating (fresh batch) for incremental append, so the
    /// journal is durable as each move completes rather than written once after the
    /// loop. nil disables undo (best-effort). "Last run only" semantics are now
    /// established at the START of the batch (truncate) instead of the end.
    /// (R2 → crash-safe)
    static func openUndoJournalTruncating(at url: URL?) -> FileHandle? {
        guard let url else { return nil }
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        // createFile truncates any prior run's journal to empty; the handle then
        // opens at offset 0 (= end of the empty file), so writes append in order.
        FileManager.default.createFile(atPath: url.path, contents: nil)
        return try? FileHandle(forWritingTo: url)
    }

    /// Append one inverse-move entry (NDJSON) to the open journal.
    static func appendUndoEntry(_ e: UndoEntry, to handle: FileHandle) {
        let obj: [String: Any] = ["file_id": e.fileID, "from": e.from, "to": e.to]
        guard var line = try? JSONSerialization.data(withJSONObject: obj) else { return }
        line.append(0x0A)
        try? handle.write(contentsOf: line)
    }

    private static func undoEntryCount(from url: URL) throws -> Int {
        let reader = try NDJSONLineReader(url: url)
        let decoder = JSONDecoder()
        var count = 0
        while let line = try reader.nextLine() {
            _ = try decoder.decode(UndoEntry.self, from: line)
            count += 1
        }
        return count
    }

    static func clearUndoJournal(_ url: URL?) {
        guard let url else { return }
        try? FileManager.default.removeItem(at: url)
    }

    /// Remove the empty group folders an apply created, after its undo restored the
    /// files. Removes a dir ONLY when it has no entries (we never delete a non-empty
    /// folder), stays strictly inside the resolved root, and never touches the root.
    /// Deepest-first so nested empties fully collapse. Best-effort.
    /// (R2 → reversibility completeness)
    private static func cleanupEmptyDirs(from journal: URL, root: URL) {
        guard let reader = try? NDJSONLineReader(url: journal) else { return }
        let decoder = JSONDecoder()
        let fm = FileManager.default
        let resolvedRoot = root.resolvingSymlinksInPath().standardizedFileURL.path
        // The replay has finished, so each row can attempt its parent immediately.
        // Repeated folders are harmless and keep memory constant for huge journals.
        while let line = try? reader.nextLine() {
            guard let entry = try? decoder.decode(UndoEntry.self, from: line) else { continue }
            var cur = URL(fileURLWithPath: entry.from).deletingLastPathComponent()
            while true {
                let curPath = cur.resolvingSymlinksInPath().standardizedFileURL.path
                guard curPath != resolvedRoot, curPath.hasPrefix(resolvedRoot + "/") else { break }
                let contents = (try? fm.contentsOfDirectory(atPath: cur.path)) ?? ["x"]
                guard contents.isEmpty else { break }
                if (try? fm.removeItem(at: cur)) == nil { break }
                cur = cur.deletingLastPathComponent()
            }
        }
    }

    /// True when the last apply left a reversible journal — drives the app's
    /// "Undo last run" affordance.
    public static func hasUndoableRun(undoJournal: URL? = nil) -> Bool {
        guard let url = undoJournal ?? defaultUndoJournalURL,
              let reader = try? NDJSONLineReader(url: url),
              let line = try? reader.nextLine() else { return false }
        return (try? JSONDecoder().decode(UndoEntry.self, from: line)) != nil
    }

    /// Undo the most recent `apply`: move every file the last run relocated back to
    /// where it came from, replaying the inverse moves through `apply` itself (so
    /// the identical stale-check / containment / no-clobber / DB-update safety
    /// applies), then clear the journal so a run can't be undone twice.
    /// (RESTRUCTURE.md §6 reversibility)
    public static func undoLast(
        database: Database,
        libraryRoot: URL,
        isCancelled: @Sendable () -> Bool = { Task.isCancelled },
        undoJournal: URL? = nil
    ) async -> ApplyResult {
        let journalURL = undoJournal ?? Self.defaultUndoJournalURL
        guard let journalURL else {
            return ApplyResult(moved: 0, skipped: 0, failed: 0, conflicts: [])
        }
        guard FileManager.default.fileExists(atPath: journalURL.path) else {
            return ApplyResult(moved: 0, skipped: 0, failed: 0, conflicts: [])
        }
        let total: Int
        do {
            total = try undoEntryCount(from: journalURL)
        } catch {
            JSONLog.shared.warn(ev: "restructure_undo_journal_invalid", error: "\(error)")
            return ApplyResult(moved: 0, skipped: 0, failed: 1, conflicts: [])
        }
        guard total > 0 else {
            return ApplyResult(moved: 0, skipped: 0, failed: 0, conflicts: [])
        }
        let reader: NDJSONLineReader
        do {
            reader = try NDJSONLineReader(url: journalURL)
        } catch {
            return ApplyResult(moved: 0, skipped: 0, failed: 1, conflicts: [])
        }
        let decoder = JSONDecoder()
        // recordUndo:false so the undo's own moves DON'T overwrite the journal — a
        // cancelled undo must leave the original intact so the user can re-run it
        // and put the REMAINING files back (the already-restored ones stale-skip on
        // the retry). Only a fully-completed (non-cancelled) undo clears it, so the
        // button can't toggle apply→undo→apply by accident.
        let result = await applyStream(
            total: total,
            nextProposal: {
                guard let line = try reader.nextLine() else { return nil }
                let entry = try decoder.decode(UndoEntry.self, from: line)
                return RestructureProposal(
                    fileID: entry.fileID, oldPath: entry.from,
                    newPath: entry.to, bucket: "")
            },
            database: database, libraryRoot: libraryRoot,
            isCancelled: isCancelled, undoJournal: journalURL,
            recordUndo: false)
        if !isCancelled(), result.failed == 0 {
            // Reversibility completeness: remove the orphan empty group folders the
            // apply created, now that undo emptied them. Stream the journal again
            // before deleting it so cleanup also stays bounded.
            Self.cleanupEmptyDirs(from: journalURL, root: libraryRoot)
            clearUndoJournal(journalURL)
        }
        return result
    }

    /// Apply-progress throttle: log on the first move, on the last, and once per
    /// `interval` processed moves, so a 100k-move apply emits ~total/interval log
    /// lines instead of none (silent) or one-per-move (flood). Pure → the cadence
    /// is unit-assertable. (F-C6-013)
    static let applyProgressInterval = 500
    static func shouldEmitApplyProgress(processed: Int, total: Int, interval: Int) -> Bool {
        guard interval > 0, processed > 0 else { return false }
        return processed == 1 || processed == total || processed % interval == 0
    }

    /// B3: resolve a destination that collides with neither an on-disk entry nor
    /// a destination already claimed this batch, by appending ` (2)`, ` (3)`, …
    /// before the extension — within the same parent so the containment checks
    /// already performed on `dest` still hold. Occupancy is the in-batch claimed
    /// set ∪ an `lstat` (does not follow the final symlink, so a broken symlink
    /// occupying the slot is still detected). (F-C3-011)
    static func uniqueDestination(
        _ dest: URL, claimed: Set<DestinationClaim>, fm: FileManager
    ) -> URL {
        func occupied(_ url: URL) -> Bool {
            // Case-folded membership: the claimed set is stored lowercased so a
            // case-only collision (APFS/NTFS are case-insensitive) is still detected.
            claimed.contains(DestinationClaim(url.path))
                || (try? fm.attributesOfItem(atPath: url.path)) != nil
        }
        if !occupied(dest) { return dest }
        let parent = dest.deletingLastPathComponent()
        let ext = dest.pathExtension
        let stem = dest.deletingPathExtension().lastPathComponent
        for n in 2...9999 {
            let name = ext.isEmpty ? "\(stem) (\(n))" : "\(stem) (\(n)).\(ext)"
            let candidate = parent.appendingPathComponent(name)
            if !occupied(candidate) { return candidate }
        }
        // Exhausted — return the original; the no-overwrite move then fails safely.
        return dest
    }

    /// Path equality tolerant of separator/symlink differences. Fast path is a
    /// string compare (the normal case — both came from the same row at plan
    /// time); otherwise compare resolved forms. (B4 helper, F-C3-010)
    static func pathsEqual(_ a: String, _ b: String) -> Bool {
        if a == b { return true }
        return URL(fileURLWithPath: a).resolvingSymlinksInPath().path
            == URL(fileURLWithPath: b).resolvingSymlinksInPath().path
    }

    /// R-#14 positive-evidence swap detector — mirrors the Windows engine's
    /// `file_ref_swapped`. True ONLY when both the DB's stored file_ref and the on-disk
    /// inode are known AND differ — a different file now occupies the planned source
    /// path. Any missing input (NULL stored ref; an unreadable inode) returns false so a
    /// legitimate move is never wrongly skipped. The stored ref is read back
    /// `Int64 → UInt64(bitPattern:)` to undo DBWriter's `Int64(bitPattern:)` cast.
    /// (APFS/HFS st_ino can false-MATCH on inode reuse — rare — which only ever fails
    /// OPEN, never closed; the Windows NTFS file_ref's sequence number has no such gap.)
    static func fileRefSwapped(dbRef: Int64?, currentRef: UInt64?) -> Bool {
        guard let dbRef, let currentRef else { return false }
        return UInt64(bitPattern: dbRef) != currentRef
    }

    /// B5: best-effort durable record of a successful on-disk move whose DB
    /// path-update failed, so the stale `path_text` is recoverable even if the
    /// next scan (which self-heals the row) never runs. NDJSON, append-only;
    /// written beside the engine log. (F-C3-012)
    static func recordPathUpdateFailure(fileID: Int64, src: String, dst: String) {
        guard let dir = FileManager.default
            .urls(for: .applicationSupportDirectory, in: .userDomainMask).first?
            .appendingPathComponent("FileID/logs", isDirectory: true) else { return }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent("restructure_recover.ndjson")
        let obj: [String: Any] = ["file_id": fileID, "src": src, "dst": dst]
        guard var line = try? JSONSerialization.data(withJSONObject: obj) else { return }
        line.append(0x0A)
        if !FileManager.default.fileExists(atPath: url.path) {
            FileManager.default.createFile(atPath: url.path, contents: nil)
        }
        guard let handle = try? FileHandle(forWritingTo: url) else { return }
        defer { try? handle.close() }
        _ = try? handle.seekToEnd()
        try? handle.write(contentsOf: line)
        try? handle.synchronize()
    }

    /// Full English month name (Windows is canonical for this cosmetic parity;
    /// macOS converged from "01-Jan".."12-Dec"). (F-C3-018)
    static func monthName(_ m: Int) -> String {
        let names = ["", "January", "February", "March", "April", "May", "June",
                     "July", "August", "September", "October", "November", "December"]
        return names[max(1, min(12, m))]
    }
}
