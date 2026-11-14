// People actions that need an answer from the engine: undoing a merge
// (`revertMerge`), "Not the same person" on a merge suggestion
// (`markPersonsDifferent`) and moving photos to another person
// (`reassignFace`). The wording is the macOS wording.

use std::cell::RefCell;
use std::rc::Rc;

use fileid_engine::ipc::{BulkActionResult, CommandPayload, FaceReviewPage, UndoFaceReviewPayload};
use gtk::glib;

use crate::engine_client::{EngineClient, EngineEvent, read_db};

struct Subscription(crate::engine_client::EventReceiver);

impl Drop for Subscription {
    fn drop(&mut self) {
        self.0.close();
    }
}

/// Sends every payload, then collects the `action` results: one per payload
/// that was sent, fewer if the engine restarts or stays silent for
/// `timeout_secs`. `None` when the first payload could not be sent.
pub(super) async fn send_and_collect(
    engine: &Rc<RefCell<EngineClient>>,
    payloads: Vec<CommandPayload>,
    action: &str,
    timeout_secs: u32,
) -> Option<Vec<BulkActionResult>> {
    let subscription = Subscription(engine.borrow_mut().subscribe());
    let mut sent = 0;
    for payload in payloads {
        if engine.borrow_mut().send(payload).is_err() {
            break;
        }
        sent += 1;
    }
    if sent == 0 {
        return None;
    }
    let closer = subscription.0.closer();
    glib::timeout_add_seconds_local_once(timeout_secs, move || {
        closer.close();
    });
    let mut results = Vec::new();
    while results.len() < sent {
        let Ok(event) = subscription.0.recv().await else { break };
        match &*event {
            EngineEvent::BulkActionResult(result) if result.action == action => {
                results.push(result.clone())
            }
            EngineEvent::Exited => break,
            _ => {}
        }
    }
    Some(results)
}

/// Asks the engine to undo a merge it journaled (`undoFaceReview` with the
/// merge's operation). `None` when the command could not be sent, the engine
/// restarted or it stayed silent for `timeout_secs`.
pub(super) async fn undo_merge_operation(
    engine: &Rc<RefCell<EngineClient>>,
    operation: String,
    timeout_secs: u32,
) -> Option<FaceReviewPage> {
    let subscription = Subscription(engine.borrow_mut().subscribe());
    let request_id = super::face_review::request_id();
    let payload = CommandPayload::UndoFaceReview(UndoFaceReviewPayload { request_id: request_id.clone(), operation_id: operation });
    engine.borrow_mut().send(payload).ok()?;
    let closer = subscription.0.closer();
    glib::timeout_add_seconds_local_once(timeout_secs, move || {
        closer.close();
    });
    loop {
        let Ok(event) = subscription.0.recv().await else { return None };
        match &*event {
            EngineEvent::FaceReview(page) if page.request_id == request_id => return Some(page.clone()),
            EngineEvent::Exited => return None,
            _ => {}
        }
    }
}

pub(super) fn first_failure(results: &[BulkActionResult]) -> Option<String> {
    results
        .iter()
        .flat_map(|result| result.messages.iter())
        .find(|item| !item.ok)
        .and_then(|item| item.message.clone())
}

/// Every face the person owns now, for the merge undo record.
pub(super) fn read_face_ids(person_ids: Vec<i64>) -> async_channel::Receiver<Vec<(i64, Vec<i64>)>> {
    read_db(move |conn| {
        let Ok(mut stmt) =
            conn.prepare("SELECT id FROM face_prints WHERE person_id = ?1 ORDER BY id")
        else {
            return Vec::new();
        };
        person_ids
            .into_iter()
            .map(|person| {
                let faces = stmt
                    .query_map([person], |row| row.get::<_, i64>(0))
                    .map(|rows| rows.flatten().collect())
                    .unwrap_or_default();
                (person, faces)
            })
            .collect()
    })
}

pub(super) fn move_title(count: usize) -> String {
    format!(
        "Move {count} photo{} to…",
        if count == 1 { "" } else { "s" }
    )
}

pub(super) fn move_hint(source_name: &str) -> String {
    format!(
        "Pick the person these photos actually show. Only the selected photos move; the rest of {source_name}'s group stays put."
    )
}

pub(super) fn selected_label(count: usize) -> String {
    format!("{count} selected")
}

pub(super) fn move_status(
    moved: usize,
    failed: usize,
    target_name: &str,
    first_error: Option<&str>,
) -> String {
    let mut line = format!(
        "Moved {moved} photo{} to {target_name}",
        if moved == 1 { "" } else { "s" }
    );
    if failed > 0 {
        line.push_str(&format!(" · {failed} couldn't be moved"));
        if let Some(error) = first_error {
            line.push_str(&format!(" — {error}"));
        }
    }
    line
}

pub(super) const MERGE_CONFIRM_HEADING: &str = "Merge these people?";
pub(super) const MERGE_CONFIRM_BODY: &str = "You can undo this right after.";

/// Every People merge asks first; `merge` runs only on the Merge answer.
pub(super) fn confirm_merge(anchor: &impl gtk::prelude::IsA<gtk::Widget>, merge: impl FnOnce() + 'static) {
    use adw::prelude::*;
    let dialog = adw::AlertDialog::new(Some(MERGE_CONFIRM_HEADING), Some(MERGE_CONFIRM_BODY));
    dialog.add_responses(&[("cancel", "Cancel"), ("merge", "Merge")]);
    dialog.set_response_appearance("merge", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let merge = RefCell::new(Some(merge));
    dialog.connect_response(None, move |_, response| {
        if response == "merge"
            && let Some(merge) = merge.borrow_mut().take()
        {
            merge();
        }
    });
    dialog.present(Some(anchor));
}

pub(super) fn undo_merge_label(source_name: &str, destination_name: &str) -> String {
    format!("Split {source_name} back out of {destination_name}")
}

pub(super) const UNDO_MERGE_OK: &str = "Merge undone. The cards are separate again.";

pub(super) fn undo_merge_failed(detail: &str) -> String {
    format!("Couldn't undo the merge: {detail}")
}

pub(super) const DIFFERENT_OK: &str =
    "Marked as different people. FileID won't suggest this pair again.";

pub(super) fn different_failed(detail: &str) -> String {
    format!("Couldn't save that: {detail}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fileid_engine::ipc::BulkActionItem;

    fn item(ok: bool, message: Option<&str>) -> BulkActionItem {
        BulkActionItem {
            file_id: Some(1),
            ok,
            message: message.map(str::to_owned),
            reason: None,
            operation_id: None,
        }
    }

    #[test]
    fn move_wording_matches_the_mac_sheet() {
        assert_eq!(move_title(1), "Move 1 photo to…");
        assert_eq!(move_title(3), "Move 3 photos to…");
        assert_eq!(
            move_hint("Sam"),
            "Pick the person these photos actually show. Only the selected photos move; the rest of Sam's group stays put."
        );
        assert_eq!(selected_label(4), "4 selected");
        assert_eq!(move_status(1, 0, "Alex", None), "Moved 1 photo to Alex");
        assert_eq!(
            move_status(2, 1, "Alex", Some("gone")),
            "Moved 2 photos to Alex · 1 couldn't be moved — gone"
        );
    }

    #[test]
    fn the_first_failure_is_the_first_unsuccessful_message() {
        let results = vec![
            BulkActionResult {
                action: "reassignFace".into(),
                succeeded: 1,
                failed: 0,
                messages: vec![item(true, None)],
            },
            BulkActionResult {
                action: "reassignFace".into(),
                succeeded: 0,
                failed: 1,
                messages: vec![item(false, Some("This face no longer exists"))],
            },
        ];
        assert_eq!(
            first_failure(&results).as_deref(),
            Some("This face no longer exists")
        );
        assert_eq!(first_failure(&results[..1]), None);
    }

    #[test]
    fn merge_undo_and_verdict_lines() {
        assert_eq!(undo_merge_label("A", "B"), "Split A back out of B");
        assert_eq!(undo_merge_failed("x"), "Couldn't undo the merge: x");
        assert_eq!(different_failed("y"), "Couldn't save that: y");
    }
}
