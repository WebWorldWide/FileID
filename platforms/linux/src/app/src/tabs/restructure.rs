use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;

use fileid_engine::ipc::{
    ApplyRestructurePayload, CommandPayload, PlanRestructurePayload, RestructureMove,
    RestructurePlan, UndoRestructurePayload,
};

use crate::engine_client::{EngineClient, EngineState};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Planning,
    Applying,
    Undoing,
}

struct MoveRow {
    check: gtk::CheckButton,
    movement: RestructureMove,
}

struct Ui {
    engine: Rc<RefCell<EngineClient>>,
    root: RefCell<Option<String>>,
    rows: RefCell<Vec<MoveRow>>,
    operation: Cell<Option<Operation>>,
    can_undo: Cell<bool>,
    root_label: gtk::Label,
    status: gtk::Label,
    summary: gtk::Label,
    selected_count: gtk::Label,
    pick_button: gtk::Button,
    plan_button: gtk::Button,
    apply_button: gtk::Button,
    undo_button: gtk::Button,
    list: gtk::ListBox,
}

pub fn build_restructure_tab(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(24)
        .margin_end(24)
        .css_classes(["fileid-tab"])
        .build();

    let title = gtk::Label::builder()
        .label("Restructure")
        .xalign(0.0)
        .css_classes(["title-1"])
        .build();
    let subtitle = gtk::Label::builder()
        .label("Preview the proposed moves, choose what to apply, and undo the last completed move batch.")
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    content.append(&title);
    content.append(&subtitle);

    let root_label = gtk::Label::builder()
        .label("Choose a library folder to build a plan.")
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes(["dim-label", "caption"])
        .build();
    let pick_button = gtk::Button::builder()
        .label("Choose folder…")
        .css_classes(["pill"])
        .build();
    let plan_button = gtk::Button::builder()
        .label("Preview plan")
        .css_classes(["gold-button"])
        .sensitive(false)
        .build();
    let root_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .build();
    root_row.append(&pick_button);
    root_row.append(&root_label);
    root_row.append(&plan_button);
    content.append(&root_row);

    let status = gtk::Label::builder()
        .label("Nothing moves until you apply selected proposals.")
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    let summary = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .wrap(true)
        .build();
    content.append(&status);
    content.append(&summary);

    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list)
        .build();
    content.append(&scroller);

    let selected_count = gtk::Label::builder()
        .label("0 selected")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["dim-label"])
        .build();
    let apply_button = gtk::Button::builder()
        .label("Apply selected")
        .css_classes(["gold-button"])
        .sensitive(false)
        .build();
    let undo_button = gtk::Button::builder()
        .label("Undo last batch")
        .css_classes(["pill"])
        .sensitive(false)
        .build();
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    actions.append(&selected_count);
    actions.append(&undo_button);
    actions.append(&apply_button);
    content.append(&actions);

    let ui = Rc::new(Ui {
        engine,
        root: RefCell::new(
            crate::app_settings::last_folder().map(|path| path.to_string_lossy().into_owned()),
        ),
        rows: RefCell::new(Vec::new()),
        operation: Cell::new(None),
        can_undo: Cell::new(false),
        root_label,
        status,
        summary,
        selected_count,
        pick_button,
        plan_button,
        apply_button,
        undo_button,
        list,
    });

    if let Some(root) = ui.root.borrow().as_deref() {
        set_root_label(&ui, root);
    }
    wire_actions(&ui);
    update_controls(&ui);

    let events = ui.engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(
        #[strong]
        ui,
        async move {
            while let Ok(event) = events.recv().await {
                match event {
                    EngineState::RestructurePlan(plan) => show_plan(&ui, plan),
                    EngineState::RestructureApplyResult(result) => finish_operation(&ui, result),
                    EngineState::Error { kind, message, .. } => {
                        if ui.operation.get().is_some() {
                            ui.operation.set(None);
                            ui.status.set_label(&format!("{kind}: {message}"));
                            update_controls(&ui);
                        }
                    }
                    EngineState::Exited => {
                        ui.operation.set(None);
                        ui.status
                            .set_label("The FileID engine exited. Restart the app to continue.");
                        update_controls(&ui);
                    }
                    _ => {}
                }
            }
        }
    ));

    content.upcast()
}

fn wire_actions(ui: &Rc<Ui>) {
    ui.pick_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |button| {
            let dialog = gtk::FileDialog::builder()
                .title("Choose a library folder")
                .modal(true)
                .build();
            let parent = button.root().and_downcast::<gtk::Window>();
            dialog.select_folder(
                parent.as_ref(),
                gtk::gio::Cancellable::NONE,
                clone!(
                    #[strong]
                    ui,
                    move |result| match result {
                        Ok(folder) => match folder.path() {
                            Some(path) => {
                                let root = path.to_string_lossy().into_owned();
                                crate::app_settings::remember_folder(&path);
                                *ui.root.borrow_mut() = Some(root.clone());
                                set_root_label(&ui, &root);
                                ui.rows.borrow_mut().clear();
                                clear_list(&ui.list);
                                ui.summary.set_label("");
                                ui.status.set_label("Folder selected. Preview a plan before applying any moves.");
                                update_controls(&ui);
                            }
                            None => {
                                ui.status.set_label("The selected folder has no usable filesystem path.");
                            }
                        },
                        Err(error) => {
                            tracing::debug!(error = %error, "Linux restructure folder picker closed");
                        }
                    }
                ),
            );
        }
    ));

    ui.plan_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            let Some(root) = ui.root.borrow().clone() else {
                return;
            };
            ui.operation.set(Some(Operation::Planning));
            ui.can_undo.set(false);
            ui.status
                .set_label("Building a preview from the selected folder…");
            ui.summary.set_label("");
            ui.rows.borrow_mut().clear();
            clear_list(&ui.list);
            update_controls(&ui);
            let command =
                CommandPayload::PlanRestructure(PlanRestructurePayload { library_root: root });
            if let Err(error) = ui.engine.borrow_mut().send(command) {
                ui.operation.set(None);
                ui.status
                    .set_label(&format!("Could not start the plan: {error}"));
                update_controls(&ui);
            }
        }
    ));

    ui.apply_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            if ui.operation.get().is_some() {
                return;
            }
            let Some(root) = ui.root.borrow().clone() else {
                return;
            };
            let moves: Vec<RestructureMove> = ui
                .rows
                .borrow()
                .iter()
                .filter(|row| row.check.is_active())
                .map(|row| row.movement.clone())
                .collect();
            if moves.is_empty() {
                return;
            }
            ui.operation.set(Some(Operation::Applying));
            ui.status.set_label("Applying selected moves…");
            update_controls(&ui);
            let command = CommandPayload::ApplyRestructure(ApplyRestructurePayload {
                library_root: root,
                moves,
                use_symlinks: false,
            });
            if let Err(error) = ui.engine.borrow_mut().send(command) {
                ui.operation.set(None);
                ui.status
                    .set_label(&format!("Could not apply the plan: {error}"));
                update_controls(&ui);
            }
        }
    ));

    ui.undo_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |_| {
            if ui.operation.get().is_some() {
                return;
            }
            let Some(root) = ui.root.borrow().clone() else {
                return;
            };
            ui.operation.set(Some(Operation::Undoing));
            ui.status
                .set_label("Undoing the last completed move batch…");
            update_controls(&ui);
            if let Err(error) = ui.engine.borrow_mut().send(CommandPayload::UndoRestructure(
                UndoRestructurePayload { library_root: root },
            )) {
                ui.operation.set(None);
                ui.status
                    .set_label(&format!("Could not undo the last batch: {error}"));
                update_controls(&ui);
            }
        }
    ));
}

fn show_plan(ui: &Ui, plan: RestructurePlan) {
    ui.operation.set(None);
    ui.can_undo.set(false);
    ui.rows.borrow_mut().clear();
    clear_list(&ui.list);

    let total = plan.moves.len();
    let mut auto = 0usize;
    let mut review = 0usize;
    let mut ask = 0usize;
    for movement in plan.moves {
        match movement.confidence.to_ascii_lowercase().as_str() {
            "auto" => auto += 1,
            "review" => review += 1,
            "ask" => ask += 1,
            _ => {}
        }
        append_move(ui, movement);
    }

    ui.summary.set_text(&format!(
        "{total} proposed moves · {auto} automatic · {review} review · {ask} ask. Only automatic moves start selected."
    ));
    ui.status.set_label(if total == 0 {
        "No moves were proposed for this folder."
    } else {
        "Review the destinations and reasons below. Nothing changes until you apply the selected moves."
    });
    update_selection(ui);
    update_controls(ui);
}

fn append_move(ui: &Ui, movement: RestructureMove) {
    let check = gtk::CheckButton::new();
    check.set_active(default_selected(&movement.confidence));
    let paths = gtk::Label::builder()
        .label(&format!("{}  →  {}", movement.source, movement.destination))
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .hexpand(true)
        .build();
    paths.set_tooltip_text(Some(&format!(
        "{}\n→ {}",
        movement.source, movement.destination
    )));
    let reason = movement
        .reason
        .as_deref()
        .unwrap_or("No explanation was provided.");
    let detail = gtk::Label::builder()
        .label(&format!(
            "{} · {} · {}",
            movement.category,
            movement.tier.as_deref().unwrap_or("Unclassified"),
            reason
        ))
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label", "caption"])
        .build();
    let text = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(3)
        .hexpand(true)
        .build();
    text.append(&paths);
    text.append(&detail);
    let line = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(10)
        .margin_end(10)
        .build();
    line.append(&check);
    line.append(&text);
    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&line));
    ui.list.append(&row);

    let ui_for_toggle = ui.clone();
    check.connect_toggled(move |_| update_selection(&ui_for_toggle));
    ui.rows.borrow_mut().push(MoveRow { check, movement });
}

fn finish_operation(ui: &Ui, result: fileid_engine::ipc::RestructureApplyResult) {
    let operation = ui.operation.replace(None);
    if let Some(message) = result
        .privilege_error
        .as_deref()
        .filter(|message| !message.is_empty())
    {
        ui.status.set_label(message);
    } else if operation == Some(Operation::Undoing) {
        if result.failed == 0 {
            ui.can_undo.set(false);
            ui.status.set_label(&format!(
                "Undo complete: {} files restored.",
                result.applied
            ));
        } else {
            ui.can_undo.set(true);
            ui.status.set_label(&format!(
                "Undo restored {} files; {} failed. You can retry.",
                result.applied, result.failed
            ));
        }
    } else {
        ui.can_undo.set(result.applied > 0);
        ui.status.set_label(&format!(
            "Applied {} moves; {} failed. The completed moves can be undone.",
            result.applied, result.failed
        ));
    }
    update_controls(ui);
}

fn set_root_label(ui: &Ui, root: &str) {
    ui.root_label.set_label(root);
    ui.root_label.set_tooltip_text(Some(root));
}

fn clear_list(list: &gtk::ListBox) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
}

fn selected_count(ui: &Ui) -> usize {
    ui.rows
        .borrow()
        .iter()
        .filter(|row| row.check.is_active())
        .count()
}

fn update_selection(ui: &Ui) {
    let count = selected_count(ui);
    ui.selected_count.set_label(&format!("{count} selected"));
    update_controls(ui);
}

fn update_controls(ui: &Ui) {
    let busy = ui.operation.get().is_some();
    let has_root = ui.root.borrow().is_some();
    ui.plan_button.set_sensitive(has_root && !busy);
    ui.apply_button
        .set_sensitive(selected_count(ui) > 0 && !busy);
    ui.undo_button.set_sensitive(ui.can_undo.get() && !busy);
}

fn default_selected(confidence: &str) -> bool {
    confidence.eq_ignore_ascii_case("auto")
}

#[cfg(test)]
mod tests {
    use super::default_selected;

    #[test]
    fn only_automatic_moves_are_selected_by_default() {
        assert!(default_selected("auto"));
        assert!(default_selected("AUTO"));
        assert!(!default_selected("review"));
        assert!(!default_selected("ask"));
        assert!(!default_selected(""));
    }
}
