//! Learn-from-corrections: a token→folder co-occurrence memory backed by the v18
//! `restructure_feedback` table. Every move a user APPLIES is approved, so the moved
//! file's filename tokens are credited toward its destination folder; the next plan
//! reads those weights as an ADDITIVE confidence hint — it can upgrade a move the
//! planner already produced to Auto, never re-route. Instance-based, no model
//! retraining (the SOTA pattern, deep-research 2026-06-17). Lockstep with the Swift
//! engine's `RestructureFeedback`.

use std::path::Path;
use std::sync::Arc;

use parking_lot::Mutex;
use rusqlite::{params, Connection};

use super::restructure::{Confidence, ProposedMove};
use super::restructure_semantic::filename_tokens;

/// Total summed feedback weight (over a move's filename tokens toward its destination
/// folder) at/above which the destination is treated as the user's learned habit and
/// the move is upgraded to auto-confidence. Lockstep with the Swift constant.
pub(crate) const FEEDBACK_AUTO_WEIGHT: i64 = 3;

/// Basename of a destination FILE path's parent folder — the "folder" key. None when
/// the destination has no parent or an empty basename.
fn dest_folder(dest: &Path) -> Option<String> {
    dest.parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Credit each applied move's filename tokens toward its destination folder. Best-
/// effort — a feedback write never fails an apply. `now` is the unix timestamp stamped
/// on the touched rows. Call ONLY on a forward apply (never undo).
pub(crate) fn record<'a>(
    conn: &Arc<Mutex<Connection>>,
    moves: impl Iterator<Item = (&'a Path, &'a Path)>,
    now: f64,
) {
    let conn = conn.lock();
    for (source, destination) in moves {
        let Some(folder) = dest_folder(destination) else {
            continue;
        };
        for token in filename_tokens(source) {
            let _ = conn.execute(
                "INSERT INTO restructure_feedback (token, folder, weight, updated_at)
                 VALUES (?1, ?2, 1, ?3)
                 ON CONFLICT(token, folder) DO UPDATE SET weight = weight + 1, updated_at = ?3",
                params![token, folder, now],
            );
        }
    }
}

/// Additive confidence boost: a planned move whose filename tokens were previously
/// filed into the SAME destination folder is upgraded to Auto (with a note), because
/// the user has demonstrably filed files like this there before. Only touches
/// confidence on the moves the planner already produced — never re-routes.
pub(crate) fn boost(conn: &Arc<Mutex<Connection>>, moves: &mut [ProposedMove]) {
    if moves.is_empty() {
        return;
    }
    let conn = conn.lock();
    let Ok(mut stmt) = conn.prepare(
        "SELECT COALESCE(SUM(weight), 0) FROM restructure_feedback WHERE folder = ?1 AND token = ?2",
    ) else {
        return;
    };
    for m in moves.iter_mut() {
        // Only upgrade Review → Auto. An Ask-tier move is one the butler was
        // explicitly UNSURE about — RESTRUCTURE.md defines Ask as "leave in
        // place, needs per-file consent". Upgrading Ask → Auto here (at plan
        // time, before the plan is spooled) let feedback weight silently
        // rewrite an unsure move to auto, so exclude_ask_tier no longer
        // matched it and it rode the bulk stored-plan apply without the
        // consent the Ask tier exists to require.
        if matches!(m.confidence, Confidence::Auto | Confidence::Ask) {
            continue;
        }
        let Some(folder) = dest_folder(&m.destination) else {
            continue;
        };
        let mut score: i64 = 0;
        for token in filename_tokens(&m.source) {
            score += stmt
                .query_row(params![folder, &token], |r| r.get::<_, i64>(0))
                .unwrap_or(0);
        }
        if score >= FEEDBACK_AUTO_WEIGHT {
            m.confidence = Confidence::Auto;
            let note = "you've filed files like this here before";
            m.reason = Some(match m.reason.take() {
                Some(r) if !r.is_empty() => format!("{r}; {note}"),
                _ => format!("Learned from your filing — {note}"),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn db() -> Arc<Mutex<Connection>> {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE restructure_feedback (
                token TEXT NOT NULL, folder TEXT NOT NULL,
                weight INTEGER NOT NULL DEFAULT 1, updated_at DOUBLE NOT NULL DEFAULT 0,
                PRIMARY KEY (token, folder));",
        )
        .unwrap();
        Arc::new(Mutex::new(conn))
    }

    fn pm(id: i64, source: &str, dest: &str, conf: Confidence) -> ProposedMove {
        ProposedMove {
            file_id: id,
            source: source.into(),
            destination: dest.into(),
            category: String::new(),
            confidence: conf,
            reason: None,
        }
    }

    #[test]
    fn record_then_boost_upgrades_a_matching_move() {
        // Labeled scenario: the user applied three "acme invoice" files into
        // /lib/Invoices; a NEW similar file the planner only marked Review must be
        // upgraded to Auto on the learned filing habit.
        let db = db();
        let applied: Vec<(PathBuf, PathBuf)> = (0..3)
            .map(|i| {
                (
                    PathBuf::from(format!("/in/acme_invoice_{i}.pdf")),
                    PathBuf::from(format!("/lib/Invoices/acme_invoice_{i}.pdf")),
                )
            })
            .collect();
        record(&db, applied.iter().map(|(s, d)| (s.as_path(), d.as_path())), 0.0);

        let mut moves = vec![pm(
            99,
            "/in/acme_invoice_new.pdf",
            "/lib/Invoices/acme_invoice_new.pdf",
            Confidence::Review,
        )];
        boost(&db, &mut moves);
        assert_eq!(moves[0].confidence, Confidence::Auto, "filing history should upgrade the move");
        assert!(moves[0].reason.as_deref().unwrap_or("").contains("filed files like this"));
    }

    #[test]
    fn boost_leaves_unrelated_moves_alone() {
        let db = db();
        record(
            &db,
            [(
                Path::new("/in/acme_invoice_0.pdf"),
                Path::new("/lib/Invoices/acme_invoice_0.pdf"),
            )]
            .into_iter(),
            0.0,
        );
        let mut moves = vec![pm(1, "/in/trip_hawaii.mp4", "/lib/Videos/trip_hawaii.mp4", Confidence::Review)];
        boost(&db, &mut moves);
        assert_eq!(moves[0].confidence, Confidence::Review, "no feedback for this folder → unchanged");
    }

    #[test]
    fn boost_never_upgrades_an_ask_tier_move() {
        // The butler marked this move Ask (explicitly unsure — "leave in place,
        // needs consent"). Even with overwhelming filing history toward the
        // destination, boost() must NOT rewrite it to Auto, or it would ride
        // the bulk stored-plan apply without the per-file consent the Ask tier
        // exists to require.
        let db = db();
        let applied: Vec<(PathBuf, PathBuf)> = (0..5)
            .map(|i| {
                (
                    PathBuf::from(format!("/in/receipt_{i}.pdf")),
                    PathBuf::from(format!("/lib/Receipts/receipt_{i}.pdf")),
                )
            })
            .collect();
        record(&db, applied.iter().map(|(s, d)| (s.as_path(), d.as_path())), 0.0);

        let mut moves = vec![pm(
            42,
            "/in/receipt_new.pdf",
            "/lib/Receipts/receipt_new.pdf",
            Confidence::Ask,
        )];
        boost(&db, &mut moves);
        assert_eq!(
            moves[0].confidence,
            Confidence::Ask,
            "an Ask-tier move must never be auto-upgraded by feedback"
        );
    }

    #[test]
    fn record_accumulates_weight() {
        let db = db();
        let one = [(Path::new("/in/report_q1.pdf"), Path::new("/lib/Reports/report_q1.pdf"))];
        record(&db, one.into_iter(), 0.0);
        record(&db, one.into_iter(), 1.0);
        let w: i64 = db
            .lock()
            .query_row(
                "SELECT weight FROM restructure_feedback WHERE token='report' AND folder='Reports'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(w, 2, "re-recording the same token→folder bumps the weight");
    }
}
