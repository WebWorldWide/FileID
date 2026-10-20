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
    kept: gtk::Label,
    clip_banner: gtk::Box,
    clip_label: gtk::Label,
    clip_button: gtk::Button,
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

    let subtitle = gtk::Label::builder()
        .label("Preview the proposed moves, choose what to apply, and undo the last completed move batch.")
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
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
    let kept = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .css_classes(["pink-accent"])
        .build();
    content.append(&kept);
    let clip_banner = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .visible(false)
        .css_classes(["engine-note"])
        .build();
    let clip_label = gtk::Label::builder().xalign(0.0).wrap(true).hexpand(true).build();
    let clip_button = gtk::Button::builder().css_classes(["secondary"]).valign(gtk::Align::Center).build();
    clip_banner.append(&clip_label);
    clip_banner.append(&clip_button);
    content.append(&clip_banner);

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
        .css_classes(["dim-label", "numeric"])
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
        kept,
        clip_banner,
        clip_label,
        clip_button,
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
    ui.clip_button.connect_clicked(clone!(
        #[strong]
        ui,
        move |button| {
            if clip_model_installed() {
                let root = ui.root.borrow().clone();
                if let Some(root) = root {
                    let started = ui.engine.borrow_mut().start_scan(&root);
                    match started {
                        Ok(()) => ui.status.set_label("Rescanning with CLIP; build the plan again when it finishes."),
                        Err(error) => ui.status.set_label(&format!("Could not start the rescan: {error}")),
                    }
                }
            } else {
                let _ = button.activate_action("win.navigate", Some(&"settings".to_variant()));
            }
        }
    ));
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
            let command = CommandPayload::PlanRestructure(PlanRestructurePayload {
                library_root: root,
                supports_paged_plans: false,
            });
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
                plan_id: None,
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
                UndoRestructurePayload {
                    library_root: root,
                    shortcut_undo_token: None,
                },
            )) {
                ui.operation.set(None);
                ui.status
                    .set_label(&format!("Could not undo the last batch: {error}"));
                update_controls(&ui);
            }
        }
    ));
}

fn show_plan(ui: &Rc<Ui>, plan: RestructurePlan) {
    ui.operation.set(None);
    ui.can_undo.set(false);
    ui.rows.borrow_mut().clear();
    clear_list(&ui.list);

    let total = plan.moves.len();
    let anchors = plan.folder_classifications.as_ref().map_or(0, |counts| counts.anchor_folders);
    let total_moves = plan.total_moves.unwrap_or(total as u64);
    show_library_coverage(ui, &plan.library_root, anchors, total_moves);
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
    ui.status.set_label(&if total == 0 {
        zero_move_status(anchors)
    } else {
        "Review the destinations and reasons below. Nothing changes until you apply the selected moves.".to_owned()
    });
    update_selection(ui);
    update_controls(ui);
}

fn zero_move_status(anchors: u32) -> String {
    match anchors {
        0 => "No moves were proposed for this folder.".to_owned(),
        n => format!("Everything here is already organized. {n} folder{} kept.", if n == 1 { "" } else { "s" }),
    }
}

/// macOS "Keep N folders untouched · M files staying exactly where they are".
fn kept_summary(anchors: u32, indexed: u64, total_moves: u64) -> Option<String> {
    (anchors > 0).then(|| {
        let staying = indexed.saturating_sub(total_moves);
        format!(
            "Keep {anchors} folder{} untouched · {staying} file{} staying where {}",
            if anchors == 1 { "" } else { "s" },
            if staying == 1 { "" } else { "s" },
            if staying == 1 { "it is" } else { "they are" },
        )
    })
}

#[derive(Debug, Default, PartialEq, Eq)]
struct Coverage {
    indexed: u64,
    images_without_clip: u64,
}

fn coverage_under(conn: &rusqlite::Connection, root: &str) -> Coverage {
    let prefix = format!("{}/", root.trim_end_matches('/'));
    conn.query_row(
        "SELECT COUNT(*), \
         COALESCE(SUM(CASE WHEN f.kind = 'image' \
             AND NOT EXISTS (SELECT 1 FROM clip_embeddings c WHERE c.file_id = f.id) THEN 1 ELSE 0 END), 0) \
         FROM files f WHERE f.failed = 0 AND substr(f.path_text, 1, length(?1)) = ?1",
        rusqlite::params![prefix],
        |row| Ok(Coverage { indexed: row.get::<_, i64>(0)?.max(0) as u64, images_without_clip: row.get::<_, i64>(1)?.max(0) as u64 }),
    )
    .unwrap_or_default()
}

fn clip_model_installed() -> bool {
    use fileid_engine::models::registry::{self, LookupResult};
    matches!(registry::lookup_full("mobileclip_s2"), LookupResult::Found(model) if registry::installation_complete(&model))
}

/// Kept-folder summary and the CLIP banner, from one off-main read.
fn show_library_coverage(ui: &Rc<Ui>, root: &str, anchors: u32, total_moves: u64) {
    ui.kept.set_visible(false);
    ui.clip_banner.set_visible(false);
    let (tx, rx) = async_channel::bounded(1);
    let root = root.to_owned();
    std::thread::spawn(move || {
        let coverage = fileid_engine::paths::db_path()
            .ok()
            .filter(|path| path.exists())
            .and_then(|path| fileid_engine::db::open_read(&path).ok())
            .map(|conn| coverage_under(&conn, &root))
            .unwrap_or_default();
        let _ = tx.send_blocking(coverage);
    });
    let ui = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        let Ok(coverage) = rx.recv().await else { return };
        if let Some(text) = kept_summary(anchors, coverage.indexed, total_moves) {
            ui.kept.set_text(&text);
            ui.kept.set_visible(true);
        }
        if coverage.images_without_clip > 0 {
            let installed = clip_model_installed();
            ui.clip_label.set_text(&format!(
                "{} photo{} were scanned without CLIP, so they group by date only.",
                coverage.images_without_clip,
                if coverage.images_without_clip == 1 { "" } else { "s" }
            ));
            ui.clip_button.set_label(if installed { "Rescan" } else { "Open Settings" });
            ui.clip_banner.set_visible(true);
        }
    });
}

fn append_move(ui: &Rc<Ui>, movement: RestructureMove) {
    let check = gtk::CheckButton::new();
    check.set_active(default_selected(&movement.confidence));
    let paths = gtk::Label::builder()
        .label(format!("{}  →  {}", movement.source, movement.destination))
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
        .label(format!(
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

    let ui_for_toggle = Rc::clone(ui);
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
    use super::*;

    #[test]
    fn only_automatic_moves_are_selected_by_default() {
        assert!(default_selected("auto"));
        assert!(default_selected("AUTO"));
        assert!(!default_selected("review"));
        assert!(!default_selected("ask"));
        assert!(!default_selected(""));
    }

    #[test]
    fn kept_folders_and_staying_files_are_reported() {
        assert_eq!(kept_summary(0, 100, 10), None);
        assert_eq!(
            kept_summary(3, 1200, 200).as_deref(),
            Some("Keep 3 folders untouched · 1000 files staying where they are")
        );
        assert_eq!(kept_summary(1, 5, 9).as_deref(), Some("Keep 1 folder untouched · 0 files staying where they are"));
        assert_eq!(zero_move_status(0), "No moves were proposed for this folder.");
        assert_eq!(zero_move_status(4), "Everything here is already organized. 4 folders kept.");
    }

    #[test]
    fn coverage_counts_only_this_library_and_images_without_clip() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE files (id INTEGER PRIMARY KEY, path_text TEXT, kind TEXT, failed INTEGER);
             CREATE TABLE clip_embeddings (file_id INTEGER PRIMARY KEY);
             INSERT INTO files VALUES (1, '/lib/a.jpg', 'image', 0), (2, '/lib/b.jpg', 'image', 0),
                 (3, '/lib/c.pdf', 'pdf', 0), (4, '/library2/d.jpg', 'image', 0), (5, '/lib/e.jpg', 'image', 1);
             INSERT INTO clip_embeddings VALUES (1);",
        )
        .unwrap();
        assert_eq!(coverage_under(&conn, "/lib/"), Coverage { indexed: 3, images_without_clip: 1 });
    }
}
