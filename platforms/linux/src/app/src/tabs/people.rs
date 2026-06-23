// People tab — face-cluster viewer over `persons` + `face_prints`, the 1:1 port
// of macOS `PeopleView.swift` (+ PersonDetailSheet / SuggestedMergesSheet /
// MergeTargetPicker).
//
//   * a `gtk::FlowBox` of person cards (representative face crop + name +
//     "N photos · M faces"), read directly from the DB (the engine is the
//     single writer; the app is a reader, mirroring `library.rs`),
//   * "Group photos by face" → `runFaceClustering`,
//   * a person-detail `adw::Dialog` (structured-name `adw::EntryRow`s + photo
//     grid + "I don't know who this is") that saves via `renamePerson` /
//     `markPersonsAsUnknown` on close,
//   * "Merge people" (checkbox mode → target picker → `mergeClusters`),
//   * "Mark unknown" (checkbox mode → `markPersonsAsUnknown`), and
//   * "Suggest merges" — borderline centroid pairs computed client-side from
//     `face_prints.arcface_embedding` (cosine band 0.45–0.65), exactly
//     mirroring macOS `ClusterSuggestions`, presented in a sheet whose Merge
//     buttons fan out `mergeClusters`.
//
// Live-scan behaviour mirrors `library.rs`: the tab subscribes to engine events
// and throttle-reloads as face detection lands during a scan, then reloads on
// completion.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::engine_client::{texture_from_decoded, DecodedImage, EngineClient, EngineEvent};
use fileid_engine::ipc::{
    CommandPayload, Empty, MarkPersonsAsUnknownPayload, MergeClustersPayload, RenamePersonPayload,
};

const CARD_THUMB_PX: i32 = 256;
const PHOTO_THUMB_PX: i32 = 240;
const PERSON_FILE_LIMIT: i64 = 500;

// Cosine-similarity band the clusterer treats as "borderline" (might be the
// same person; might not) — identical to macOS `ClusterSuggestions`.
const BORDERLINE_MIN: f32 = 0.45;
const BORDERLINE_MAX: f32 = 0.65;
const VERY_LIKELY: f32 = 0.55;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Merge,
    Unknown,
}

#[derive(Clone, Default)]
struct PersonRow {
    id: i64,
    title: Option<String>,
    first_name: Option<String>,
    middle_name: Option<String>,
    last_name: Option<String>,
    suffix: Option<String>,
    name: Option<String>,
    is_unknown: bool,
    file_count: i64,
    face_count: i64,
    rep_path: Option<String>,
    rep_bbox: Option<String>,
}

impl PersonRow {
    fn structured(&self) -> String {
        [
            &self.title,
            &self.first_name,
            &self.middle_name,
            &self.last_name,
            &self.suffix,
        ]
        .iter()
        .filter_map(|o| o.as_ref())
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
    }

    fn display_name(&self) -> String {
        let s = self.structured();
        if !s.is_empty() {
            return s;
        }
        if let Some(n) = self
            .name
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
        {
            return n.to_string();
        }
        if self.is_unknown {
            "Unknown person".to_string()
        } else {
            "Unnamed person".to_string()
        }
    }

    fn has_any_name(&self) -> bool {
        !self.structured().is_empty()
            || self
                .name
                .as_ref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
    }

    fn counts(&self) -> String {
        format!(
            "{} photo{} · {} face{}",
            self.file_count,
            plural(self.file_count),
            self.face_count,
            plural(self.face_count)
        )
    }
}

#[derive(Clone, Copy)]
struct Candidate {
    a: i64,
    b: i64,
    sim: f32,
}

#[derive(Default)]
struct Snapshot {
    all_with_faces: Vec<PersonRow>,
    total_faces: i64,
}

struct Ui {
    engine: Rc<RefCell<EngineClient>>,

    persons: RefCell<Vec<PersonRow>>,
    person_by_id: RefCell<HashMap<i64, PersonRow>>,
    suggestions: RefCell<Vec<Candidate>>,
    total_faces: Cell<i64>,
    hidden_unknown: Cell<i64>,
    show_hidden: Cell<bool>,
    mode: Cell<Mode>,
    merge_checked: RefCell<HashSet<i64>>,
    unknown_checked: RefCell<HashSet<i64>>,
    reload_gen: Cell<u64>,
    clustering: Cell<bool>,
    // Keyed by (representative photo path, face bbox): two people can share a
    // representative photo but crop different faces from it, so the path alone
    // would make the second card reuse the first card's face crop.
    thumb_cache: RefCell<HashMap<(String, Option<String>), gtk::gdk::MemoryTexture>>,

    count_label: gtk::Label,
    status_label: gtk::Label,
    actions_box: gtk::Box,
    bulk_strip: gtk::Box,
    bulk_label: gtk::Label,
    bulk_button: gtk::Button,
    grid: gtk::FlowBox,
    grid_scroller: gtk::ScrolledWindow,
    empty_page: adw::StatusPage,
    no_clusters_page: adw::StatusPage,
    cluster_spinner: gtk::Spinner,
    footer: gtk::Box,
    footer_label: gtk::Label,
    footer_button: gtk::Button,
    anchor: gtk::Box,
}

pub fn build(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    // ── Header ────────────────────────────────────────────────────────────────
    let title = gtk::Label::builder()
        .label("People")
        .xalign(0.0)
        .css_classes(["title-1"])
        .build();
    let count_label = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let actions_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .halign(gtk::Align::End)
        .build();
    let title_spacer = gtk::Box::builder().hexpand(true).build();
    let title_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    title_row.append(&title);
    title_row.append(&count_label);
    title_row.append(&title_spacer);
    title_row.append(&actions_box);

    let status_label = gtk::Label::builder()
        .label("")
        .xalign(0.0)
        .visible(false)
        .css_classes(["dim-label"])
        .build();

    let bulk_label = gtk::Label::builder()
        .label("")
        .css_classes(["dim-label"])
        .build();
    let bulk_spacer = gtk::Box::builder().hexpand(true).build();
    let bulk_button = gtk::Button::builder()
        .label("")
        .css_classes(["gold-button"])
        .build();
    let bulk_strip = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .visible(false)
        .build();
    bulk_strip.append(&bulk_label);
    bulk_strip.append(&bulk_spacer);
    bulk_strip.append(&bulk_button);

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    header.append(&title_row);
    header.append(&status_label);
    header.append(&bulk_strip);

    // ── Content: grid / empty / no-clusters ──────────────────────────────────
    let grid = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(16)
        .min_children_per_line(2)
        .column_spacing(14)
        .row_spacing(14)
        .homogeneous(true)
        .valign(gtk::Align::Start)
        .build();
    let grid_scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&grid)
        .css_classes(["fileid-tab"])
        .build();

    let empty_page = adw::StatusPage::builder()
        .icon_name("system-users-symbolic")
        .title("No people yet")
        .description(
            "Pick a folder and Start Scan. As the scan runs, faces in your photos get \
             detected — they'll appear here as cards you can name.",
        )
        .vexpand(true)
        .build();

    let group_btn = gtk::Button::builder()
        .label("Group photos by face")
        .halign(gtk::Align::Center)
        .css_classes(["gold-button", "pill"])
        .build();
    let cluster_spinner = gtk::Spinner::new();
    let no_clusters_inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .build();
    no_clusters_inner.append(&group_btn);
    no_clusters_inner.append(&cluster_spinner);
    let no_clusters_page = adw::StatusPage::builder()
        .icon_name("view-grid-symbolic")
        .title("Ready to group faces")
        .description(
            "Click Group photos by face. The app compares every face to every other and \
             creates one card per person. Once grouped, open a card to add a name.",
        )
        .child(&no_clusters_inner)
        .vexpand(true)
        .build();

    // ── Hidden-unknowns footer ───────────────────────────────────────────────
    let footer_label = gtk::Label::builder()
        .label("")
        .css_classes(["dim-label"])
        .build();
    let footer_spacer = gtk::Box::builder().hexpand(true).build();
    let footer_button = gtk::Button::builder()
        .label("Show them")
        .css_classes(["flat"])
        .build();
    let footer = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .visible(false)
        .build();
    footer.append(&footer_label);
    footer.append(&footer_spacer);
    footer.append(&footer_button);

    // ── Root ─────────────────────────────────────────────────────────────────
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["fileid-tab"])
        .build();
    root.append(&header);
    root.append(&grid_scroller);
    root.append(&empty_page);
    root.append(&no_clusters_page);
    root.append(&footer);

    let ui = Rc::new(Ui {
        engine: engine.clone(),
        persons: RefCell::new(Vec::new()),
        person_by_id: RefCell::new(HashMap::new()),
        suggestions: RefCell::new(Vec::new()),
        total_faces: Cell::new(0),
        hidden_unknown: Cell::new(0),
        show_hidden: Cell::new(false),
        mode: Cell::new(Mode::Normal),
        merge_checked: RefCell::new(HashSet::new()),
        unknown_checked: RefCell::new(HashSet::new()),
        reload_gen: Cell::new(0),
        clustering: Cell::new(false),
        thumb_cache: RefCell::new(HashMap::new()),
        count_label: count_label.clone(),
        status_label: status_label.clone(),
        actions_box: actions_box.clone(),
        bulk_strip: bulk_strip.clone(),
        bulk_label: bulk_label.clone(),
        bulk_button: bulk_button.clone(),
        grid: grid.clone(),
        grid_scroller: grid_scroller.clone(),
        empty_page: empty_page.clone(),
        no_clusters_page: no_clusters_page.clone(),
        cluster_spinner: cluster_spinner.clone(),
        footer: footer.clone(),
        footer_label: footer_label.clone(),
        footer_button: footer_button.clone(),
        anchor: root.clone(),
    });

    {
        let ui = ui.clone();
        bulk_button.connect_clicked(move |_| on_bulk_clicked(&ui));
    }
    {
        let ui = ui.clone();
        group_btn.connect_clicked(move |_| start_clustering(&ui));
    }
    {
        let ui = ui.clone();
        footer_button.connect_clicked(move |_| {
            ui.show_hidden.set(!ui.show_hidden.get());
            reload(&ui);
        });
    }

    // Live-scan reloads (faces land during the scan): throttle on batches, final
    // on completion — same coalescing as `library.rs`.
    let ev_rx = ui.engine.borrow_mut().subscribe();
    {
        let ui = ui.clone();
        glib::MainContext::default().spawn_local(async move {
            let mut last = Instant::now() - Duration::from_secs(10);
            while let Ok(ev) = ev_rx.recv().await {
                match ev {
                    EngineEvent::BatchLanded(_) => {
                        if last.elapsed() >= Duration::from_millis(1200) {
                            last = Instant::now();
                            reload(&ui);
                        }
                    }
                    EngineEvent::ScanComplete(_) => reload(&ui),
                    _ => {}
                }
            }
        });
    }

    reload(&ui);
    refresh_view(&ui);
    root.upcast()
}

// ── View refresh ──────────────────────────────────────────────────────────────

fn refresh_view(ui: &Rc<Ui>) {
    let (has_persons, count_text, persons_len) = {
        let persons = ui.persons.borrow();
        (
            !persons.is_empty(),
            count_line(&persons, ui.total_faces.get()),
            persons.len(),
        )
    };
    ui.count_label.set_text(&count_text);

    let has_faces = ui.total_faces.get() > 0;
    ui.grid_scroller.set_visible(has_persons);
    ui.empty_page.set_visible(!has_persons && !has_faces);
    ui.no_clusters_page.set_visible(!has_persons && has_faces);
    if !has_persons && has_faces {
        ui.no_clusters_page.set_title(&format!(
            "{} faces detected — ready to group",
            ui.total_faces.get()
        ));
    }

    let clustering = ui.clustering.get();
    ui.cluster_spinner.set_visible(clustering);
    if clustering {
        ui.cluster_spinner.start();
    } else {
        ui.cluster_spinner.stop();
    }

    rebuild_actions(ui, persons_len);
    update_bulk_strip(ui);

    let show_footer = has_persons && ui.hidden_unknown.get() > 0;
    ui.footer.set_visible(show_footer);
    if show_footer {
        let n = ui.hidden_unknown.get();
        if ui.show_hidden.get() {
            ui.footer_label
                .set_text(&format!("{n} marked unknown — currently visible"));
            ui.footer_button.set_label("Hide them");
        } else {
            ui.footer_label.set_text(&format!("{n} hidden as unknown"));
            ui.footer_button.set_label("Show them");
        }
    }

    if has_persons {
        rebuild_grid(ui);
    }
}

fn count_line(persons: &[PersonRow], total_faces: i64) -> String {
    let p = persons.len();
    let unnamed = persons.iter().filter(|x| !x.has_any_name()).count();
    if p == 0 && total_faces == 0 {
        return String::new();
    }
    if p == 0 {
        return format!("{total_faces} faces · clustering…");
    }
    if unnamed > 0 {
        format!("{p} people · {unnamed} still unnamed")
    } else {
        format!("{p} people · all named")
    }
}

fn rebuild_actions(ui: &Rc<Ui>, person_count: usize) {
    while let Some(child) = ui.actions_box.first_child() {
        ui.actions_box.remove(&child);
    }
    if ui.mode.get() != Mode::Normal {
        let cancel = gtk::Button::with_label("Cancel");
        let ui2 = ui.clone();
        cancel.connect_clicked(move |_| set_mode(&ui2, Mode::Normal));
        ui.actions_box.append(&cancel);
        return;
    }
    if person_count >= 2 {
        let suggest = gtk::Button::builder()
            .label("Suggest merges")
            .css_classes(["gold-button"])
            .build();
        let ui2 = ui.clone();
        suggest.connect_clicked(move |b| on_suggest_clicked(&ui2, b));

        let merge = gtk::Button::with_label("Merge people");
        let ui3 = ui.clone();
        merge.connect_clicked(move |_| set_mode(&ui3, Mode::Merge));

        let unknown = gtk::Button::with_label("Mark unknown");
        let ui4 = ui.clone();
        unknown.connect_clicked(move |_| set_mode(&ui4, Mode::Unknown));

        ui.actions_box.append(&suggest);
        ui.actions_box.append(&merge);
        ui.actions_box.append(&unknown);
    }
}

fn update_bulk_strip(ui: &Rc<Ui>) {
    let mode = ui.mode.get();
    ui.bulk_strip.set_visible(mode != Mode::Normal);
    match mode {
        Mode::Merge => {
            let n = ui.merge_checked.borrow().len();
            ui.bulk_label.set_text(&format!("{n} selected to merge"));
            ui.bulk_button.set_label(&format!("Merge {n} selected"));
            ui.bulk_button.set_sensitive(n >= 2);
        }
        Mode::Unknown => {
            let n = ui.unknown_checked.borrow().len();
            ui.bulk_label.set_text(&format!("{n} selected to mark unknown"));
            ui.bulk_button.set_label(&format!("Mark {n} as unknown"));
            ui.bulk_button.set_sensitive(n >= 1);
        }
        Mode::Normal => {}
    }
}

fn set_mode(ui: &Rc<Ui>, mode: Mode) {
    ui.mode.set(mode);
    ui.merge_checked.borrow_mut().clear();
    ui.unknown_checked.borrow_mut().clear();
    refresh_view(ui);
}

fn on_bulk_clicked(ui: &Rc<Ui>) {
    match ui.mode.get() {
        Mode::Merge => {
            if ui.merge_checked.borrow().len() >= 2 {
                open_merge_target_picker(ui);
            }
        }
        Mode::Unknown => {
            let ids: Vec<i64> = ui.unknown_checked.borrow().iter().copied().collect();
            if !ids.is_empty() {
                send_cmd(
                    ui,
                    CommandPayload::MarkPersonsAsUnknown(MarkPersonsAsUnknownPayload {
                        person_ids: ids.clone(),
                    }),
                );
                set_status(
                    ui,
                    format!("Marked {} cluster{} as unknown.", ids.len(), plural(ids.len() as i64)),
                );
                set_mode(ui, Mode::Normal);
                schedule_reload_burst(ui);
            }
        }
        Mode::Normal => {}
    }
}

// ── Person grid + cards ───────────────────────────────────────────────────────

fn rebuild_grid(ui: &Rc<Ui>) {
    ui.grid.remove_all();
    let persons = ui.persons.borrow();
    for p in persons.iter() {
        let card = build_card(ui, p);
        ui.grid.append(&card);
    }
}

fn build_card(ui: &Rc<Ui>, p: &PersonRow) -> gtk::Widget {
    let pid = p.id;
    let mode = ui.mode.get();
    let checked = match mode {
        Mode::Merge => ui.merge_checked.borrow().contains(&pid),
        Mode::Unknown => ui.unknown_checked.borrow().contains(&pid),
        Mode::Normal => false,
    };

    let pic = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .height_request(150)
        .hexpand(true)
        .css_classes(["tile-thumb"])
        .build();

    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&pic));
    let check = gtk::Image::builder()
        .icon_name(if checked {
            "emblem-ok-symbolic"
        } else {
            "checkbox-symbolic"
        })
        .pixel_size(22)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_top(6)
        .margin_start(6)
        .build();
    check.set_visible(mode != Mode::Normal);
    if checked {
        check.add_css_class("gold-accent");
    }
    overlay.add_overlay(&check);

    let name = gtk::Label::builder()
        .label(p.display_name())
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes(["heading"])
        .build();
    if p.has_any_name() {
        name.add_css_class("gold-accent");
    }
    let caption = gtk::Label::builder()
        .label(p.counts())
        .xalign(0.0)
        .css_classes(["tile-caption"])
        .build();

    let vbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .width_request(150)
        .css_classes(["file-tile"])
        .build();
    vbox.append(&overlay);
    vbox.append(&name);
    vbox.append(&caption);
    if checked {
        vbox.add_css_class("file-tile-selected");
    }

    // Weak self-refs in the click handler: the gesture is owned by `vbox`, so a
    // strong capture would be a retain cycle that leaks every card on reload.
    let gesture = gtk::GestureClick::new();
    let vbox_weak = vbox.downgrade();
    let check_weak = check.downgrade();
    let ui_g = ui.clone();
    gesture.connect_released(move |_, _, _, _| {
        let (Some(vbox), Some(check)) = (vbox_weak.upgrade(), check_weak.upgrade()) else {
            return;
        };
        on_card_clicked(&ui_g, pid, &vbox, &check);
    });
    vbox.add_controller(gesture);

    match p.rep_path.clone() {
        Some(rp) => load_card_thumb(ui, &pic, rp, p.rep_bbox.clone()),
        None => pic.set_paintable(person_icon().as_ref()),
    }

    vbox.upcast()
}

fn on_card_clicked(ui: &Rc<Ui>, pid: i64, vbox: &gtk::Box, check: &gtk::Image) {
    match ui.mode.get() {
        Mode::Normal => open_person_detail(ui, pid),
        Mode::Merge => {
            let on = toggle(&ui.merge_checked, pid);
            update_check_visual(check, vbox, on);
            update_bulk_strip(ui);
        }
        Mode::Unknown => {
            let on = toggle(&ui.unknown_checked, pid);
            update_check_visual(check, vbox, on);
            update_bulk_strip(ui);
        }
    }
}

fn toggle(set: &RefCell<HashSet<i64>>, pid: i64) -> bool {
    let mut s = set.borrow_mut();
    if s.contains(&pid) {
        s.remove(&pid);
        false
    } else {
        s.insert(pid);
        true
    }
}

fn update_check_visual(check: &gtk::Image, vbox: &gtk::Box, on: bool) {
    check.set_from_icon_name(Some(if on {
        "emblem-ok-symbolic"
    } else {
        "checkbox-symbolic"
    }));
    if on {
        check.add_css_class("gold-accent");
        vbox.add_css_class("file-tile-selected");
    } else {
        check.remove_css_class("gold-accent");
        vbox.remove_css_class("file-tile-selected");
    }
}

fn load_card_thumb(ui: &Rc<Ui>, pic: &gtk::Picture, rep_path: String, bbox: Option<String>) {
    // (path, bbox) — the crop region is part of the identity (see thumb_cache).
    let key = (rep_path.clone(), bbox.clone());
    if let Some(tex) = ui.thumb_cache.borrow().get(&key).cloned() {
        pic.set_paintable(Some(&tex));
        return;
    }
    let rx = ui.engine.borrow().request_thumbnail(rep_path);
    let pic_weak = pic.downgrade();
    let ui = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        let Ok(Some(bytes)) = rx.recv().await else {
            return;
        };
        // Decode + crop + scale OFF the main loop; only Send pixel data crosses back.
        let (dtx, drx) = async_channel::bounded::<Option<DecodedImage>>(1);
        std::thread::spawn(move || {
            let _ = dtx.send_blocking(cropped_texture(bytes, bbox.as_deref(), CARD_THUMB_PX));
        });
        let Ok(Some(decoded)) = drx.recv().await else {
            return;
        };
        let tex = texture_from_decoded(&decoded);
        ui.thumb_cache.borrow_mut().insert(key, tex.clone());
        if let Some(pic) = pic_weak.upgrade() {
            pic.set_paintable(Some(&tex));
        }
    });
}

// ── Reload (DB → caches → view) ───────────────────────────────────────────────

fn reload(ui: &Rc<Ui>) {
    let g = ui.reload_gen.get().wrapping_add(1);
    ui.reload_gen.set(g);
    let rx = read_snapshot_async();
    let ui = ui.clone();
    glib::MainContext::default().spawn_local(async move {
        let snap = rx.recv().await.unwrap_or_default();
        // Latest-wins: a slower earlier read can't clobber a newer one.
        if ui.reload_gen.get() != g {
            return;
        }
        ui.total_faces.set(snap.total_faces);
        let with_faces: Vec<PersonRow> = snap
            .all_with_faces
            .into_iter()
            .filter(|p| p.face_count > 0)
            .collect();
        let hidden = with_faces.iter().filter(|p| p.is_unknown).count() as i64;
        ui.hidden_unknown.set(hidden);

        let show_hidden = ui.show_hidden.get();
        let visible: Vec<PersonRow> = with_faces
            .iter()
            .filter(|p| !p.is_unknown || show_hidden)
            .cloned()
            .collect();
        let mut map = HashMap::with_capacity(visible.len());
        for p in &visible {
            map.insert(p.id, p.clone());
        }
        *ui.person_by_id.borrow_mut() = map;
        *ui.persons.borrow_mut() = visible;

        if ui.clustering.get() && !ui.persons.borrow().is_empty() {
            ui.clustering.set(false);
        }
        refresh_view(&ui);
    });
}

fn schedule_reload_burst(ui: &Rc<Ui>) {
    for ms in [500u64, 1500, 3000] {
        let ui = ui.clone();
        glib::timeout_add_local_once(Duration::from_millis(ms), move || reload(&ui));
    }
}

fn send_cmd(ui: &Rc<Ui>, payload: CommandPayload) {
    let _ = ui.engine.borrow_mut().send(payload);
}

fn set_status(ui: &Rc<Ui>, msg: String) {
    ui.status_label.set_text(&msg);
    ui.status_label.set_visible(true);
}

// ── Clustering ────────────────────────────────────────────────────────────────

fn start_clustering(ui: &Rc<Ui>) {
    send_cmd(ui, CommandPayload::RunFaceClustering(Empty {}));
    ui.clustering.set(true);
    set_status(ui, "Grouping faces into people…".to_string());
    refresh_view(ui);
    cluster_poll(ui);
    // Hard stop so the spinner can't spin forever if no persons ever appear.
    let ui = ui.clone();
    glib::timeout_add_local_once(Duration::from_secs(45), move || {
        if ui.clustering.get() {
            ui.clustering.set(false);
            refresh_view(&ui);
        }
    });
}

// The engine doesn't surface a `faceClusteringComplete` event through the
// current `EngineEvent` set, so we poll the DB while clustering runs; the
// reload that first sees persons flips `clustering` off (see `reload`).
fn cluster_poll(ui: &Rc<Ui>) {
    if !ui.clustering.get() {
        return;
    }
    reload(ui);
    let ui = ui.clone();
    glib::timeout_add_local_once(Duration::from_millis(1500), move || cluster_poll(&ui));
}

// ── Person-detail dialog ──────────────────────────────────────────────────────

fn open_person_detail(ui: &Rc<Ui>, pid: i64) {
    let person = match ui.person_by_id.borrow().get(&pid).cloned() {
        Some(p) => p,
        None => return,
    };

    let dialog = adw::Dialog::new();
    dialog.set_title(&person.display_name());
    dialog.set_content_width(760);
    dialog.set_content_height(640);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let subtitle = gtk::Label::builder()
        .label(person.counts())
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    body.append(&subtitle);

    let group = adw::PreferencesGroup::new();
    let title_row = adw::EntryRow::builder().title("Title (Uncle, Grandma…)").build();
    let first_row = adw::EntryRow::builder().title("First name").build();
    let middle_row = adw::EntryRow::builder().title("Middle name").build();
    let last_row = adw::EntryRow::builder().title("Last name").build();
    let suffix_row = adw::EntryRow::builder().title("Suffix (Jr, III…)").build();
    title_row.set_text(person.title.as_deref().unwrap_or(""));
    first_row.set_text(
        person
            .first_name
            .as_deref()
            .or(person.name.as_deref())
            .unwrap_or(""),
    );
    middle_row.set_text(person.middle_name.as_deref().unwrap_or(""));
    last_row.set_text(person.last_name.as_deref().unwrap_or(""));
    suffix_row.set_text(person.suffix.as_deref().unwrap_or(""));
    group.add(&title_row);
    group.add(&first_row);
    group.add(&middle_row);
    group.add(&last_row);
    group.add(&suffix_row);
    body.append(&group);

    let btn_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let mark_btn = gtk::Button::with_label("I don't know who this is");
    let btn_spacer = gtk::Box::builder().hexpand(true).build();
    let done_btn = gtk::Button::builder()
        .label("Done")
        .css_classes(["gold-button"])
        .build();
    btn_row.append(&mark_btn);
    btn_row.append(&btn_spacer);
    btn_row.append(&done_btn);
    body.append(&btn_row);

    let photos = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(6)
        .min_children_per_line(2)
        .column_spacing(8)
        .row_spacing(8)
        .homogeneous(true)
        .valign(gtk::Align::Start)
        .build();
    let photos_scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hexpand(true)
        .child(&photos)
        .build();
    body.append(&photos_scroll);

    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    // Mark-unknown clears the name fields engine-side; the skip flag stops the
    // save-on-close from re-writing whatever's in the (now stale) entries.
    let skip_save = Rc::new(Cell::new(false));

    {
        let ui = ui.clone();
        let skip = skip_save.clone();
        let (t, f, m, l, s) = (
            title_row.clone(),
            first_row.clone(),
            middle_row.clone(),
            last_row.clone(),
            suffix_row.clone(),
        );
        dialog.connect_closed(move |_| {
            if skip.get() {
                return;
            }
            send_cmd(
                &ui,
                CommandPayload::RenamePerson(RenamePersonPayload {
                    person_id: pid,
                    title: norm(t.text().as_str()),
                    first_name: norm(f.text().as_str()),
                    middle_name: norm(m.text().as_str()),
                    last_name: norm(l.text().as_str()),
                    suffix: norm(s.text().as_str()),
                }),
            );
            schedule_reload_burst(&ui);
        });
    }
    {
        let dialog = dialog.clone();
        done_btn.connect_clicked(move |_| {
            dialog.close();
        });
    }
    {
        let ui = ui.clone();
        let skip = skip_save.clone();
        let dialog = dialog.clone();
        mark_btn.connect_clicked(move |_| {
            skip.set(true);
            send_cmd(
                &ui,
                CommandPayload::MarkPersonsAsUnknown(MarkPersonsAsUnknownPayload {
                    person_ids: vec![pid],
                }),
            );
            set_status(&ui, "Marked as unknown.".to_string());
            schedule_reload_burst(&ui);
            dialog.close();
        });
    }

    dialog.present(&ui.anchor);

    // Photos load off the main loop, then tiles stream in.
    let rx = read_person_files_async(pid);
    let ui = ui.clone();
    let photos_weak = photos.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let files = rx.recv().await.unwrap_or_default();
        let Some(photos) = photos_weak.upgrade() else {
            return;
        };
        for (_id, path) in files {
            let tile = build_photo_tile(&ui, &path);
            photos.append(&tile);
        }
    });
}

fn build_photo_tile(ui: &Rc<Ui>, path: &str) -> gtk::Widget {
    let pic = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .height_request(110)
        .width_request(110)
        .css_classes(["tile-thumb"])
        .build();
    let name = gtk::Label::builder()
        .label(basename(path))
        .xalign(0.5)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(14)
        .single_line_mode(true)
        .css_classes(["tile-caption"])
        .build();
    let vbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .css_classes(["file-tile"])
        .build();
    vbox.append(&pic);
    vbox.append(&name);

    let rx = ui.engine.borrow().request_thumbnail(path.to_string());
    let pic_weak = pic.downgrade();
    glib::MainContext::default().spawn_local(async move {
        let Ok(Some(bytes)) = rx.recv().await else {
            return;
        };
        // Decode + scale OFF the main loop; only Send pixel data crosses back.
        let (dtx, drx) = async_channel::bounded::<Option<DecodedImage>>(1);
        std::thread::spawn(move || {
            let _ = dtx.send_blocking(cropped_texture(bytes, None, PHOTO_THUMB_PX));
        });
        let Ok(Some(decoded)) = drx.recv().await else {
            return;
        };
        if let Some(pic) = pic_weak.upgrade() {
            pic.set_paintable(Some(&texture_from_decoded(&decoded)));
        }
    });
    vbox.upcast()
}

// ── Merge-target picker (manual merge mode) ───────────────────────────────────

fn open_merge_target_picker(ui: &Rc<Ui>) {
    let ids: Vec<i64> = ui.merge_checked.borrow().iter().copied().collect();
    if ids.len() < 2 {
        return;
    }
    let mut candidates: Vec<PersonRow> = {
        let map = ui.person_by_id.borrow();
        ids.iter().filter_map(|id| map.get(id).cloned()).collect()
    };
    // Named first (alphabetical), then unnamed (by photo count desc).
    candidates.sort_by(|a, b| {
        b.has_any_name()
            .cmp(&a.has_any_name())
            .then(b.file_count.cmp(&a.file_count))
            .then(a.id.cmp(&b.id))
    });

    let dialog = adw::Dialog::new();
    dialog.set_title("Merge into…");
    dialog.set_content_width(480);
    dialog.set_content_height(440);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();
    let info = gtk::Label::builder()
        .label(
            "Pick which person becomes the primary. The others are absorbed — their photos \
             move in, and the source clusters disappear.",
        )
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    body.append(&info);

    let listbox = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    for cand in &candidates {
        let row = adw::ActionRow::builder()
            .title(cand.display_name())
            .subtitle(cand.counts())
            .activatable(true)
            .build();
        let icon = gtk::Image::from_icon_name("avatar-default-symbolic");
        icon.add_css_class("gold-accent");
        row.add_prefix(&icon);

        let target_id = cand.id;
        let ui2 = ui.clone();
        let ids2 = ids.clone();
        let dialog2 = dialog.clone();
        row.connect_activated(move |_| {
            for src in ids2.iter().copied().filter(|s| *s != target_id) {
                send_cmd(
                    &ui2,
                    CommandPayload::MergeClusters(MergeClustersPayload {
                        source_person_id: src,
                        destination_person_id: target_id,
                    }),
                );
            }
            set_status(
                &ui2,
                format!("Merging {} clusters into one…", ids2.len()),
            );
            set_mode(&ui2, Mode::Normal);
            schedule_reload_burst(&ui2);
            dialog2.close();
        });
        listbox.append(&row);
    }
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&listbox)
        .build();
    body.append(&scroll);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));
    dialog.present(&ui.anchor);
}

// ── Suggested merges ──────────────────────────────────────────────────────────

fn on_suggest_clicked(ui: &Rc<Ui>, btn: &gtk::Button) {
    btn.set_sensitive(false);
    btn.set_label("Scanning…");
    let rx = read_candidates_async();
    let ui = ui.clone();
    let btn = btn.clone();
    glib::MainContext::default().spawn_local(async move {
        let cands = rx.recv().await.unwrap_or_default();
        btn.set_sensitive(true);
        btn.set_label("Suggest merges");
        // Don't preempt whatever the user navigated to while the scan ran.
        if ui.mode.get() != Mode::Normal {
            return;
        }
        *ui.suggestions.borrow_mut() = cands;
        open_suggested_merges(&ui);
    });
}

fn open_suggested_merges(ui: &Rc<Ui>) {
    let dialog = adw::Dialog::new();
    dialog.set_title("Suggested merges");
    dialog.set_content_width(640);
    dialog.set_content_height(560);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .build();

    let displayable: Vec<Candidate> = {
        let map = ui.person_by_id.borrow();
        ui.suggestions
            .borrow()
            .iter()
            .copied()
            .filter(|c| map.contains_key(&c.a) && map.contains_key(&c.b))
            .collect()
    };

    let sub = gtk::Label::builder()
        .label(format!(
            "{} cluster pair{} look like they might be the same person.",
            displayable.len(),
            plural(displayable.len() as i64)
        ))
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    body.append(&sub);

    let very_count = displayable.iter().filter(|c| c.sim >= VERY_LIKELY).count();
    let btn_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let merge_very = gtk::Button::builder()
        .label(format!("Merge {very_count} very-likely"))
        .css_classes(["gold-button"])
        .sensitive(very_count > 0)
        .build();
    let merge_all = gtk::Button::with_label(&format!("Merge all {}", displayable.len()));
    merge_all.set_sensitive(!displayable.is_empty());
    btn_row.append(&merge_very);
    btn_row.append(&merge_all);
    body.append(&btn_row);

    {
        let ui2 = ui.clone();
        let disp = displayable.clone();
        let dialog2 = dialog.clone();
        merge_very.connect_clicked(move |_| {
            let very: Vec<Candidate> = disp.iter().copied().filter(|c| c.sim >= VERY_LIKELY).collect();
            run_batch_merges(&ui2, &very);
            set_status(&ui2, format!("Merging {} very-likely pairs…", very.len()));
            schedule_reload_burst(&ui2);
            dialog2.close();
        });
    }
    {
        let ui2 = ui.clone();
        let disp = displayable.clone();
        let dialog2 = dialog.clone();
        merge_all.connect_clicked(move |_| {
            run_batch_merges(&ui2, &disp);
            set_status(&ui2, format!("Merging {} pairs…", disp.len()));
            schedule_reload_burst(&ui2);
            dialog2.close();
        });
    }

    let listbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    if displayable.is_empty() {
        let none = gtk::Label::builder()
            .label("No borderline pairs found — every cluster is reliably the same or different.")
            .wrap(true)
            .xalign(0.0)
            .css_classes(["dim-label"])
            .build();
        listbox.append(&none);
    } else {
        let map = ui.person_by_id.borrow();
        for c in &displayable {
            let pa = &map[&c.a];
            let pb = &map[&c.b];
            let (target, source) = preferred(pa, pb);
            let target_id = target.id;
            let source_id = source.id;
            let (row, merge_btn) = build_suggestion_row(pa, pb, c.sim);
            let ui2 = ui.clone();
            let row_weak = row.downgrade();
            merge_btn.connect_clicked(move |_| {
                send_cmd(
                    &ui2,
                    CommandPayload::MergeClusters(MergeClustersPayload {
                        source_person_id: source_id,
                        destination_person_id: target_id,
                    }),
                );
                if let Some(row) = row_weak.upgrade() {
                    row.set_visible(false);
                }
                set_status(&ui2, "Merged.".to_string());
                schedule_reload_burst(&ui2);
            });
            listbox.append(&row);
        }
    }
    let scroll = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&listbox)
        .build();
    body.append(&scroll);

    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));
    dialog.present(&ui.anchor);
}

fn build_suggestion_row(pa: &PersonRow, pb: &PersonRow, sim: f32) -> (gtk::Box, gtk::Button) {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(10)
        .margin_end(10)
        .css_classes(["glass-card"])
        .build();
    row.append(&mini_person(pa));
    let arrow = gtk::Label::builder()
        .label("↔")
        .css_classes(["dim-label"])
        .build();
    row.append(&arrow);
    row.append(&mini_person(pb));
    let spacer = gtk::Box::builder().hexpand(true).build();
    row.append(&spacer);

    let right = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(4)
        .halign(gtk::Align::End)
        .build();
    let sim_label = gtk::Label::new(None);
    sim_label.set_markup(&sim_markup(sim));
    let merge_btn = gtk::Button::builder()
        .label("Merge")
        .css_classes(["gold-button"])
        .build();
    right.append(&sim_label);
    right.append(&merge_btn);
    row.append(&right);
    (row, merge_btn)
}

fn mini_person(p: &PersonRow) -> gtk::Box {
    let b = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .width_request(170)
        .halign(gtk::Align::Start)
        .build();
    let icon = gtk::Image::from_icon_name("avatar-default-symbolic");
    icon.set_pixel_size(28);
    icon.add_css_class("gold-accent");
    let v = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(1)
        .build();
    let name = gtk::Label::builder()
        .label(p.display_name())
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .css_classes(["heading"])
        .build();
    let cnt = gtk::Label::builder()
        .label(format!("{} photo{}", p.file_count, plural(p.file_count)))
        .xalign(0.0)
        .css_classes(["tile-caption"])
        .build();
    v.append(&name);
    v.append(&cnt);
    b.append(&icon);
    b.append(&v);
    b
}

/// Named cluster wins (so a typed name sticks); else the larger photo count.
/// Returns `(target, source)`.
fn preferred<'a>(a: &'a PersonRow, b: &'a PersonRow) -> (&'a PersonRow, &'a PersonRow) {
    let (an, bn) = (a.has_any_name(), b.has_any_name());
    if an && !bn {
        return (a, b);
    }
    if bn && !an {
        return (b, a);
    }
    if a.file_count >= b.file_count {
        (a, b)
    } else {
        (b, a)
    }
}

fn run_batch_merges(ui: &Rc<Ui>, cands: &[Candidate]) {
    let plan = {
        let map = ui.person_by_id.borrow();
        plan_batch_merges(cands, &map)
    };
    for (source, destination) in plan {
        send_cmd(
            ui,
            CommandPayload::MergeClusters(MergeClustersPayload {
                source_person_id: source,
                destination_person_id: destination,
            }),
        );
    }
}

/// Union-find over candidate pairs so chained suggestions (A↔B, B↔C) collapse
/// to a single root and we never emit a merge whose source was already
/// absorbed — mirrors macOS `mergePersonsBatch`. Returns `(source, dest)` pairs.
fn plan_batch_merges(cands: &[Candidate], by_id: &HashMap<i64, PersonRow>) -> Vec<(i64, i64)> {
    let mut parent: HashMap<i64, i64> = HashMap::new();
    let mut merges = Vec::new();
    for c in cands {
        if !by_id.contains_key(&c.a) || !by_id.contains_key(&c.b) {
            continue;
        }
        parent.entry(c.a).or_insert(c.a);
        parent.entry(c.b).or_insert(c.b);
        let ra = uf_find(&mut parent, c.a);
        let rb = uf_find(&mut parent, c.b);
        if ra == rb {
            continue;
        }
        let (target, source) = preferred(&by_id[&ra], &by_id[&rb]);
        parent.insert(source.id, target.id);
        merges.push((source.id, target.id));
    }
    merges
}

fn uf_find(parent: &mut HashMap<i64, i64>, x: i64) -> i64 {
    let mut root = x;
    while let Some(&p) = parent.get(&root) {
        if p == root {
            break;
        }
        root = p;
    }
    let mut cur = x;
    while let Some(&p) = parent.get(&cur) {
        if p == root {
            break;
        }
        parent.insert(cur, root);
        cur = p;
    }
    root
}

// ── DB reads (mirror of macOS/Windows ReadStore + ClusterSuggestions) ─────────

fn read_snapshot_async() -> async_channel::Receiver<Snapshot> {
    let (tx, rx) = async_channel::bounded::<Snapshot>(1);
    std::thread::spawn(move || {
        let snap = read_snapshot().unwrap_or_default();
        let _ = tx.send_blocking(snap);
    });
    rx
}

fn read_candidates_async() -> async_channel::Receiver<Vec<Candidate>> {
    let (tx, rx) = async_channel::bounded::<Vec<Candidate>>(1);
    std::thread::spawn(move || {
        let cands = read_candidates().unwrap_or_default();
        let _ = tx.send_blocking(cands);
    });
    rx
}

fn read_person_files_async(pid: i64) -> async_channel::Receiver<Vec<(i64, String)>> {
    let (tx, rx) = async_channel::bounded::<Vec<(i64, String)>>(1);
    std::thread::spawn(move || {
        let files = read_person_files(pid).unwrap_or_default();
        let _ = tx.send_blocking(files);
    });
    rx
}

fn read_snapshot() -> anyhow::Result<Snapshot> {
    let Ok(db_path) = fileid_engine::paths::db_path() else {
        return Ok(Snapshot::default());
    };
    if !db_path.exists() {
        return Ok(Snapshot::default());
    }
    let conn = fileid_engine::db::open_read(&db_path)?;
    let total_faces: i64 = conn
        .query_row("SELECT COUNT(*) FROM face_prints", [], |r| r.get(0))
        .unwrap_or(0);

    let sql = "\
        SELECT p.id, p.title, p.first_name, p.middle_name, p.last_name, p.suffix, p.name, \
               COALESCE(p.is_unknown, 0), COALESCE(p.file_count, 0), \
               (SELECT COUNT(*) FROM face_prints fp WHERE fp.person_id = p.id), \
               f.path_text, rf.bbox \
        FROM persons p \
        LEFT JOIN face_prints rf \
          ON rf.id = COALESCE(p.representative_face_id, \
               (SELECT fp2.id FROM face_prints fp2 WHERE fp2.person_id = p.id ORDER BY fp2.id LIMIT 1)) \
        LEFT JOIN files f ON f.id = rf.file_id \
        ORDER BY \
          CASE WHEN TRIM(COALESCE(p.title,'') || COALESCE(p.first_name,'') || \
               COALESCE(p.last_name,'') || COALESCE(p.name,'')) = '' THEN 1 ELSE 0 END, \
          p.file_count DESC, p.id ASC";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([], map_person)?
        .collect::<rusqlite::Result<Vec<PersonRow>>>()?;
    Ok(Snapshot {
        all_with_faces: rows,
        total_faces,
    })
}

fn map_person(row: &rusqlite::Row<'_>) -> rusqlite::Result<PersonRow> {
    Ok(PersonRow {
        id: row.get(0)?,
        title: row.get(1)?,
        first_name: row.get(2)?,
        middle_name: row.get(3)?,
        last_name: row.get(4)?,
        suffix: row.get(5)?,
        name: row.get(6)?,
        is_unknown: row.get::<_, i64>(7)? != 0,
        file_count: row.get(8)?,
        face_count: row.get(9)?,
        rep_path: row.get(10)?,
        rep_bbox: row.get(11)?,
    })
}

fn read_person_files(pid: i64) -> anyhow::Result<Vec<(i64, String)>> {
    let Ok(db_path) = fileid_engine::paths::db_path() else {
        return Ok(Vec::new());
    };
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let conn = fileid_engine::db::open_read(&db_path)?;
    let mut stmt = conn.prepare(
        "SELECT f.id, f.path_text FROM files f \
         JOIN face_prints fp ON fp.file_id = f.id \
         WHERE fp.person_id = ?1 \
         GROUP BY f.id ORDER BY f.scanned_at DESC LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(rusqlite::params![pid, PERSON_FILE_LIMIT], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, String)>>>()?;
    Ok(rows)
}

fn read_candidates() -> anyhow::Result<Vec<Candidate>> {
    let Ok(db_path) = fileid_engine::paths::db_path() else {
        return Ok(Vec::new());
    };
    if !db_path.exists() {
        return Ok(Vec::new());
    }
    let conn = fileid_engine::db::open_read(&db_path)?;
    let mut stmt = conn.prepare(
        "SELECT person_id, arcface_embedding FROM face_prints \
         WHERE person_id IS NOT NULL AND LENGTH(arcface_embedding) > 0",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<(i64, Vec<u8>)>>>()?;
    Ok(compute_candidates(rows))
}

/// Per-cluster centroid cosine over ArcFace/SFace embeddings, keeping pairs in
/// the borderline band [0.45, 0.65]. A direct port of macOS `ClusterSuggestions`.
fn compute_candidates(rows: Vec<(i64, Vec<u8>)>) -> Vec<Candidate> {
    let decoded: Vec<(i64, Vec<f32>)> = rows
        .into_iter()
        .filter_map(|(pid, b)| {
            let v = blob_to_f32(&b);
            if v.is_empty() {
                None
            } else {
                Some((pid, v))
            }
        })
        .collect();
    let Some(dim) = decoded.first().map(|(_, v)| v.len()) else {
        return Vec::new();
    };
    if dim == 0 {
        return Vec::new();
    }

    let mut by_person: HashMap<i64, Vec<Vec<f32>>> = HashMap::new();
    for (pid, v) in decoded {
        if v.len() == dim {
            by_person.entry(pid).or_default().push(v);
        }
    }
    if by_person.len() < 2 {
        return Vec::new();
    }

    let mut centroids: Vec<(i64, Vec<f32>)> = Vec::with_capacity(by_person.len());
    for (pid, vecs) in by_person {
        let mut sum = vec![0f32; dim];
        for v in &vecs {
            for i in 0..dim {
                sum[i] += v[i];
            }
        }
        let mut norm = 0f32;
        for x in &sum {
            norm += x * x;
        }
        let inv = 1.0 / norm.sqrt().max(f32::MIN_POSITIVE);
        for x in sum.iter_mut() {
            *x *= inv;
        }
        centroids.push((pid, sum));
    }

    let mut pairs = Vec::new();
    for i in 0..centroids.len() {
        for j in (i + 1)..centroids.len() {
            let s = dot(&centroids[i].1, &centroids[j].1);
            if s >= BORDERLINE_MIN && s <= BORDERLINE_MAX {
                let a = centroids[i].0.min(centroids[j].0);
                let b = centroids[i].0.max(centroids[j].0);
                pairs.push(Candidate { a, b, sim: s });
            }
        }
    }
    pairs.sort_by(|x, y| y.sim.partial_cmp(&x.sim).unwrap_or(std::cmp::Ordering::Equal));
    pairs
}

fn blob_to_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

// ── Thumbnail decode + face crop ──────────────────────────────────────────────

/// Decode raw image bytes, crop to the (pixel, top-left-origin) face bbox with
/// 20% padding like macOS `cropFace`, then scale down to `max_px`. Decoding
/// full-res is required because the bbox is in the original image's pixel space
/// and the DB stores no dimensions to normalize against. Any failure falls back
/// to the uncropped frame, then to `None` (icon placeholder).
fn cropped_texture(bytes: Vec<u8>, bbox: Option<&str>, max_px: i32) -> Option<DecodedImage> {
    let gbytes = glib::Bytes::from_owned(bytes);
    let stream = gio::MemoryInputStream::from_bytes(&gbytes);
    let full = gtk::gdk_pixbuf::Pixbuf::from_stream(&stream, gio::Cancellable::NONE).ok()?;
    let cropped = bbox.and_then(|b| crop_to_bbox(&full, b)).unwrap_or(full);

    let (w, h) = (cropped.width(), cropped.height());
    if w <= 0 || h <= 0 {
        return None;
    }
    let longest = w.max(h);
    let scaled = if longest > max_px {
        let s = max_px as f64 / longest as f64;
        cropped.scale_simple(
            ((w as f64) * s).round() as i32,
            ((h as f64) * s).round() as i32,
            gtk::gdk_pixbuf::InterpType::Bilinear,
        )?
    } else {
        cropped
    };
    Some(DecodedImage::from_pixbuf(&scaled))
}

fn crop_to_bbox(full: &gtk::gdk_pixbuf::Pixbuf, bbox: &str) -> Option<gtk::gdk_pixbuf::Pixbuf> {
    let v: serde_json::Value = serde_json::from_str(bbox).ok()?;
    let x = v.get("x")?.as_f64()?;
    let y = v.get("y")?.as_f64()?;
    let w = v.get("w")?.as_f64()?;
    let h = v.get("h")?.as_f64()?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let iw = full.width() as f64;
    let ih = full.height() as f64;
    let pad = 0.20;
    let mut cx = (x - w * pad).max(0.0);
    let mut cy = (y - h * pad).max(0.0);
    let mut cw = w * (1.0 + 2.0 * pad);
    let mut ch = h * (1.0 + 2.0 * pad);
    if cx + cw > iw {
        cw = iw - cx;
    }
    if cy + ch > ih {
        ch = ih - cy;
    }
    let (cx, cy, cw, ch) = (cx as i32, cy as i32, cw as i32, ch as i32);
    if cw <= 4 || ch <= 4 || cx < 0 || cy < 0 || cx + cw > full.width() || cy + ch > full.height() {
        return None;
    }
    Some(full.new_subpixbuf(cx, cy, cw, ch))
}

fn person_icon() -> Option<gtk::IconPaintable> {
    let display = gtk::gdk::Display::default()?;
    let theme = gtk::IconTheme::for_display(&display);
    Some(theme.lookup_icon(
        "avatar-default-symbolic",
        &[],
        96,
        1,
        gtk::TextDirection::None,
        gtk::IconLookupFlags::empty(),
    ))
}

// ── Small helpers ─────────────────────────────────────────────────────────────

fn norm(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn sim_markup(s: f32) -> String {
    let (color, text) = if s >= 0.55 {
        ("#5FD35F", "Very likely same")
    } else if s >= 0.50 {
        ("#E8C547", "Likely same")
    } else {
        ("#E0944A", "Possibly same")
    };
    format!("<span foreground='{color}' weight='bold'>{text}</span>")
}
