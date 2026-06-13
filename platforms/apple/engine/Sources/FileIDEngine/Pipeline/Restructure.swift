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
//   6. Audio        → Audio/                      (category "audio")
//   7. Fallback     → Misc/                       (category "misc")
//
// `vlm_proposed_name` becomes the new filename within whichever
// folder the heuristic picks. A missing timestamp coerces to 1970.
import Foundation
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

    /// Build proposals for every image in the library. The caller (UI)
    /// renders, the user filters/checks, then apply runs the moves.
    public static func proposeAll(
        database: Database,
        libraryRoot: URL
    ) async throws -> [RestructureProposal] {
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
            db -> (rows: [Source], embeddings: [Int64: [Float]], tags: [Int64: [String]]) in
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
                """)
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
            let erows = try GRDB.Row.fetchAll(db, sql: """
                SELECT ce.file_id, ce.embedding FROM clip_embeddings ce
                JOIN files f ON f.id = ce.file_id
                WHERE f.failed = 0 AND f.kind = 'image'
                """)
            for row in erows {
                let id: Int64 = row["file_id"] ?? 0
                if let data: Data = row["embedding"], !data.isEmpty, data.count % 4 == 0 {
                    embeddings[id] = Self.floatsLE(data)
                }
            }
            // Content tags for distinctive-term naming + fusion.
            var tags: [Int64: [String]] = [:]
            let trows = try GRDB.Row.fetchAll(
                db, sql: "SELECT file_id, tag FROM tags WHERE source IN ('auto','vlm','user')")
            for row in trows {
                let id: Int64 = row["file_id"] ?? 0
                if let t: String = row["tag"] { tags[id, default: []].append(t) }
            }
            return (rows, embeddings, tags)
        }
        let rows = loaded.rows

        // Butler P1: semantic + learn-your-style placement for image files that
        // have a CLIP embedding; everything else (and density noise) falls back
        // to the rule cascade. Mirrors the Windows engine (commands/restructure.rs).
        let semanticFiles: [RestructureSemantic.SemanticFile] = rows.compactMap { s in
            guard s.kind == "image", let clip = loaded.embeddings[s.id] else { return nil }
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

        // Engine-authoritative folder classification on the FULL proposal set
        // (Windows A1/A3): classify each source folder, then strip every move out
        // of an Anchor folder so files the UI promised would "stay put" are never
        // silently relocated. Semantic-claimed folders are exempt — their
        // homogeneity is a real relocation, not an in-place anchor. (F-C3-016)
        let folderClass = classifyFolders(proposals)
        return stripAnchorFolderMovesExcept(
            proposals, classified: folderClass, exempt: semanticSourceFolders)
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
    ///   6. Audio          → Audio/                     (category "audio")
    ///   7. Fallback       → Misc/                      (category "misc")
    /// A missing timestamp coerces to 1970 (Windows year_month). (F-C3-017..020)
    public static func ruleClassify(
        _ files: [FileForClassify], libraryRoot: URL
    ) -> [RestructureProposal] {
        var out: [RestructureProposal] = []
        out.reserveCapacity(files.count)
        for f in files {
            let ts = f.createdUnix ?? f.modifiedUnix
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
                dir = libraryRoot.appendingPathComponent("Documents", isDirectory: true)
                    .appendingPathComponent("\(y)", isDirectory: true)
                category = "document"
                confidence = "review"
                reason = "Document from \(y)"
            } else if f.kind == "image" {
                dir = libraryRoot.appendingPathComponent("Photos", isDirectory: true)
                    .appendingPathComponent("\(y)", isDirectory: true)
                    .appendingPathComponent(mname, isDirectory: true)
                category = "photo"
                confidence = "review"
                reason = "Photo from \(mname) \(y)"
            } else if f.kind == "video" {
                dir = libraryRoot.appendingPathComponent("Videos", isDirectory: true)
                    .appendingPathComponent("\(y)", isDirectory: true)
                category = "video"
                confidence = "review"
                reason = "Video from \(y)"
            } else if f.kind == "audio" {
                dir = libraryRoot.appendingPathComponent("Audio", isDirectory: true)
                category = "audio"
                confidence = "review"
                reason = "Audio file"
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

    public static func apply(
        proposals: [RestructureProposal],
        database: Database,
        libraryRoot: URL
    ) async -> ApplyResult {
        let fm = FileManager.default
        var moved = 0
        var skipped = 0
        var failed = 0
        let conflicts: [String] = []
        let resolvedRoot = libraryRoot.resolvingSymlinksInPath().path
        // B3: destinations claimed by an earlier move in THIS batch, so two
        // distinct sources mapping to the same basename don't collide before
        // either touches disk.
        var claimed = Set<String>()

        for p in proposals {
            let oldURL = URL(fileURLWithPath: p.oldPath)
            let plannedURL = URL(fileURLWithPath: p.newPath)

            // B4 stale-plan / identity guard: the payload `oldPath` is not
            // authoritative on its own. Re-read the live row for this fileID and
            // require it still names `oldPath`, so a plan that went stale (the
            // file was renamed/moved/replaced since planning) can't move the
            // wrong bytes. (F-C3-010)
            let live: String? = try? await database.pool.read { db in
                try String.fetchOne(
                    db, sql: "SELECT path_text FROM files WHERE id = ?", arguments: [p.fileID])
            }
            guard let livePath = live, Self.pathsEqual(livePath, p.oldPath) else {
                failed += 1
                JSONLog.shared.warn(ev: "restructure_stale_plan",
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
                failed += 1; continue
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
            claimed.insert(finalURL.path)

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
        JSONLog.shared.info(ev: "restructure_applied",
                            extra: ["moved": AnyCodable(moved),
                                    "skipped": AnyCodable(skipped),
                                    "failed": AnyCodable(failed)])
        return ApplyResult(moved: moved, skipped: skipped, failed: failed, conflicts: conflicts)
    }

    /// B3: resolve a destination that collides with neither an on-disk entry nor
    /// a destination already claimed this batch, by appending ` (2)`, ` (3)`, …
    /// before the extension — within the same parent so the containment checks
    /// already performed on `dest` still hold. Occupancy is the in-batch claimed
    /// set ∪ an `lstat` (does not follow the final symlink, so a broken symlink
    /// occupying the slot is still detected). (F-C3-011)
    static func uniqueDestination(
        _ dest: URL, claimed: Set<String>, fm: FileManager
    ) -> URL {
        func occupied(_ url: URL) -> Bool {
            claimed.contains(url.path) || (try? fm.attributesOfItem(atPath: url.path)) != nil
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
