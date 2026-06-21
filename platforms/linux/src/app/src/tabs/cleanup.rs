// Cleanup tab — duplicate groups, the 1:1 GTK port of macOS `CleanupView.swift`
// (and Windows `CleanupViewModel.cs` / `PerceptualGrouping.cs`).
//
// Two modes, mirroring the reference:
//   * "Exact"   — byte-identical copies grouped by `files.content_hash`
//                 (BLAKE3 ≤16 MB, else a head+tail+size composite; migration v8)
//                 plus an identical `size_bytes` guard.
//   * "Similar" — visually near-identical images grouped by the Hamming distance
//                 of their 64-bit dHash (`files.phash`), union-found into clusters
//                 (default threshold 8 of 64 bits; `FILEID_NEARDUP_HAMMING`
//                 overrides, clamped 0..20). Pure byte-exact clusters are dropped
//                 (they already appear under Exact).
//
// Each group is a glass card. The keeper (rank 0 — aesthetic ⇣, size ⇣, created
// ⇡, shortest path ⇡, path ordinal) wears a gold KEEPER badge. Tiles toggle a
// per-copy selection; the user can mass-select non-keepers (Exact only), trash a
// whole group, or trash everything selected (with a confirmation). Similar mode
// pre-selects NOTHING and shows the "review before deleting — NOT byte-identical"
// warning, because those copies are not byte-for-byte equal.
//
// Reads the DB directly (the engine is the single writer; we open a fresh
// read-only connection per query, exactly like the Library tab + macOS/Windows
// `ReadStore`). Deletion is the engine's `trashFiles` IPC command.

use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::engine_client::{
    decode_scaled, texture_from_decoded, DecodedImage, EngineClient, EngineEvent,
};
use fileid_engine::ipc::{CommandPayload, TrashFilesPayload};

/// Files larger than this carry a head+tail+size COMPOSITE `content_hash`, not a
/// full BLAKE3 — matching composites are "likely", not byte-verified. Mirror of
/// the engine's `FULL_HASH_MAX_BYTES`.
const FULL_HASH_MAX_BYTES: i64 = 16 * 1024 * 1024;
/// Default "visually similar" Hamming threshold (8 of 64 bits). `FILEID_NEARDUP_HAMMING`
/// overrides, clamped to 0..20. (mirrors macOS `defaultNearDupHamming`.)
const DEFAULT_NEARDUP_HAMMING: u32 = 8;
/// Above this image-with-dHash count the O(N²) pairwise scan is skipped rather
/// than hang the UI. (mirrors macOS `nearDupImageCap`.)
const NEAR_DUP_IMAGE_CAP: usize = 20_000;
/// Largest clusters first; cap the rendered groups like the reference.
const MAX_GROUPS: usize = 200;
const TILE_THUMB_PX: i32 = 256;
const BYTES_PER_MB: f64 = 1_048_576.0;

// ─── Data model (app-side mirror of DuplicateGroup / DuplicateMember) ─────────

#[derive(Clone)]
struct Member {
    id: i64,
    path: String,
    name: String,
    size: i64,
    modified: Option<f64>,
    kind: String,
    is_keeper: bool,
}

struct DupGroup {
    /// Stable identity: `dup-<hash>:<size>` (exact) or `sim-<min member id>`
    /// (similar) — independent of which copy currently ranks as keeper, so a
    /// mid-scan re-rank can't change the key. Drives skip-state tracking.
    key: String,
    members: Vec<Member>,
    is_similar: bool,
    is_approximate: bool,
    total_bytes: i64,
    keeper_bytes: i64,
}

impl DupGroup {
    fn reclaimable(&self) -> i64 {
        self.total_bytes - self.keeper_bytes
    }
}

struct LoadResult {
    groups: Vec<DupGroup>,
    /// Number of candidate rows considered (files with a content hash, or images
    /// with a dHash) — distinguishes "nothing scanned yet" from "no duplicates".
    candidate_count: usize,
}

impl LoadResult {
    fn empty() -> Self {
        Self { groups: Vec::new(), candidate_count: 0 }
    }
}

// ─── The tab ──────────────────────────────────────────────────────────────────

struct Cleanup {
    engine: Rc<RefCell<EngineClient>>,
    mode: RefCell<String>, // "exact" | "similar"
    groups: RefCell<Vec<DupGroup>>,
    selection: RefCell<HashSet<i64>>, // file ids selected for deletion
    skipped: RefCell<HashSet<String>>, // group keys hidden from the view
    query_gen: Cell<u64>,
    deleting: Cell<bool>,
    last_candidates: Cell<usize>,
    reload_throttle: Cell<Instant>,

    subtitle: gtk::Label,
    actions_box: gtk::Box,
    select_nonkeepers_btn: gtk::Button,
    clear_btn: gtk::Button,
    delete_btn: gtk::Button,
    status_bar: gtk::Box,
    status_label: gtk::Label,
    content_stack: gtk::Stack,
    empty_page: adw::StatusPage,
    list_box: gtk::Box,
}

pub fn build_cleanup_tab(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    // ── Header ──────────────────────────────────────────────────────────────
    let title = gtk::Label::builder()
        .label("Cleanup")
        .xalign(0.0)
        .css_classes(["title-1"])
        .build();
    let subtitle = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build();
    let title_col = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .build();
    title_col.append(&title);
    title_col.append(&subtitle);

    let pill_exact = gtk::Button::builder()
        .label("Exact")
        .css_classes(["pill", "pill-active"])
        .build();
    let pill_similar = gtk::Button::builder()
        .label("Similar")
        .css_classes(["pill"])
        .build();
    let pillbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .valign(gtk::Align::Center)
        .build();
    pillbox.append(&pill_exact);
    pillbox.append(&pill_similar);

    let filler = gtk::Box::builder().hexpand(true).build();

    let select_nonkeepers_btn = gtk::Button::builder()
        .label("Select all non-keepers")
        .css_classes(["pill"])
        .build();
    let clear_btn = gtk::Button::builder()
        .label("Clear selection")
        .css_classes(["pill"])
        .sensitive(false)
        .build();
    let delete_btn = gtk::Button::builder()
        .label("Delete 0 selected (0.0 MB)")
        .css_classes(["destructive-action"])
        .sensitive(false)
        .build();
    let actions_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .valign(gtk::Align::Center)
        .visible(false)
        .build();
    actions_box.append(&select_nonkeepers_btn);
    actions_box.append(&clear_btn);
    actions_box.append(&delete_btn);

    let header_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    header_row.append(&title_col);
    header_row.append(&pillbox);
    header_row.append(&filler);
    header_row.append(&actions_box);

    // ── Status bar (shown after a trash) ────────────────────────────────────
    let status_icon = gtk::Image::from_icon_name("user-trash-symbolic");
    let status_label = gtk::Label::builder().xalign(0.0).wrap(true).build();
    let status_filler = gtk::Box::builder().hexpand(true).build();
    let status_dismiss = gtk::Button::builder()
        .label("Dismiss")
        .css_classes(["flat"])
        .build();
    let status_bar = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .visible(false)
        .css_classes(["glass-card"])
        .build();
    status_bar.append(&status_icon);
    status_bar.append(&status_label);
    status_bar.append(&status_filler);
    status_bar.append(&status_dismiss);

    // ── Content (empty state ↔ group list) ──────────────────────────────────
    let empty_page = adw::StatusPage::builder()
        .icon_name("user-trash-symbolic")
        .title("Nothing to clean up yet")
        .vexpand(true)
        .build();

    let list_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .build();
    let list_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&list_box)
        .build();
    list_scroll.add_css_class("fileid-tab");

    let content_stack = gtk::Stack::new();
    content_stack.set_hexpand(true);
    content_stack.set_vexpand(true);
    content_stack.add_named(&empty_page, Some("empty"));
    content_stack.add_named(&list_scroll, Some("list"));

    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["fileid-tab"])
        .build();
    root.append(&header_row);
    root.append(&status_bar);
    root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    root.append(&content_stack);

    // ── Shared state ─────────────────────────────────────────────────────────
    let this = Rc::new(Cleanup {
        engine,
        mode: RefCell::new("exact".to_string()),
        groups: RefCell::new(Vec::new()),
        selection: RefCell::new(HashSet::new()),
        skipped: RefCell::new(HashSet::new()),
        query_gen: Cell::new(0),
        deleting: Cell::new(false),
        last_candidates: Cell::new(0),
        reload_throttle: Cell::new(Instant::now() - Duration::from_secs(10)),
        subtitle,
        actions_box,
        select_nonkeepers_btn: select_nonkeepers_btn.clone(),
        clear_btn: clear_btn.clone(),
        delete_btn: delete_btn.clone(),
        status_bar: status_bar.clone(),
        status_label,
        content_stack,
        empty_page,
        list_box,
    });

    // ── Mode pills ───────────────────────────────────────────────────────────
    {
        let this = this.clone();
        let pe = pill_exact.clone();
        let ps = pill_similar.clone();
        pill_exact.connect_clicked(move |_| this.switch_mode("exact", &pe, &ps));
    }
    {
        let this = this.clone();
        let pe = pill_exact.clone();
        let ps = pill_similar.clone();
        pill_similar.connect_clicked(move |_| this.switch_mode("similar", &pe, &ps));
    }

    // ── Header actions ───────────────────────────────────────────────────────
    {
        let this = this.clone();
        select_nonkeepers_btn.connect_clicked(move |_| {
            {
                let groups = this.groups.borrow();
                let skipped = this.skipped.borrow();
                let mut sel = this.selection.borrow_mut();
                sel.clear();
                for g in groups.iter().filter(|g| !skipped.contains(&g.key)) {
                    for (i, m) in g.members.iter().enumerate() {
                        if i > 0 {
                            sel.insert(m.id);
                        }
                    }
                }
            }
            this.rebuild_list();
            this.update_global_summary();
        });
    }
    {
        let this = this.clone();
        clear_btn.connect_clicked(move |_| {
            this.selection.borrow_mut().clear();
            this.rebuild_list();
            this.update_global_summary();
        });
    }
    {
        let this = this.clone();
        delete_btn.connect_clicked(move |b| this.confirm_delete_selected(b));
    }
    status_dismiss.connect_clicked({
        let bar = status_bar.clone();
        move |_| bar.set_visible(false)
    });

    // ── Live-scan reloads: throttle on batches, final reload on completion. ──
    let ev_rx = this.engine.borrow_mut().subscribe();
    {
        let this = this.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(ev) = ev_rx.recv().await {
                match ev {
                    EngineEvent::BatchLanded(_) => {
                        if this.reload_throttle.get().elapsed() >= Duration::from_millis(1200) {
                            this.reload_throttle.set(Instant::now());
                            this.reload();
                        }
                    }
                    EngineEvent::ScanComplete(_) => this.reload(),
                    _ => {}
                }
            }
        });
    }

    // Initial fill.
    this.reload();
    this.update_global_summary();

    root.upcast()
}

impl Cleanup {
    fn switch_mode(self: &Rc<Self>, mode: &str, pill_exact: &gtk::Button, pill_similar: &gtk::Button) {
        if self.mode.borrow().as_str() == mode {
            return;
        }
        *self.mode.borrow_mut() = mode.to_string();
        if mode == "exact" {
            pill_exact.add_css_class("pill-active");
            pill_similar.remove_css_class("pill-active");
        } else {
            pill_similar.add_css_class("pill-active");
            pill_exact.remove_css_class("pill-active");
        }
        // A fresh slate — nothing carries over, and Similar mode must begin with
        // NOTHING pre-selected for deletion.
        self.selection.borrow_mut().clear();
        self.skipped.borrow_mut().clear();
        self.groups.borrow_mut().clear();
        self.status_bar.set_visible(false);
        self.reload();
        self.update_global_summary();
    }

    // ── Reload (DB read + grouping off the main loop) ────────────────────────

    fn reload(self: &Rc<Self>) {
        let g = self.query_gen.get().wrapping_add(1);
        self.query_gen.set(g);
        let mode_similar = self.mode.borrow().as_str() == "similar";

        let (tx, rx) = async_channel::bounded::<LoadResult>(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(load(mode_similar));
        });

        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let res = match rx.recv().await {
                Ok(r) => r,
                Err(_) => return,
            };
            // Latest-wins: a slower earlier query can't clobber a newer one.
            if this.query_gen.get() != g {
                return;
            }
            this.last_candidates.set(res.candidate_count);
            // Prune any selection that no longer maps to a visible copy.
            {
                let visible_ids: HashSet<i64> = res
                    .groups
                    .iter()
                    .flat_map(|x| x.members.iter().map(|m| m.id))
                    .collect();
                this.selection
                    .borrow_mut()
                    .retain(|id| visible_ids.contains(id));
            }
            *this.groups.borrow_mut() = res.groups;
            this.rebuild_list();
            this.update_global_summary();
        });
    }

    // ── Rebuild the visible card list ────────────────────────────────────────

    fn rebuild_list(self: &Rc<Self>) {
        while let Some(child) = self.list_box.first_child() {
            self.list_box.remove(&child);
        }

        let mode_similar = self.mode.borrow().as_str() == "similar";
        let groups = self.groups.borrow();
        let skipped = self.skipped.borrow();
        let visible: Vec<&DupGroup> = groups.iter().filter(|g| !skipped.contains(&g.key)).collect();

        if visible.is_empty() {
            self.show_empty(mode_similar, !groups.is_empty());
            return;
        }

        self.content_stack.set_visible_child_name("list");
        self.list_box.append(&build_banner(mode_similar));
        for g in &visible {
            let card = self.build_group_card(g);
            self.list_box.append(&card);
        }
    }

    fn show_empty(self: &Rc<Self>, mode_similar: bool, has_skipped_only: bool) {
        let candidates = self.last_candidates.get();
        if has_skipped_only {
            self.empty_page.set_icon_name(Some("emblem-ok-symbolic"));
            self.empty_page.set_title("All duplicate groups skipped");
            self.empty_page
                .set_description(Some("You've hidden every group from this view."));
            let btn = gtk::Button::builder()
                .label("Show skipped groups again")
                .halign(gtk::Align::Center)
                .css_classes(["pill"])
                .build();
            let this = self.clone();
            btn.connect_clicked(move |_| {
                this.skipped.borrow_mut().clear();
                this.rebuild_list();
                this.update_global_summary();
            });
            self.empty_page.set_child(Some(&btn));
        } else if candidates == 0 {
            self.empty_page.set_child(None::<&gtk::Box>);
            self.empty_page.set_icon_name(Some("user-trash-symbolic"));
            self.empty_page.set_title("Nothing to clean up yet");
            self.empty_page.set_description(Some(
                "Pick a folder in the sidebar and click Start Scan. Duplicate copies show up \
                 here grouped together — pick which copy to keep.",
            ));
        } else {
            self.empty_page.set_child(None::<&gtk::Box>);
            self.empty_page.set_icon_name(Some("emblem-ok-symbolic"));
            if mode_similar {
                self.empty_page.set_title("No visually similar images found");
                let desc = format!(
                    "All {candidates} images compared — none are near-identical within the \
                     similarity threshold. Byte-for-byte duplicates appear under \"Exact\"."
                );
                self.empty_page.set_description(Some(desc.as_str()));
            } else {
                self.empty_page.set_title("No duplicates found");
                let desc =
                    format!("All {candidates} files compared — none are byte-for-byte identical.");
                self.empty_page.set_description(Some(desc.as_str()));
            }
        }
        self.content_stack.set_visible_child_name("empty");
    }

    // ── A single group card ──────────────────────────────────────────────────

    fn build_group_card(self: &Rc<Self>, group: &DupGroup) -> gtk::Widget {
        let card = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .css_classes(["glass-card"])
            .build();

        // Header: count badge + caution badge + size info + (live) selected count.
        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(8)
            .build();
        let count_text = format!(
            "{} {}",
            group.members.len(),
            if group.is_similar { "images" } else { "copies" }
        );
        let count_badge = gtk::Label::builder()
            .label(count_text.as_str())
            .css_classes(["kind-badge"])
            .valign(gtk::Align::Center)
            .build();
        head.append(&count_badge);
        if group.is_similar {
            head.append(
                &gtk::Label::builder()
                    .label("Visually similar")
                    .css_classes(["pill"])
                    .valign(gtk::Align::Center)
                    .build(),
            );
        } else if group.is_approximate {
            head.append(
                &gtk::Label::builder()
                    .label("~ likely match")
                    .css_classes(["pill"])
                    .valign(gtk::Align::Center)
                    .build(),
            );
        }
        let size_text = format!(
            "{:.1} MB total · {:.1} MB if you keep 1",
            group.total_bytes as f64 / BYTES_PER_MB,
            group.reclaimable() as f64 / BYTES_PER_MB,
        );
        head.append(
            &gtk::Label::builder()
                .label(size_text.as_str())
                .css_classes(["dim-label"])
                .valign(gtk::Align::Center)
                .build(),
        );
        head.append(&gtk::Box::builder().hexpand(true).build());
        let sel_lbl = gtk::Label::builder()
            .css_classes(["gold-accent"])
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        head.append(&sel_lbl);
        card.append(&head);

        // Controls: select menu + per-group delete + skip.
        let member_ids: Rc<Vec<i64>> = Rc::new(group.members.iter().map(|m| m.id).collect());
        let del_btn = gtk::Button::builder()
            .label("Delete 0 from this group")
            .css_classes(["destructive-action"])
            .sensitive(false)
            .build();

        let controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        controls.append(&self.build_select_menu(member_ids.clone()));
        controls.append(&del_btn);
        let skip_btn = gtk::Button::builder()
            .label("Skip group")
            .css_classes(["pill"])
            .build();
        controls.append(&skip_btn);
        card.append(&controls);

        // Tiles.
        let tiles = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .build();
        let group_sizes: Rc<Vec<(i64, i64)>> =
            Rc::new(group.members.iter().map(|m| (m.id, m.size)).collect());
        for m in &group.members {
            let tile = self.build_tile(m, group_sizes.clone(), sel_lbl.clone(), del_btn.clone());
            tiles.append(&tile);
        }
        let tiles_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .min_content_height(206)
            .child(&tiles)
            .build();
        tiles_scroll.add_css_class("fileid-tab");
        card.append(&tiles_scroll);

        // Initial per-group selected state.
        update_group_selection_widgets(&self.selection.borrow(), &group_sizes, &sel_lbl, &del_btn);

        // Per-group delete (no confirmation, mirroring the reference).
        {
            let this = self.clone();
            let ids = member_ids.clone();
            del_btn.connect_clicked(move |_| {
                let to_trash: Vec<i64> = {
                    let sel = this.selection.borrow();
                    ids.iter().copied().filter(|id| sel.contains(id)).collect()
                };
                this.trash(to_trash);
            });
        }
        // Skip group.
        {
            let this = self.clone();
            let key = group.key.clone();
            skip_btn.connect_clicked(move |_| {
                this.skipped.borrow_mut().insert(key.clone());
                this.rebuild_list();
                this.update_global_summary();
            });
        }

        card.upcast()
    }

    fn build_select_menu(self: &Rc<Self>, member_ids: Rc<Vec<i64>>) -> gtk::MenuButton {
        let popover = gtk::Popover::new();
        let popbox = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .build();

        let make = |label: &str| {
            gtk::Button::builder()
                .label(label)
                .css_classes(["flat"])
                .build()
        };
        let b_except = make("All except keeper");
        let b_all = make("All");
        let b_none = make("None");
        let b_invert = make("Invert");
        popbox.append(&b_except);
        popbox.append(&b_all);
        popbox.append(&b_none);
        popbox.append(&b_invert);
        popover.set_child(Some(&popbox));

        wire_select(&b_except, self, &member_ids, &popover, SelectOp::AllExceptKeeper);
        wire_select(&b_all, self, &member_ids, &popover, SelectOp::All);
        wire_select(&b_none, self, &member_ids, &popover, SelectOp::None);
        wire_select(&b_invert, self, &member_ids, &popover, SelectOp::Invert);

        let mb = gtk::MenuButton::builder()
            .label("Select…")
            .css_classes(["pill"])
            .build();
        mb.set_popover(Some(&popover));
        mb
    }

    fn build_tile(
        self: &Rc<Self>,
        member: &Member,
        group_sizes: Rc<Vec<(i64, i64)>>,
        sel_lbl: gtk::Label,
        del_btn: gtk::Button,
    ) -> gtk::Widget {
        let selected_now = self.selection.borrow().contains(&member.id);

        let outer = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .width_request(148)
            .css_classes(["file-tile"])
            .build();
        if selected_now {
            outer.add_css_class("file-tile-selected");
        }

        let pic = gtk::Picture::builder()
            .content_fit(gtk::ContentFit::Cover)
            .width_request(132)
            .height_request(132)
            .css_classes(["tile-thumb"])
            .build();

        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&pic));
        if member.is_keeper {
            overlay.add_overlay(
                &gtk::Label::builder()
                    .label("KEEPER")
                    .css_classes(["kind-badge"])
                    .halign(gtk::Align::Start)
                    .valign(gtk::Align::Start)
                    .margin_top(6)
                    .margin_start(6)
                    .build(),
            );
        }
        let indicator = gtk::Image::builder()
            .icon_name(checkbox_icon(selected_now))
            .pixel_size(22)
            .halign(gtk::Align::End)
            .valign(gtk::Align::Start)
            .margin_top(6)
            .margin_end(6)
            .build();
        overlay.add_overlay(&indicator);
        outer.append(&overlay);

        let name = gtk::Label::builder()
            .label(member.name.as_str())
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(18)
            .single_line_mode(true)
            .build();
        outer.append(&name);

        let meta = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(4)
            .build();
        let size_caption = format_bytes(member.size);
        meta.append(
            &gtk::Label::builder()
                .label(size_caption.as_str())
                .css_classes(["tile-caption"])
                .build(),
        );
        if let Some(d) = fmt_date(member.modified) {
            meta.append(&gtk::Box::builder().hexpand(true).build());
            meta.append(
                &gtk::Label::builder()
                    .label(d.as_str())
                    .css_classes(["tile-caption"])
                    .build(),
            );
        }
        outer.append(&meta);

        // Thumbnail (images decode client-side; everything else gets an icon).
        if member.kind == "image" {
            let rx = self.engine.borrow().request_thumbnail(member.path.clone());
            let pic_weak = pic.downgrade();
            glib::MainContext::default().spawn_local(async move {
                let Ok(Some(bytes)) = rx.recv().await else { return };
                // Decode + scale OFF the main loop; only Send pixel data crosses back.
                let (dtx, drx) = async_channel::bounded::<Option<DecodedImage>>(1);
                std::thread::spawn(move || {
                    let _ = dtx.send_blocking(decode_scaled(&bytes, TILE_THUMB_PX));
                });
                let Ok(Some(decoded)) = drx.recv().await else { return };
                if let Some(p) = pic_weak.upgrade() {
                    p.set_paintable(Some(&texture_from_decoded(&decoded)));
                }
            });
        } else {
            pic.set_paintable(icon_paintable(icon_for_kind(&member.kind), 96).as_ref());
        }

        // Whole-tile click toggles this copy's selection.
        let gesture = gtk::GestureClick::new();
        let this = self.clone();
        let id = member.id;
        let outer_weak = outer.downgrade();
        let ind_weak = indicator.downgrade();
        gesture.connect_released(move |_, _, _, _| {
            let now_selected = {
                let mut sel = this.selection.borrow_mut();
                if sel.contains(&id) {
                    sel.remove(&id);
                    false
                } else {
                    sel.insert(id);
                    true
                }
            };
            if let Some(o) = outer_weak.upgrade() {
                if now_selected {
                    o.add_css_class("file-tile-selected");
                } else {
                    o.remove_css_class("file-tile-selected");
                }
            }
            if let Some(im) = ind_weak.upgrade() {
                im.set_icon_name(Some(checkbox_icon(now_selected)));
            }
            update_group_selection_widgets(&this.selection.borrow(), &group_sizes, &sel_lbl, &del_btn);
            this.update_global_summary();
        });
        outer.add_controller(gesture);

        outer.upcast()
    }

    // ── Header summary ───────────────────────────────────────────────────────

    fn update_global_summary(self: &Rc<Self>) {
        let mode_similar = self.mode.borrow().as_str() == "similar";
        let groups = self.groups.borrow();
        let skipped = self.skipped.borrow();
        let sel = self.selection.borrow();

        let visible: Vec<&DupGroup> = groups.iter().filter(|g| !skipped.contains(&g.key)).collect();
        let mut total_sel = 0i64;
        let mut total_sel_bytes = 0i64;
        let mut reclaimable = 0i64;
        for g in &visible {
            reclaimable += g.reclaimable();
            for m in &g.members {
                if sel.contains(&m.id) {
                    total_sel += 1;
                    total_sel_bytes += m.size;
                }
            }
        }
        let n_groups = visible.len();
        let n_skipped = skipped.len();

        let mut subtitle = if mode_similar {
            if n_groups == 0 {
                "Visually similar images — resizes, re-encodes, crops, and light edits that \
                 byte-exact matching misses"
                    .to_string()
            } else {
                format!(
                    "{n_groups} similar group{} · review each before deleting — these are NOT \
                     byte-identical",
                    plural(n_groups)
                )
            }
        } else {
            format!(
                "{n_groups} duplicate group{} · {:.1} MB reclaimable if you keep 1 per group",
                plural(n_groups),
                reclaimable as f64 / BYTES_PER_MB,
            )
        };
        if n_skipped > 0 {
            subtitle.push_str(&format!(" · {n_skipped} skipped"));
        }
        self.subtitle.set_text(&subtitle);

        self.delete_btn.set_label(&format!(
            "Delete {total_sel} selected ({:.1} MB)",
            total_sel_bytes as f64 / BYTES_PER_MB
        ));
        self.delete_btn.set_sensitive(total_sel > 0);
        self.clear_btn.set_sensitive(total_sel > 0);

        let has_groups = !visible.is_empty();
        self.actions_box.set_visible(has_groups);
        // Bulk "select every non-keeper" is hidden in Similar mode: those copies
        // are NOT byte-identical, so one-click mass selection would be unsafe.
        self.select_nonkeepers_btn
            .set_visible(has_groups && !mode_similar);
    }

    // ── Trash (engine `trashFiles` IPC) ──────────────────────────────────────

    fn confirm_delete_selected(self: &Rc<Self>, anchor: &gtk::Button) {
        let ids: Vec<i64> = {
            let groups = self.groups.borrow();
            let skipped = self.skipped.borrow();
            let sel = self.selection.borrow();
            groups
                .iter()
                .filter(|g| !skipped.contains(&g.key))
                .flat_map(|g| g.members.iter())
                .filter(|m| sel.contains(&m.id))
                .map(|m| m.id)
                .collect()
        };
        if ids.is_empty() {
            return;
        }
        let bytes = self.selected_bytes(&ids);
        let n = ids.len();
        let heading = format!("Move {n} file{} to Trash?", plural(n));
        let body = format!(
            "Moves the selected cop{} to Trash. Frees about {:.1} MB. You can restore them \
             from Trash if you change your mind.",
            if n == 1 { "y" } else { "ies" },
            bytes as f64 / BYTES_PER_MB,
        );
        let dialog = adw::AlertDialog::new(Some(heading.as_str()), Some(body.as_str()));
        dialog.add_responses(&[("cancel", "Cancel"), ("trash", "Move to Trash")]);
        dialog.set_response_appearance("trash", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let this = self.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp == "trash" {
                this.trash(ids.clone());
            }
        });
        dialog.present(anchor);
    }

    fn selected_bytes(&self, ids: &[i64]) -> i64 {
        let idset: HashSet<i64> = ids.iter().copied().collect();
        self.groups
            .borrow()
            .iter()
            .flat_map(|g| g.members.iter())
            .filter(|m| idset.contains(&m.id))
            .map(|m| m.size)
            .sum()
    }

    fn trash(self: &Rc<Self>, ids: Vec<i64>) {
        // Single-flight guard: the delete buttons have no second confirmation, so
        // a rapid double-tap would otherwise trash the same ids twice.
        if ids.is_empty() || self.deleting.get() {
            return;
        }
        self.deleting.set(true);

        let freed = self.selected_bytes(&ids);
        let _ = self
            .engine
            .borrow_mut()
            .send(CommandPayload::TrashFiles(TrashFilesPayload {
                file_ids: ids.clone(),
            }));

        // Optimistic local prune — the delayed reconcile reload re-reads the DB
        // to reflect what the engine actually trashed/pruned.
        let idset: HashSet<i64> = ids.iter().copied().collect();
        {
            let mut sel = self.selection.borrow_mut();
            for id in &ids {
                sel.remove(id);
            }
        }
        {
            let mut groups = self.groups.borrow_mut();
            for g in groups.iter_mut() {
                g.members.retain(|m| !idset.contains(&m.id));
            }
            groups.retain(|g| g.members.len() >= 2);
            for g in groups.iter_mut() {
                recompute_group(g);
            }
        }
        self.rebuild_list();
        self.update_global_summary();

        let n = ids.len();
        self.status_label.set_text(&format!(
            "Trashed {n} file{} · freed {:.1} MB · restore from Trash to undo",
            plural(n),
            freed as f64 / BYTES_PER_MB,
        ));
        self.status_bar.set_visible(true);
        // Springy reveal on the shared brand spring (the project's motion
        // signature) — a discrete, user-triggered event, so it never re-fires
        // during a scan the way a per-card animation would.
        let bar = self.status_bar.clone();
        let _ = crate::spring::animate(&self.status_bar, 0.0, 1.0, move |v| bar.set_opacity(v));
        self.deleting.set(false);

        // Reconcile against the DB once the engine has processed the deletion.
        let this = self.clone();
        glib::timeout_add_local_once(Duration::from_millis(1000), move || this.reload());
    }
}

#[derive(Clone, Copy)]
enum SelectOp {
    AllExceptKeeper,
    All,
    None,
    Invert,
}

/// Wire a group "Select…" menu item: `op` mutates the selection set for this
/// group's members, then rebuilds. A nested-free fn (not a closure) so its
/// reference parameters don't snag closure-lifetime inference.
fn wire_select(
    btn: &gtk::Button,
    this: &Rc<Cleanup>,
    ids: &Rc<Vec<i64>>,
    pop: &gtk::Popover,
    op: SelectOp,
) {
    let this = this.clone();
    let ids = ids.clone();
    let pop = pop.clone();
    btn.connect_clicked(move |_| {
        {
            let mut sel = this.selection.borrow_mut();
            match op {
                SelectOp::AllExceptKeeper => {
                    for (i, id) in ids.iter().enumerate() {
                        if i == 0 {
                            sel.remove(id);
                        } else {
                            sel.insert(*id);
                        }
                    }
                }
                SelectOp::All => {
                    for id in ids.iter() {
                        sel.insert(*id);
                    }
                }
                SelectOp::None => {
                    for id in ids.iter() {
                        sel.remove(id);
                    }
                }
                SelectOp::Invert => {
                    for id in ids.iter() {
                        if sel.contains(id) {
                            sel.remove(id);
                        } else {
                            sel.insert(*id);
                        }
                    }
                }
            }
        }
        pop.popdown();
        this.rebuild_list();
        this.update_global_summary();
    });
}

fn update_group_selection_widgets(
    selection: &HashSet<i64>,
    group_sizes: &[(i64, i64)],
    sel_lbl: &gtk::Label,
    del_btn: &gtk::Button,
) {
    let (cnt, bytes) = group_sizes.iter().fold((0i64, 0i64), |(c, b), (id, sz)| {
        if selection.contains(id) {
            (c + 1, b + sz)
        } else {
            (c, b)
        }
    });
    sel_lbl.set_text(&format!("{cnt} selected · {:.1} MB", bytes as f64 / BYTES_PER_MB));
    sel_lbl.set_visible(cnt > 0);
    del_btn.set_label(&format!("Delete {cnt} from this group"));
    del_btn.set_sensitive(cnt > 0);
}

fn build_banner(mode_similar: bool) -> gtk::Widget {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(10)
        .margin_bottom(10)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["glass-card"])
        .build();
    let (icon, css, text) = if mode_similar {
        (
            "dialog-warning-symbolic",
            "gold-accent",
            "Visually similar — review before deleting (not identical). These images match by \
             perceptual hash (resizes, re-encodes, crops, light edits), NOT byte-for-byte. \
             Nothing is pre-selected: confirm each is a true duplicate, then choose which copies \
             to Trash.",
        )
    } else {
        (
            "dialog-information-symbolic",
            "lavender-accent",
            "Each group is a set of duplicate copies. The KEEPER (gold badge) is the copy we \
             recommend you keep — usually the largest / highest quality. Selected copies move to \
             Trash; you can restore them if you change your mind.",
        )
    };
    row.append(
        &gtk::Image::builder()
            .icon_name(icon)
            .valign(gtk::Align::Start)
            .css_classes([css])
            .build(),
    );
    row.append(
        &gtk::Label::builder()
            .label(text)
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .hexpand(true)
            .build(),
    );
    row.upcast()
}

// ─── DB read + grouping (off the main loop) ──────────────────────────────────

fn load(mode_similar: bool) -> LoadResult {
    let Ok(db_path) = fileid_engine::paths::db_path() else {
        return LoadResult::empty();
    };
    if !db_path.exists() {
        return LoadResult::empty(); // no scan yet
    }
    let Ok(conn) = fileid_engine::db::open_read(&db_path) else {
        return LoadResult::empty();
    };
    if mode_similar {
        load_similar(&conn)
    } else {
        load_exact(&conn)
    }
}

struct RawRow {
    id: i64,
    path: String,
    size: i64,
    modified: Option<f64>,
    created: Option<f64>,
    aesthetic: Option<f64>,
    kind: String,
    phash: i64,
    hash: Option<Vec<u8>>,
}

fn load_exact(conn: &rusqlite::Connection) -> LoadResult {
    // Every file with a content hash; group by EXACT (hash + size) equality —
    // identical content_hash AND size_bytes is byte-for-byte identical.
    let sql = "SELECT id, path_text, size_bytes, content_hash, modified_at, created_at, aesthetic, kind \
               FROM files WHERE content_hash IS NOT NULL AND failed = 0";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return LoadResult::empty();
    };
    let rows = stmt.query_map([], |row| {
        Ok(RawRow {
            id: row.get(0)?,
            path: row.get(1)?,
            size: row.get(2)?,
            hash: row.get::<_, Option<Vec<u8>>>(3)?,
            modified: row.get(4)?,
            created: row.get(5)?,
            aesthetic: row.get(6)?,
            kind: row.get(7)?,
            phash: 0,
        })
    });
    let Ok(rows) = rows else {
        return LoadResult::empty();
    };

    let mut raw: Vec<RawRow> = Vec::new();
    for r in rows.flatten() {
        match &r.hash {
            Some(h) if !h.is_empty() => raw.push(r),
            _ => {}
        }
    }
    let candidate_count = raw.len();

    // Group by content_hash hex + size (O(n) via a map).
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, r) in raw.iter().enumerate() {
        let key = format!("{}:{}", hex(r.hash.as_deref().unwrap_or(&[])), r.size);
        by_key.entry(key).or_default().push(i);
    }

    let mut groups: Vec<DupGroup> = Vec::new();
    for (_, mut indices) in by_key {
        if indices.len() < 2 {
            continue;
        }
        rank_indices(&raw, &mut indices);
        let keeper = &raw[indices[0]];
        let key = format!("dup-{}:{}", hex(keeper.hash.as_deref().unwrap_or(&[])), keeper.size);
        let is_approximate = keeper.size > FULL_HASH_MAX_BYTES;
        groups.push(build_group(&raw, &indices, key, false, is_approximate));
    }

    finalize(groups, candidate_count)
}

fn load_similar(conn: &rusqlite::Connection) -> LoadResult {
    // Only images carry a dHash; phash == 0 is the engine's "none / failed"
    // sentinel — exclude it so blank hashes don't collapse into one giant group.
    let sql = "SELECT id, path_text, size_bytes, content_hash, modified_at, created_at, aesthetic, phash, kind \
               FROM files \
               WHERE kind = 'image' AND failed = 0 AND phash IS NOT NULL AND phash != 0";
    let Ok(mut stmt) = conn.prepare(sql) else {
        return LoadResult::empty();
    };
    let rows = stmt.query_map([], |row| {
        Ok(RawRow {
            id: row.get(0)?,
            path: row.get(1)?,
            size: row.get(2)?,
            hash: row.get::<_, Option<Vec<u8>>>(3)?,
            modified: row.get(4)?,
            created: row.get(5)?,
            aesthetic: row.get(6)?,
            phash: row.get(7)?,
            kind: row.get(8)?,
        })
    });
    let Ok(rows) = rows else {
        return LoadResult::empty();
    };

    let raw: Vec<RawRow> = rows.flatten().filter(|r| r.phash != 0).collect();
    let candidate_count = raw.len();
    if raw.len() <= 1 || raw.len() > NEAR_DUP_IMAGE_CAP {
        // Empty or beyond the O(N²) cap — skip perceptual grouping (Exact stays).
        return LoadResult {
            groups: Vec::new(),
            candidate_count,
        };
    }

    let max_hamming = near_dup_threshold();
    let mut index_by_id: HashMap<i64, usize> = HashMap::with_capacity(raw.len());
    let mut items: Vec<(i64, i64)> = Vec::with_capacity(raw.len());
    for (i, r) in raw.iter().enumerate() {
        index_by_id.insert(r.id, i);
        items.push((r.id, r.phash));
    }

    let mut groups: Vec<DupGroup> = Vec::new();
    for ids in group_by_hamming(&items, max_hamming) {
        let mut indices: Vec<usize> = ids
            .iter()
            .filter_map(|id| index_by_id.get(id).copied())
            .collect();
        if indices.len() < 2 {
            continue;
        }
        // Drop pure byte-exact clusters (all members share one non-null
        // content_hash) — they already appear under "Exact".
        if all_byte_exact(&raw, &indices) {
            continue;
        }
        rank_indices(&raw, &mut indices);
        // Stable identity: smallest member id, independent of keeper re-ranks.
        let gid = indices.iter().map(|&i| raw[i].id).min().unwrap_or(0);
        groups.push(build_group(&raw, &indices, format!("sim-{gid}"), true, false));
    }

    finalize(groups, candidate_count)
}

fn finalize(mut groups: Vec<DupGroup>, candidate_count: usize) -> LoadResult {
    groups.sort_by(|a, b| b.members.len().cmp(&a.members.len()));
    if groups.len() > MAX_GROUPS {
        groups.truncate(MAX_GROUPS);
    }
    LoadResult {
        groups,
        candidate_count,
    }
}

fn build_group(
    raw: &[RawRow],
    indices: &[usize],
    key: String,
    is_similar: bool,
    is_approximate: bool,
) -> DupGroup {
    let mut members = Vec::with_capacity(indices.len());
    let mut total_bytes = 0i64;
    for (k, &i) in indices.iter().enumerate() {
        let r = &raw[i];
        total_bytes += r.size;
        members.push(Member {
            id: r.id,
            path: r.path.clone(),
            name: file_name(&r.path),
            size: r.size,
            modified: r.modified,
            kind: r.kind.clone(),
            is_keeper: k == 0,
        });
    }
    let keeper_bytes = members.first().map(|m| m.size).unwrap_or(0);
    DupGroup {
        key,
        members,
        is_similar,
        is_approximate,
        total_bytes,
        keeper_bytes,
    }
}

/// Recompute keeper flags + totals after an optimistic member removal. Members
/// stay in rank order, so element 0 is the best surviving keeper.
fn recompute_group(g: &mut DupGroup) {
    g.total_bytes = g.members.iter().map(|m| m.size).sum();
    g.keeper_bytes = g.members.first().map(|m| m.size).unwrap_or(0);
    for (i, m) in g.members.iter_mut().enumerate() {
        m.is_keeper = i == 0;
    }
}

/// Keeper rank (macOS / Windows parity): aesthetic DESC, size DESC, earliest
/// created_at ASC, shortest path ASC, then path ordinal as a stable tiebreak.
fn rank_indices(raw: &[RawRow], indices: &mut [usize]) {
    indices.sort_by(|&a, &b| {
        let ra = &raw[a];
        let rb = &raw[b];
        rb.aesthetic
            .unwrap_or(0.0)
            .partial_cmp(&ra.aesthetic.unwrap_or(0.0))
            .unwrap_or(Ordering::Equal)
            .then_with(|| rb.size.cmp(&ra.size))
            .then_with(|| {
                ra.created
                    .unwrap_or(f64::MAX)
                    .partial_cmp(&rb.created.unwrap_or(f64::MAX))
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| ra.path.len().cmp(&rb.path.len()))
            .then_with(|| ra.path.cmp(&rb.path))
    });
}

fn all_byte_exact(raw: &[RawRow], indices: &[usize]) -> bool {
    let mut first: Option<String> = None;
    for &i in indices {
        match raw[i].hash.as_deref() {
            Some(h) if !h.is_empty() => {
                let hx = hex(h);
                match &first {
                    None => first = Some(hx),
                    Some(f) if *f != hx => return false,
                    _ => {}
                }
            }
            _ => return false,
        }
    }
    true
}

/// Union-find clustering of dHashes within `max_hamming` (transitively). Returns
/// groups of size ≥ 2 in first-seen order. (Direct port of `PerceptualGrouping`.)
fn group_by_hamming(items: &[(i64, i64)], max_hamming: u32) -> Vec<Vec<i64>> {
    let n = items.len();
    if n <= 1 {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], x: usize) -> usize {
        let mut r = x;
        while parent[r] != r {
            parent[r] = parent[parent[r]];
            r = parent[r];
        }
        r
    }

    for i in 0..n {
        for j in (i + 1)..n {
            if hamming(items[i].1, items[j].1) <= max_hamming {
                let ra = find(&mut parent, i);
                let rb = find(&mut parent, j);
                if ra != rb {
                    // Point the higher index at the lower so every root is its
                    // smallest member — keeps group order deterministic.
                    if ra < rb {
                        parent[rb] = ra;
                    } else {
                        parent[ra] = rb;
                    }
                }
            }
        }
    }

    let mut order: Vec<usize> = Vec::new();
    let mut members_by_root: HashMap<usize, Vec<i64>> = HashMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        if !members_by_root.contains_key(&r) {
            order.push(r);
            members_by_root.insert(r, Vec::new());
        }
        members_by_root.get_mut(&r).unwrap().push(items[i].0);
    }

    let mut groups = Vec::new();
    for root in order {
        if let Some(ids) = members_by_root.remove(&root) {
            if ids.len() >= 2 {
                groups.push(ids);
            }
        }
    }
    groups
}

/// popcount(a XOR b) over the raw 64 bits — the dHashes' bit patterns, not their
/// signed values (the sign bit is just bit 63 of the hash).
fn hamming(a: i64, b: i64) -> u32 {
    ((a as u64) ^ (b as u64)).count_ones()
}

fn near_dup_threshold() -> u32 {
    if let Ok(raw) = std::env::var("FILEID_NEARDUP_HAMMING") {
        if let Ok(v) = raw.trim().parse::<i64>() {
            return v.clamp(0, 20) as u32;
        }
    }
    DEFAULT_NEARDUP_HAMMING
}

// ─── Small helpers (mirrors of library.rs) ───────────────────────────────────

fn checkbox_icon(selected: bool) -> &'static str {
    if selected {
        "checkbox-checked-symbolic"
    } else {
        "checkbox-symbolic"
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn icon_paintable(name: &str, size: i32) -> Option<gtk::IconPaintable> {
    let display = gtk::gdk::Display::default()?;
    let theme = gtk::IconTheme::for_display(&display);
    Some(theme.lookup_icon(
        name,
        &[],
        size,
        1,
        gtk::TextDirection::None,
        gtk::IconLookupFlags::empty(),
    ))
}

fn icon_for_kind(kind: &str) -> &'static str {
    match kind {
        "image" => "image-x-generic-symbolic",
        "video" => "video-x-generic-symbolic",
        "audio" => "audio-x-generic-symbolic",
        "pdf" | "doc" => "x-office-document-symbolic",
        _ => "text-x-generic-symbolic",
    }
}

fn format_bytes(b: i64) -> String {
    let kb = b as f64 / 1024.0;
    if kb < 1024.0 {
        format!("{kb:.0} KB")
    } else {
        format!("{:.1} MB", kb / 1024.0)
    }
}

fn fmt_date(secs: Option<f64>) -> Option<String> {
    let s = secs?;
    let dt = glib::DateTime::from_unix_local(s as i64).ok()?;
    dt.format("%Y-%m-%d").ok().map(|g| g.to_string())
}
