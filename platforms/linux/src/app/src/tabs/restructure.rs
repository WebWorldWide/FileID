use crate::engine_client::{EngineClient, EngineState};
use adw::prelude::*;
use fileid_engine::ipc::*;
use std::{cell::RefCell, rc::Rc};

// Parent may enable only after integrating and accepting atomic no-overwrite Linux move/Undo safety.
pub(super) const LINUX_MOVE_SAFETY_ACCEPTED: bool = true;
const UNSAFE_RUNTIME_REASON: &str = "Apply and Undo are unavailable: this Linux engine's move runtime has not passed atomic no-overwrite and source-deletion safety acceptance. Planning and selection do not change your files.";

#[derive(Default)]
struct State {
    root: String,
    plan: Option<RestructurePlan>,
    selected: Vec<RestructureMove>,
    pending: Option<&'static str>,
    undo_root: Option<String>,
    generation: u64,
}
fn selected_by_default(m: &RestructureMove) -> bool {
    m.confidence.eq_ignore_ascii_case("auto")
}
fn can_apply(s: &State) -> bool {
    LINUX_MOVE_SAFETY_ACCEPTED && s.pending.is_none() && s.plan.is_some() && !s.selected.is_empty()
}
fn outcome(action: &str, r: &RestructureApplyResult) -> String {
    format!(
        "{action}: {} applied · {} failed{}. Partial outcomes must be reviewed before retrying.",
        r.applied,
        r.failed,
        r.privilege_error
            .as_ref()
            .map(|s| format!(" · {s}"))
            .unwrap_or_default()
    )
}
struct Ui {
    body: gtk::Box,
    root: gtk::Entry,
    pick: gtk::Button,
    plan: gtk::Button,
    apply: gtk::Button,
    undo: gtk::Button,
    status: gtk::Label,
    proposals: gtk::Box,
}
impl Ui {
    fn update(&self, s: &State) {
        let busy = s.pending.is_some();
        self.root.set_sensitive(!busy);
        self.pick.set_sensitive(!busy);
        self.plan.set_sensitive(!busy);
        self.apply.set_sensitive(can_apply(s));
        self.undo
            .set_sensitive(LINUX_MOVE_SAFETY_ACCEPTED && !busy && s.undo_root.is_some());
        self.proposals.set_sensitive(!busy);
    }
    fn clear_proposals(&self) {
        while let Some(c) = self.proposals.first_child() {
            self.proposals.remove(&c);
        }
    }
}

pub fn build_restructure_tab(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.append(
        &gtk::Label::builder()
            .label("Restructure")
            .xalign(0.0)
            .css_classes(["title-1"])
            .build(),
    );
    body.append(&gtk::Label::builder().label("Review a plan inside an indexed library root. Nothing moves without explicit confirmation. Review and Ask-confidence moves start unchecked. Shortcuts and scoped cancellation are unavailable in the current IPC contract.").wrap(true).xalign(0.0).build());
    if !LINUX_MOVE_SAFETY_ACCEPTED {
        body.append(
            &gtk::Label::builder()
                .label(UNSAFE_RUNTIME_REASON)
                .wrap(true)
                .xalign(0.0)
                .build(),
        );
    }
    let root = gtk::Entry::builder()
        .placeholder_text("Indexed library root (absolute path)")
        .build();
    body.append(&root);
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    body.append(&actions);
    let pick = gtk::Button::with_label("Pick root");
    let plan = gtk::Button::with_label("Compute plan");
    let apply = gtk::Button::with_label("Apply selected moves");
    apply.add_css_class("suggested-action");
    let undo = gtk::Button::with_label("Undo last run");
    if !LINUX_MOVE_SAFETY_ACCEPTED {
        apply.set_tooltip_text(Some(UNSAFE_RUNTIME_REASON));
        undo.set_tooltip_text(Some(UNSAFE_RUNTIME_REASON));
    }
    for b in [&pick, &plan, &apply, &undo] {
        actions.append(b);
    }
    let status = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .build();
    body.append(&status);
    let proposals = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.append(&proposals);
    let state = Rc::new(RefCell::new(State::default()));
    let ui = Rc::new(Ui {
        body: body.clone(),
        root,
        pick,
        plan,
        apply,
        undo,
        status,
        proposals,
    });
    ui.update(&state.borrow());
    {
        let ui = ui.clone();
        let button = ui.pick.clone();
        button.connect_clicked(move |_| {
            let dialog = gtk::FileDialog::builder()
                .title("Indexed library root")
                .build();
            let win = ui.body.root().and_downcast::<gtk::Window>();
            let ui = ui.clone();
            dialog.select_folder(win.as_ref(), gio::Cancellable::NONE, move |r| {
                if let Ok(file) = r {
                    if let Some(p) = file.path() {
                        ui.root.set_text(&p.to_string_lossy());
                    }
                }
            });
        });
    }
    {
        let state = state.clone();
        let ui = ui.clone();
        let root = ui.root.clone();
        root.connect_changed(move |entry| {
            let mut s = state.borrow_mut();
            s.root = entry.text().to_string();
            s.generation += 1;
            s.plan = None;
            s.selected.clear();
            ui.clear_proposals();
            ui.update(&s);
        });
    }
    {
        let state = state.clone();
        let engine = engine.clone();
        let ui = ui.clone();
        let plan = ui.plan.clone();
        plan.connect_clicked(move |_| {
            if state.borrow().pending.is_some() {
                return;
            }
            let path = ui.root.text().to_string();
            if !std::path::Path::new(&path).is_absolute() {
                ui.status
                    .set_label("Choose an absolute indexed library root.");
                return;
            }
            {
                let mut s = state.borrow_mut();
                s.root = path.clone();
                s.generation += 1;
                s.plan = None;
                s.selected.clear();
                s.pending = Some("plan");
            }
            ui.clear_proposals();
            ui.update(&state.borrow());
            if let Err(e) =
                engine
                    .borrow_mut()
                    .send(CommandPayload::PlanRestructure(PlanRestructurePayload {
                        library_root: path,
                    }))
            {
                state.borrow_mut().pending = None;
                ui.status.set_label(&e.to_string());
                ui.update(&state.borrow());
            } else {
                ui.status.set_label("Computing plan…");
            }
        });
    }
    {
        let state = state.clone();
        let engine = engine.clone();
        let ui = ui.clone();
        let apply = ui.apply.clone();
        apply.connect_clicked(move |_| {
            if !LINUX_MOVE_SAFETY_ACCEPTED { ui.status.set_label(UNSAFE_RUNTIME_REASON); return; }
            if !can_apply(&state.borrow()) { return; }
            let moves = state.borrow().selected.clone(); let target = state.borrow().root.clone();
            let generation = state.borrow().generation;
            let alert = adw::AlertDialog::new(Some("Move selected files on disk?"), Some(&format!("{} files will move inside {}. Originals change location. Undo uses the engine's move journal; review the result for partial failures.",moves.len(),target)));
            alert.add_response("cancel","Cancel"); alert.add_response("apply","Move files");
            alert.set_response_appearance("apply",adw::ResponseAppearance::Destructive);
            alert.set_default_response(Some("cancel")); alert.set_close_response("cancel");
            let state = state.clone(); let engine = engine.clone(); let u = ui.clone();
            alert.connect_response(None,move |_,response| {
                if response != "apply" || !LINUX_MOVE_SAFETY_ACCEPTED || state.borrow().pending.is_some() { return; }
                if state.borrow().root != target || state.borrow().generation != generation || state.borrow().plan.is_none() {
                    u.status.set_label("Plan changed; compute and review a new plan."); return;
                }
                state.borrow_mut().pending = Some("apply"); u.update(&state.borrow());
                if let Err(e) = engine.borrow_mut().send(CommandPayload::ApplyRestructure(ApplyRestructurePayload {library_root:target.clone(),moves:moves.clone(),use_symlinks:false})) {
                    state.borrow_mut().pending = None; u.status.set_label(&e.to_string()); u.update(&state.borrow());
                } else { u.status.set_label("Applying confirmed moves…"); }
            });
            alert.present(Some(&ui.body));
        });
    }
    {
        let state = state.clone();
        let engine = engine.clone();
        let ui = ui.clone();
        let undo = ui.undo.clone();
        undo.connect_clicked(move |_| {
            if !LINUX_MOVE_SAFETY_ACCEPTED {
                ui.status.set_label(UNSAFE_RUNTIME_REASON);
                return;
            }
            if state.borrow().pending.is_some() {
                return;
            }
            let Some(root) = state.borrow().undo_root.clone() else {
                return;
            };
            state.borrow_mut().pending = Some("undo");
            ui.update(&state.borrow());
            if let Err(e) =
                engine
                    .borrow_mut()
                    .send(CommandPayload::UndoRestructure(UndoRestructurePayload {
                        library_root: root,
                    }))
            {
                state.borrow_mut().pending = None;
                ui.status.set_label(&e.to_string());
                ui.update(&state.borrow());
            } else {
                ui.status.set_label("Undoing journaled moves…");
            }
        });
    }
    let rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(async move {
        while let Ok(ev) = rx.recv().await {
            match ev {
                EngineState::RestructurePlan(p) if state.borrow().pending == Some("plan") && p.library_root == state.borrow().root => {
                    state.borrow_mut().pending = None; ui.clear_proposals();
                    state.borrow_mut().selected = p.moves.iter().filter(|m| selected_by_default(m)).cloned().collect();
                    ui.status.set_label(&format!("{} proposed moves. Review every destination and confidence before applying.",p.moves.len()));
                    for m in &p.moves {
                        let check = gtk::CheckButton::with_label(&format!("{} → {}\n{} · {} · {}",m.source,m.destination,m.category,m.confidence,m.reason.as_deref().unwrap_or("No reason supplied")));
                        check.set_active(selected_by_default(m));
                        ui.proposals.append(&check);
                        let m = m.clone(); let state = state.clone(); let ui = ui.clone();
                        check.connect_toggled(move |c| {
                            let mut s = state.borrow_mut(); s.selected.retain(|x| x.file_id != m.file_id);
                            if c.is_active() { s.selected.push(m.clone()); } ui.update(&s);
                        });
                    }
                    state.borrow_mut().plan = Some(p);
                }
                EngineState::RestructureApplyResult(r) => {
                    let Some(action) = state.borrow_mut().pending.take() else { continue; };
                    if action == "plan" { continue; }
                    ui.status.set_label(&outcome(action,&r));
                    let mut s = state.borrow_mut();
                    if action == "apply" && r.applied > 0 { s.undo_root = Some(s.root.clone()); }
                    if action == "undo" && r.failed == 0 { s.undo_root = None; }
                    s.plan = None; s.selected.clear(); ui.clear_proposals();
                }
                EngineState::Error {kind,message,..} if kind.contains("restructure") => {
                    state.borrow_mut().pending = None; ui.status.set_label(&message);
                }
                EngineState::Failed(e) => {
                    let mut s = state.borrow_mut(); s.pending = None; s.plan = None;
                    ui.status.set_label(&format!("{e}. Interrupted mutations may be partial; review the library and journal."));
                }
                EngineState::Exited => {
                    let mut s = state.borrow_mut(); s.pending = None; s.plan = None;
                    ui.status.set_label("Engine exited. Interrupted moves may be partial; review the library and journal before retrying.");
                }
                _ => {}
            }
            ui.update(&state.borrow());
        }
    });
    gtk::ScrolledWindow::builder()
        .child(&body)
        .hexpand(true)
        .vexpand(true)
        .build()
        .upcast()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn low_confidence_moves_require_opt_in_and_partial_outcomes_are_visible() {
        let mut m = RestructureMove {
            file_id: 1,
            source: "/root/a".into(),
            destination: "/root/b".into(),
            category: "Docs".into(),
            tier: None,
            confidence: "ask".into(),
            reason: None,
            content_hash: None,
            size_bytes: None,
            file_ref: None,
            modified_at: None,
        };
        assert!(!selected_by_default(&m));
        m.confidence = "review".into();
        assert!(!selected_by_default(&m));
        m.confidence = "auto".into();
        assert!(selected_by_default(&m));
        assert!(outcome(
            "undo",
            &RestructureApplyResult {
                applied: 2,
                failed: 1,
                privilege_error: None
            }
        )
        .contains("2 applied · 1 failed"));
    }
    #[test]
    fn apply_requires_the_single_backend_safety_gate_and_a_current_idle_selection() {
        let mut s = State::default();
        assert!(!can_apply(&s));
        s.plan = Some(RestructurePlan {
            library_root: "/root".into(),
            moves: vec![],
            category_counts: vec![],
            folder_classifications: None,
        });
        s.selected.push(RestructureMove {
            file_id: 1,
            source: "/root/a".into(),
            destination: "/root/b".into(),
            category: "Docs".into(),
            tier: None,
            confidence: "auto".into(),
            reason: None,
            content_hash: None,
            size_bytes: None,
            file_ref: None,
            modified_at: None,
        });
        assert_eq!(can_apply(&s), LINUX_MOVE_SAFETY_ACCEPTED);
        s.pending = Some("plan");
        assert!(!can_apply(&s));
    }
}
