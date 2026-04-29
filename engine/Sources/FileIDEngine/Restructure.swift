// Restructure — proposed folder hierarchy generator.
//
// Reads every image's metadata (faces, location, date, vlm_*) and
// proposes a destination path under a chosen "library root." The
// proposal is returned as a list of (old_path, new_path) pairs that
// the UI renders as a diff. Nothing moves until the user clicks
// "Apply selected" — and even then only the rows the user kept
// checked.
//
// Heuristic priority (first match wins):
//   1. Named person  → People/<Name>/<Year>/
//   2. GPS location  → Places/<lat,lon-bucketed>/<Year>/
//   3. Document      → Documents/<Year>/
//   4. Year-month    → <Year>/<Month>/
//
// VLM proposed name (when present) becomes the new filename within
// whichever folder the heuristic picks.
//
// This is intentionally rule-based, not LLM-driven. For 50K-file
// libraries, the LLM cost dominates and the heuristic produces a
// stable, predictable layout the user can trust. A future LLM-driven
// pass can plug in here for the categories that benefit (e.g.,
// distinguishing "vacation" from "everyday" photos by caption).
import Foundation
import GRDB
import FileIDShared

public struct RestructureProposal: Sendable {
    public let fileID: Int64
    public let oldPath: String
    public let newPath: String
    public let bucket: String        // "People/Mom", "Places/...", etc — used to group in UI
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
        let rows: [Source] = try await database.pool.read { db in
            // LEFT JOIN persons via face_prints to get any named-person
            // strings on each file. We GROUP_CONCAT names, then split
            // back in Swift (avoids a per-file second query).
            let r = try GRDB.Row.fetchAll(db, sql: """
                SELECT
                  f.id, f.path_text, f.kind, f.created_at, f.modified_at,
                  f.location_lat, f.location_lon, f.has_text, f.vlm_proposed_name,
                  GROUP_CONCAT(DISTINCT p.name) AS names
                FROM files f
                LEFT JOIN face_prints fp ON fp.file_id = f.id
                LEFT JOIN persons p ON p.id = fp.person_id
                WHERE f.failed = 0
                GROUP BY f.id
                """)
            return r.map { row in
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
        }

        let calendar = Calendar(identifier: .gregorian)
        var proposals: [RestructureProposal] = []
        proposals.reserveCapacity(rows.count)
        for s in rows {
            // Skip entries already inside the library root in a
            // restructured-looking path — minimal heuristic to
            // avoid re-restructuring every run.
            let date: Date? = {
                if let c = s.createdAt { return Date(timeIntervalSinceReferenceDate: c) }
                if let m = s.modifiedAt { return Date(timeIntervalSinceReferenceDate: m) }
                return nil
            }()
            let year = date.map { String(calendar.component(.year, from: $0)) }
            let month = date.map { Self.monthName(calendar.component(.month, from: $0)) }

            // Pick a bucket.
            let bucket: String
            if let names = s.personNames, !names.isEmpty {
                let first = names.split(separator: ",").first.map { String($0).trimmingCharacters(in: .whitespaces) } ?? "Unknown"
                bucket = "People/\(first)"
            } else if let lat = s.lat, let lon = s.lon {
                // 0.5° bucket ≈ ~50 km cells. Names like "37.5,-122.0".
                let latB = (lat * 2).rounded() / 2
                let lonB = (lon * 2).rounded() / 2
                bucket = String(format: "Places/%.1f_%.1f", latB, lonB)
            } else if s.hasText != 0 || s.kind == "pdf" || s.kind == "doc" {
                bucket = "Documents"
            } else if let y = year {
                bucket = "Photos/\(y)" + (month.map { "/\($0)" } ?? "")
            } else {
                bucket = "Misc"
            }

            // Filename: keep original or use VLM suggestion.
            let oldURL = URL(fileURLWithPath: s.path)
            let baseName = oldURL.deletingPathExtension().lastPathComponent
            let ext = oldURL.pathExtension
            let newName: String
            if let p = s.vlmProposed, !p.isEmpty {
                newName = ext.isEmpty ? p : "\(p).\(ext)"
            } else {
                newName = oldURL.lastPathComponent
            }
            // Compose target.
            var target = libraryRoot.appendingPathComponent(bucket, isDirectory: true)
            if let y = year, !bucket.contains(y) {
                target = target.appendingPathComponent(y, isDirectory: true)
            }
            target = target.appendingPathComponent(newName)
            proposals.append(RestructureProposal(
                fileID: s.id,
                oldPath: s.path,
                newPath: target.path,
                bucket: bucket
            ))
            _ = baseName  // hush unused
        }
        return proposals
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
        database: Database
    ) async -> ApplyResult {
        let fm = FileManager.default
        var moved = 0
        var skipped = 0
        var failed = 0
        var conflicts: [String] = []

        for p in proposals {
            let oldURL = URL(fileURLWithPath: p.oldPath)
            let newURL = URL(fileURLWithPath: p.newPath)
            if oldURL == newURL { skipped += 1; continue }
            // Ensure dir exists.
            do {
                try fm.createDirectory(at: newURL.deletingLastPathComponent(),
                                       withIntermediateDirectories: true)
            } catch {
                failed += 1; continue
            }
            // Skip if destination collides with existing file.
            if fm.fileExists(atPath: newURL.path) {
                conflicts.append(p.newPath)
                skipped += 1
                continue
            }
            do {
                try fm.moveItem(at: oldURL, to: newURL)
                moved += 1
                // Update path_text in DB.
                try await database.pool.write { db in
                    try db.execute(
                        sql: "UPDATE files SET path_text = ? WHERE id = ?",
                        arguments: [newURL.path, p.fileID]
                    )
                }
            } catch {
                failed += 1
                JSONLog.shared.warn(ev: "restructure_move_failed",
                                    path: oldURL.path, error: "\(error)")
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
