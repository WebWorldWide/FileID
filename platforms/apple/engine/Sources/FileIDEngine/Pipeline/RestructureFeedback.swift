// Learn-from-corrections: a token→folder co-occurrence memory backed by the v18
// `restructure_feedback` table. Every move a user APPLIES is approved, so the moved
// file's filename tokens are credited toward its destination folder; the next plan
// reads those weights as an ADDITIVE confidence hint — it can upgrade a move the
// planner already produced to Auto, never re-route. Instance-based, no model
// retraining (the SOTA pattern, deep-research 2026-06-17). Swift mirror of the Rust
// engine's `restructure_feedback` (kept byte-faithful so a library round-trips).
import Foundation
import GRDB
import FileIDShared

public enum RestructureFeedback {

    /// Total summed feedback weight (over a move's filename tokens toward its
    /// destination folder) at/above which the destination is treated as the user's
    /// learned habit and the move is upgraded to auto-confidence. Lockstep with the
    /// Rust `FEEDBACK_AUTO_WEIGHT`.
    static let autoWeight = 3

    /// Basename of a destination FILE path's parent folder — the "folder" key. nil
    /// when the destination has no parent or an empty basename.
    static func destFolder(_ dest: String) -> String? {
        let parent = (dest as NSString).deletingLastPathComponent
        let name = (parent as NSString).lastPathComponent
        return name.isEmpty ? nil : name
    }

    /// Credit each applied move's filename tokens toward its destination folder.
    /// Best-effort — a feedback write never fails an apply. `now` is the unix
    /// timestamp stamped on the touched rows. Call ONLY on a forward apply (never undo).
    public static func record(
        database: Database,
        moves: [(source: String, destination: String)],
        now: Double
    ) async {
        guard !moves.isEmpty else { return }
        try? await database.pool.write { db in
            for m in moves {
                guard let folder = destFolder(m.destination) else { continue }
                for token in RestructureSemantic.filenameTokens(m.source) {
                    try db.execute(sql: """
                        INSERT INTO restructure_feedback (token, folder, weight, updated_at)
                        VALUES (?, ?, 1, ?)
                        ON CONFLICT(token, folder) DO UPDATE SET weight = weight + 1, updated_at = ?
                        """, arguments: [token, folder, now, now])
                }
            }
        }
    }

    /// Additive confidence boost: a planned move whose filename tokens were
    /// previously filed into the SAME destination folder is upgraded to Auto (with a
    /// note), because the user has demonstrably filed files like this there before.
    /// Only raises confidence on moves the planner already produced — never re-routes.
    /// Returns the (possibly upgraded) proposals; `RestructureProposal` is immutable,
    /// so this rebuilds rather than mutating in place (the Rust mirror takes `&mut`).
    public static func boost(
        database: Database,
        proposals: [RestructureProposal]
    ) async -> [RestructureProposal] {
        guard !proposals.isEmpty else { return proposals }
        // Sum the feedback weight for each proposal's (folder, tokens) in ONE read
        // transaction; a failed read leaves every proposal unchanged.
        let scores: [Int] = (try? await database.pool.read { db -> [Int] in
            proposals.map { p in
                guard let folder = destFolder(p.newPath) else { return 0 }
                var score = 0
                for token in RestructureSemantic.filenameTokens(p.oldPath) {
                    if let w = try? Int.fetchOne(db, sql: """
                        SELECT COALESCE(SUM(weight), 0) FROM restructure_feedback
                        WHERE folder = ? AND token = ?
                        """, arguments: [folder, token]) {
                        score += w
                    }
                }
                return score
            }
        }) ?? proposals.map { _ in 0 }

        let note = "you've filed files like this here before"
        return zip(proposals, scores).map { p, score in
            guard score >= autoWeight, p.confidence != "auto" else { return p }
            let reason: String
            if let r = p.reason, !r.isEmpty {
                reason = "\(r); \(note)"
            } else {
                reason = "Learned from your filing — \(note)"
            }
            return RestructureProposal(
                fileID: p.fileID, oldPath: p.oldPath, newPath: p.newPath,
                bucket: p.bucket, confidence: "auto", reason: reason)
        }
    }
}
