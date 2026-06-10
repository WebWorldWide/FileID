// Proposed folder-hierarchy generator. Reads each file's metadata
// (faces, GPS, date, VLM caption) and produces (old_path, new_path)
// pairs the UI renders as a diff. Rule-based by design — at 50K
// files the LLM cost would dominate, and a deterministic layout is
// what the user can trust.
//
// Priority, first match wins:
//   1. Named person → People/<Name>/<Year>/
//   2. GPS location → Places/<lat,lon-bucketed>/<Year>/
//   3. Document     → Documents/<Year>/
//   4. Year-month   → <Year>/<Month>/
//
// `vlm_proposed_name` becomes the new filename within whichever
// folder the heuristic picks.
import Foundation
import GRDB
import FileIDShared

public struct RestructureProposal: Sendable {
    public let fileID: Int64
    public let oldPath: String
    public let newPath: String
    public let bucket: String        // "People/Mom", "Places/...", etc — used to group in UI
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
            }
        }

        let calendar = Calendar(identifier: .gregorian)
        for s in rows where !movedIDs.contains(s.id) {
            let date: Date? = {
                if let c = s.createdAt { return Date(timeIntervalSince1970: c) }
                if let m = s.modifiedAt { return Date(timeIntervalSince1970: m) }
                return nil
            }()
            let year = date.map { String(calendar.component(.year, from: $0)) }
            let month = date.map { Self.monthName(calendar.component(.month, from: $0)) }

            // Pick a bucket + its confidence band + plain-language reason
            // (mirrors restructure.rs: a named person is a deterministic
            // auto-file; misc has no signal so it's held for the user).
            let bucket: String
            let confidence: String
            let reason: String
            if let names = s.personNames, !names.isEmpty {
                let first = names
                    .split(separator: "\u{1F}")
                    .first
                    .map { String($0).trimmingCharacters(in: .whitespaces) }
                    ?? "Unknown"
                let safeFirst = FilesystemNameSafe.componentSafe(first.isEmpty ? "Unknown" : first)
                bucket = "People/\(safeFirst)"
                confidence = "auto"
                reason = "Named person: \(safeFirst)"
            } else if let lat = s.lat, let lon = s.lon {
                let latB = (lat * 2).rounded() / 2
                let lonB = (lon * 2).rounded() / 2
                bucket = String(format: "Places/%.1f_%.1f", latB, lonB)
                confidence = "review"
                reason = "Taken at a shared location"
            } else if s.hasText != 0 || s.kind == "pdf" || s.kind == "doc" {
                bucket = "Documents"
                confidence = "review"
                reason = year.map { "Document from \($0)" } ?? "Document"
            } else if let y = year {
                bucket = "Photos/\(y)" + (month.map { "/\($0)" } ?? "")
                confidence = "review"
                reason = "Photo from \(month ?? y)"
            } else {
                bucket = "Misc"
                confidence = "ask"
                reason = "No strong signal — left for you to decide"
            }

            // Filename: keep original or use the VLM suggestion. The VLM name is
            // already slug-sanitized; the extension is sanitized here in case
            // the source filename was malformed.
            let oldURL = URL(fileURLWithPath: s.path)
            let ext = FilesystemNameSafe.componentSafe(oldURL.pathExtension, maxLength: 16)
            let newName: String
            if let p = s.vlmProposed, !p.isEmpty {
                newName = ext.isEmpty || ext == "_" ? p : "\(p).\(ext)"
            } else {
                newName = FilesystemNameSafe.componentSafe(oldURL.lastPathComponent)
            }
            var target = libraryRoot.appendingPathComponent(bucket, isDirectory: true)
            if let y = year, !bucket.contains(y) {
                target = target.appendingPathComponent(y, isDirectory: true)
            }
            target = target.appendingPathComponent(newName)
            proposals.append(RestructureProposal(
                fileID: s.id, oldPath: s.path, newPath: target.path,
                bucket: bucket, confidence: confidence, reason: reason))
        }
        return proposals
    }

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

    /// Apply the user-selected proposals. Performs `FileManager.moveItem`
    /// for each pair, creating intermediate directories as needed.
    /// Updates files.path_text in the DB on success. Skips moves where
    /// the destination already exists (returns those in `conflicts`).
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
        var conflicts: [String] = []
        let resolvedRoot = libraryRoot.resolvingSymlinksInPath().path

        for p in proposals {
            let oldURL = URL(fileURLWithPath: p.oldPath)
            let newURL = URL(fileURLWithPath: p.newPath)
            if oldURL == newURL { skipped += 1; continue }
            // SEC-7 port: the destination's resolved parent must stay inside
            // the resolved library root — a symlinked bucket component must
            // not let a move escape the tree the user authorized.
            guard pathIsContained(newURL.deletingLastPathComponent(),
                                  inResolvedRoot: resolvedRoot) else {
                failed += 1
                JSONLog.shared.warn(ev: "restructure_move_escapes_root",
                                    path: redactPathForLog(p.newPath))
                continue
            }
            do {
                try fm.createDirectory(at: newURL.deletingLastPathComponent(),
                                       withIntermediateDirectories: true)
            } catch {
                failed += 1; continue
            }
            // SEC-5 port: re-verify after createDirectory (an attacker can
            // plant a symlink between check and use; cheap defense in depth).
            guard pathIsContained(newURL.deletingLastPathComponent(),
                                  inResolvedRoot: resolvedRoot) else {
                failed += 1
                JSONLog.shared.warn(ev: "restructure_move_escapes_root",
                                    path: redactPathForLog(p.newPath))
                continue
            }
            // Advisory collision report — the enforcement is moveItem itself,
            // which never overwrites (throws NSFileWriteFileExistsError).
            if fm.fileExists(atPath: newURL.path) {
                conflicts.append(p.newPath)
                skipped += 1
                continue
            }
            do {
                try fm.moveItem(at: oldURL, to: newURL)
                moved += 1
                try await database.pool.write { db in
                    try db.execute(
                        sql: "UPDATE files SET path_text = ? WHERE id = ?",
                        arguments: [newURL.path, p.fileID]
                    )
                }
            } catch {
                failed += 1
                // NSError text embeds both full paths — log domain+code only.
                let ns = error as NSError
                JSONLog.shared.warn(ev: "restructure_move_failed",
                                    path: redactPathForLog(oldURL.path),
                                    error: "\(ns.domain) \(ns.code)")
            }
        }
        JSONLog.shared.info(ev: "restructure_applied",
                            extra: ["moved": AnyCodable(moved),
                                    "skipped": AnyCodable(skipped),
                                    "failed": AnyCodable(failed)])
        return ApplyResult(moved: moved, skipped: skipped, failed: failed, conflicts: conflicts)
    }


    private static func monthName(_ m: Int) -> String {
        let names = ["", "01-Jan","02-Feb","03-Mar","04-Apr","05-May","06-Jun",
                     "07-Jul","08-Aug","09-Sep","10-Oct","11-Nov","12-Dec"]
        return names[max(1, min(12, m))]
    }
}
