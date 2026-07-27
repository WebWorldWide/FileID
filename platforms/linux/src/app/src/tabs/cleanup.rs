// Cleanup tab — duplicate groups, the 1:1 GTK port of macOS `CleanupView.swift`
// (and Windows `CleanupViewModel.cs` / `PerceptualGrouping.cs`).
//
// Two modes, mirroring the reference:
//   * "Exact"   — same-size candidates are re-read and grouped by full-file
//                 SHA-256. The indexed sampled hash is only a ranking hint.
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
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use super::util::{fmt_date, format_bytes, icon_for_kind, icon_paintable};
use crate::engine_client::{texture_from_decoded, EngineClient, EngineEvent};
use fileid_engine::ipc::{CommandPayload, ExactTrashIdentity, TrashFilesPayload};

/// Default "visually similar" Hamming threshold (8 of 64 bits). `FILEID_NEARDUP_HAMMING`
/// overrides, clamped to 0..20. (mirrors macOS `defaultNearDupHamming`.)
const DEFAULT_NEARDUP_HAMMING: u32 = 8;
/// Above this image-with-dHash count the O(N²) pairwise scan is skipped rather
/// than hang the UI. (mirrors macOS `nearDupImageCap`.)
const NEAR_DUP_IMAGE_CAP: usize = 20_000;
/// Largest clusters first; cap the rendered groups like the reference.
const MAX_GROUPS: usize = 200;
const MAX_VISIBLE_MEMBERS: usize = 5_000;
const MAX_VISIBLE_MEMBERS_PER_GROUP: usize = 500;
const EXACT_READ_BUDGET_BYTES: i64 = 64 * 1024 * 1024 * 1024;
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
    total_members: usize,
    is_similar: bool,
    is_approximate: bool,
    total_bytes: i64,
    keeper_bytes: i64,
    exact_hash: Option<[u8; 32]>,
}

impl DupGroup {
    fn reclaimable(&self) -> i64 {
        self.total_bytes - self.keeper_bytes
    }
}

struct PendingTrash {
    ids: HashSet<i64>,
    preflight_rejected: usize,
}

#[derive(Clone)]
struct ExactTrashCandidate {
    id: i64,
    path: std::path::PathBuf,
    size: u64,
}

struct ExactTrashGroup {
    expected_hash: [u8; 32],
    keeper: ExactTrashCandidate,
    selected: Vec<ExactTrashCandidate>,
}

struct LoadResult {
    groups: Vec<DupGroup>,
    /// Number of candidate rows considered (files with a content hash, or images
    /// with a dHash) — distinguishes "nothing scanned yet" from "no duplicates".
    candidate_count: usize,
    warning: Option<String>,
}

impl LoadResult {
    fn empty() -> Self {
        Self {
            groups: Vec::new(),
            candidate_count: 0,
            warning: None,
        }
    }
}

type LoadOutcome = Result<LoadResult, String>;

// ─── The tab ──────────────────────────────────────────────────────────────────

struct Cleanup {
    engine: Rc<RefCell<EngineClient>>,
    mode: RefCell<String>, // "exact" | "similar"
    groups: RefCell<Vec<DupGroup>>,
    selection: RefCell<HashSet<i64>>, // file ids selected for deletion
    skipped: RefCell<HashSet<String>>, // group keys hidden from the view
    query_gen: Cell<u64>,
    load_cancel: RefCell<Option<Arc<AtomicBool>>>,
    load_inflight: Cell<bool>,
    reload_pending: Cell<bool>,
    deleting: Cell<bool>,
    trash_generation: Cell<u64>,
    pending_trash: RefCell<Option<PendingTrash>>,
    last_candidates: Cell<usize>,
    last_warning: RefCell<Option<String>>,
    last_refresh_error: RefCell<Option<String>>,
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
        load_cancel: RefCell::new(None),
        load_inflight: Cell::new(false),
        reload_pending: Cell::new(false),
        deleting: Cell::new(false),
        trash_generation: Cell::new(0),
        pending_trash: RefCell::new(None),
        last_candidates: Cell::new(0),
        last_warning: RefCell::new(None),
        last_refresh_error: RefCell::new(None),
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
                    EngineEvent::BulkActionResult(result) if is_trash_result(&result.action) => {
                        this.finish_trash(result);
                    }
                    EngineEvent::Exited if this.deleting.get() => {
                        this.fail_trash("The engine exited before confirming the Trash operation.");
                    }
                    _ => {}
                }
            }
        });
    }

    // Initial fill + a fresh read on every tab switch (the startup read can
    // race the engine's DB open; deletes/scans done elsewhere must show here).
    this.reload();
    {
        let this = this.clone();
        root.connect_map(move |_| this.reload());
    }
    this.update_global_summary();

    root.upcast()
}

impl Cleanup {
    fn switch_mode(
        self: &Rc<Self>,
        mode: &str,
        pill_exact: &gtk::Button,
        pill_similar: &gtk::Button,
    ) {
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
        self.last_warning.borrow_mut().take();
        self.last_refresh_error.borrow_mut().take();
        self.status_bar.set_visible(false);
        self.reload();
        self.update_global_summary();
    }

    // ── Reload (DB read + grouping off the main loop) ────────────────────────

    fn reload(self: &Rc<Self>) {
        let g = self.query_gen.get().wrapping_add(1);
        self.query_gen.set(g);
        if self.load_inflight.get() {
            self.reload_pending.set(true);
            if let Some(active) = self.load_cancel.borrow().as_ref() {
                active.store(true, AtomicOrdering::Release);
            }
            return;
        }
        self.load_inflight.set(true);
        self.reload_pending.set(false);
        let mode_similar = self.mode.borrow().as_str() == "similar";
        let cancel = Arc::new(AtomicBool::new(false));
        self.load_cancel.replace(Some(cancel.clone()));

        let (tx, rx) = async_channel::bounded::<LoadOutcome>(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(load_until(mode_similar, || {
                cancel.load(AtomicOrdering::Acquire)
            }));
        });

        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let received = rx.recv().await;
            this.load_inflight.set(false);
            if this.reload_pending.replace(false) {
                this.reload();
                return;
            }
            if this.query_gen.get() != g {
                return;
            }
            let res = match received {
                Ok(Ok(res)) => res,
                Ok(Err(error)) => {
                    *this.last_refresh_error.borrow_mut() = Some(error);
                    this.rebuild_list();
                    this.update_global_summary();
                    return;
                }
                Err(_) => {
                    *this.last_refresh_error.borrow_mut() = Some(
                        "Cleanup refresh worker stopped before returning a result.".to_string(),
                    );
                    this.rebuild_list();
                    this.update_global_summary();
                    return;
                }
            };
            this.last_refresh_error.borrow_mut().take();
            this.last_candidates.set(res.candidate_count);
            *this.last_warning.borrow_mut() = res.warning;
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
        let visible: Vec<&DupGroup> = groups
            .iter()
            .filter(|g| !skipped.contains(&g.key))
            .collect();

        if visible.is_empty() {
            self.show_empty(mode_similar, !groups.is_empty());
            return;
        }

        self.content_stack.set_visible_child_name("list");
        self.list_box.append(&build_banner(mode_similar));
        if let Some(error) = self.last_refresh_error.borrow().as_deref() {
            self.list_box.append(&build_warning_banner(&format!(
                "Cleanup refresh failed; showing the last verified results. {error}"
            )));
        }
        if let Some(warning) = self.last_warning.borrow().as_deref() {
            self.list_box.append(&build_warning_banner(warning));
        }
        for g in &visible {
            let card = self.build_group_card(g);
            self.list_box.append(&card);
        }
    }

    fn show_empty(self: &Rc<Self>, mode_similar: bool, has_skipped_only: bool) {
        let candidates = self.last_candidates.get();
        if let Some(error) = self.last_refresh_error.borrow().as_deref() {
            self.empty_page.set_child(None::<&gtk::Box>);
            self.empty_page.set_icon_name(Some("dialog-error-symbolic"));
            self.empty_page.set_title("Cleanup refresh failed");
            self.empty_page.set_description(Some(error));
        } else if let Some(warning) = self.last_warning.borrow().as_deref() {
            self.empty_page.set_child(None::<&gtk::Box>);
            self.empty_page
                .set_icon_name(Some("dialog-warning-symbolic"));
            self.empty_page.set_title(if mode_similar {
                "Similar comparison not run"
            } else {
                "No duplicates in the verified subset"
            });
            self.empty_page.set_description(Some(warning));
        } else if has_skipped_only {
            self.empty_page
                .set_icon_name(Some("object-select-symbolic"));
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
            self.empty_page
                .set_icon_name(Some("object-select-symbolic"));
            if mode_similar {
                self.empty_page
                    .set_title("No visually similar images found");
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
            group.total_members,
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
        if group.total_members > group.members.len() {
            head.append(
                &gtk::Label::builder()
                    .label(format!("showing {}", group.members.len()).as_str())
                    .css_classes(["dim-label"])
                    .valign(gtk::Align::Center)
                    .build(),
            );
        }
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

        {
            let this = self.clone();
            let ids = member_ids.clone();
            del_btn.connect_clicked(move |button| {
                let to_trash: Vec<i64> = {
                    let sel = this.selection.borrow();
                    ids.iter().copied().filter(|id| sel.contains(id)).collect()
                };
                this.confirm_trash(to_trash, button);
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

        wire_select(
            &b_except,
            self,
            &member_ids,
            &popover,
            SelectOp::AllExceptKeeper,
        );
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

        // Thumbnail (images decode client-side, videos get an ffmpeg keyframe;
        // everything else gets an icon).
        if member.kind == "image" || member.kind == "video" {
            let rx = if member.kind == "video" {
                self.engine
                    .borrow()
                    .request_video_thumbnail(member.path.clone(), TILE_THUMB_PX)
            } else {
                self.engine
                    .borrow()
                    .request_scaled_thumbnail(member.path.clone(), TILE_THUMB_PX)
            };
            let pic_weak = pic.downgrade();
            glib::MainContext::default().spawn_local(async move {
                let Ok(Some(decoded)) = rx.recv().await else {
                    return;
                };
                if let Some(p) = pic_weak.upgrade() {
                    p.set_paintable(Some(&texture_from_decoded(&decoded)));
                }
            });
        } else {
            pic.set_paintable(icon_paintable(icon_for_kind(&member.kind), 96).as_ref());
        }

        let toggle = gtk::ToggleButton::builder()
            .active(selected_now)
            .has_frame(false)
            .tooltip_text(format!("Select {}", member.name))
            .child(&outer)
            .build();
        let this = self.clone();
        let id = member.id;
        let outer_weak = outer.downgrade();
        let ind_weak = indicator.downgrade();
        toggle.connect_toggled(move |button| {
            let now_selected = button.is_active();
            {
                let mut selection = this.selection.borrow_mut();
                if now_selected {
                    selection.insert(id);
                } else {
                    selection.remove(&id);
                }
            }
            if let Some(outer) = outer_weak.upgrade() {
                if now_selected {
                    outer.add_css_class("file-tile-selected");
                } else {
                    outer.remove_css_class("file-tile-selected");
                }
            }
            if let Some(indicator) = ind_weak.upgrade() {
                indicator.set_icon_name(Some(checkbox_icon(now_selected)));
            }
            update_group_selection_widgets(
                &this.selection.borrow(),
                &group_sizes,
                &sel_lbl,
                &del_btn,
            );
            this.update_global_summary();
        });

        toggle.upcast()
    }

    // ── Header summary ───────────────────────────────────────────────────────

    fn update_global_summary(self: &Rc<Self>) {
        let mode_similar = self.mode.borrow().as_str() == "similar";
        let groups = self.groups.borrow();
        let skipped = self.skipped.borrow();
        let sel = self.selection.borrow();

        let visible: Vec<&DupGroup> = groups
            .iter()
            .filter(|g| !skipped.contains(&g.key))
            .collect();
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
        if !mode_similar && self.last_warning.borrow().is_some() {
            subtitle.push_str(" · partial verification");
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
        self.confirm_trash(ids, anchor);
    }

    fn confirm_trash(self: &Rc<Self>, ids: Vec<i64>, anchor: &gtk::Button) {
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
        dialog.present(Some(anchor));
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
        if ids.is_empty() || self.deleting.get() {
            return;
        }
        self.deleting.set(true);
        let operation = self.trash_generation.get().wrapping_add(1);
        self.trash_generation.set(operation);
        self.reveal_status();

        if self.mode.borrow().as_str() == "similar" {
            self.send_trash(operation, ids, 0, None);
            return;
        }

        self.status_label
            .set_text("Revalidating exact duplicates before Trash…");
        let requested: HashSet<i64> = ids.iter().copied().collect();
        let mut represented = HashSet::new();
        let mut checks = Vec::new();
        let mut rejected_without_keeper = 0usize;
        for group in self
            .groups
            .borrow()
            .iter()
            .filter(|group| !group.is_similar)
        {
            let Some(expected_hash) = group.exact_hash else {
                continue;
            };
            let selected: Vec<ExactTrashCandidate> = group
                .members
                .iter()
                .filter(|member| requested.contains(&member.id))
                .filter_map(|member| {
                    let size = u64::try_from(member.size).ok()?;
                    represented.insert(member.id);
                    Some(ExactTrashCandidate {
                        id: member.id,
                        path: std::path::PathBuf::from(&member.path),
                        size,
                    })
                })
                .collect();
            if selected.is_empty() {
                continue;
            }
            let keeper = group
                .members
                .iter()
                .find(|member| !requested.contains(&member.id))
                .and_then(|member| {
                    Some(ExactTrashCandidate {
                        id: member.id,
                        path: std::path::PathBuf::from(&member.path),
                        size: u64::try_from(member.size).ok()?,
                    })
                });
            let Some(keeper) = keeper else {
                rejected_without_keeper += selected.len();
                continue;
            };
            checks.push(ExactTrashGroup {
                expected_hash,
                keeper,
                selected,
            });
        }
        let unrepresented = requested
            .len()
            .saturating_sub(represented.len())
            .saturating_add(rejected_without_keeper);
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let (valid, rejected) = revalidate_exact_trash(checks);
            let _ = tx.send_blocking((valid, rejected + unrepresented));
        });
        let this = self.clone();
        glib::MainContext::default().spawn_local(async move {
            let Ok((valid, rejected)) = rx.recv().await else {
                if trash_operation_is_current(
                    this.trash_generation.get(),
                    operation,
                    this.deleting.get(),
                ) {
                    this.fail_trash("Exact duplicate revalidation did not complete.");
                }
                return;
            };
            if !trash_operation_is_current(
                this.trash_generation.get(),
                operation,
                this.deleting.get(),
            ) {
                return;
            }
            if valid.is_empty() {
                this.trash_generation.set(operation.wrapping_add(1));
                this.deleting.set(false);
                this.status_label.set_text(&format!(
                    "No files were trashed; {rejected} selected file{} changed or could not be byte-verified.",
                    plural(rejected)
                ));
                this.reveal_status();
                this.reload();
                return;
            }
            let ids = valid.iter().map(|identity| identity.file_id).collect();
            this.send_trash(operation, ids, rejected, Some(valid));
        });
    }

    fn send_trash(
        self: &Rc<Self>,
        operation: u64,
        ids: Vec<i64>,
        preflight_rejected: usize,
        exact_identities: Option<Vec<ExactTrashIdentity>>,
    ) {
        if !trash_operation_is_current(self.trash_generation.get(), operation, self.deleting.get())
        {
            return;
        }
        self.pending_trash.replace(Some(PendingTrash {
            ids: ids.iter().copied().collect(),
            preflight_rejected,
        }));
        self.status_label
            .set_text("Moving selected files to Trash…");
        let result = self
            .engine
            .borrow_mut()
            .send(CommandPayload::TrashFiles(TrashFilesPayload {
                file_ids: ids,
                exact_identities,
            }));
        if let Err(error) = result {
            self.fail_trash(&format!("Could not send the Trash command: {error}"));
        }
    }

    fn finish_trash(self: &Rc<Self>, result: fileid_engine::ipc::BulkActionResult) {
        let reported: HashSet<i64> = result
            .messages
            .iter()
            .filter_map(|message| message.file_id)
            .collect();
        let belongs_to_pending = {
            let pending = self.pending_trash.borrow();
            pending
                .as_ref()
                .is_some_and(|pending| reported.is_empty() || !reported.is_disjoint(&pending.ids))
        };
        if !belongs_to_pending {
            return;
        }
        let pending = self
            .pending_trash
            .borrow_mut()
            .take()
            .expect("pending trash");
        let operation_error = result.messages.iter().find_map(|message| {
            (!message.ok && message.file_id.is_none())
                .then(|| message.message.clone())
                .flatten()
        });
        if let Some(message) = operation_error {
            self.trash_generation
                .set(self.trash_generation.get().wrapping_add(1));
            self.deleting.set(false);
            self.status_label
                .set_text(&format!("Trash operation failed: {message}"));
            self.reveal_status();
            return;
        }
        let succeeded: Vec<i64> = result
            .messages
            .iter()
            .filter(|message| message.ok)
            .filter_map(|message| message.file_id)
            .filter(|id| pending.ids.contains(id))
            .collect();
        let freed = self.selected_bytes(&succeeded);
        {
            let mut selection = self.selection.borrow_mut();
            for id in &succeeded {
                selection.remove(id);
            }
        }
        self.trash_generation
            .set(self.trash_generation.get().wrapping_add(1));
        self.deleting.set(false);
        let total_failed = result.failed as usize + pending.preflight_rejected;
        if total_failed == 0 {
            self.status_label.set_text(&format!(
                "Trashed {} file{} · freed {:.1} MB · restore from Trash to undo",
                result.succeeded,
                plural(result.succeeded as usize),
                freed as f64 / BYTES_PER_MB,
            ));
        } else {
            let details = result
                .messages
                .iter()
                .filter(|message| !message.ok)
                .filter_map(|message| {
                    message
                        .file_id
                        .filter(|id| pending.ids.contains(id))
                        .map(|id| {
                            format!(
                                "#{id}: {}",
                                message
                                    .message
                                    .as_deref()
                                    .unwrap_or("Trash rejected the file")
                            )
                        })
                })
                .take(3)
                .collect::<Vec<_>>()
                .join(" · ");
            let preflight = if pending.preflight_rejected > 0 {
                format!(
                    "{} changed or failed byte verification before Trash. ",
                    pending.preflight_rejected
                )
            } else {
                String::new()
            };
            self.status_label.set_text(&format!(
                "Trashed {}; {total_failed} were rejected, failed, or need catalog recovery. {preflight}{details}",
                result.succeeded,
            ));
        }
        self.reveal_status();
        self.reload();
    }

    fn fail_trash(&self, message: &str) {
        self.pending_trash.borrow_mut().take();
        self.trash_generation
            .set(self.trash_generation.get().wrapping_add(1));
        self.deleting.set(false);
        self.status_label.set_text(message);
        self.reveal_status();
    }

    fn reveal_status(&self) {
        self.status_bar.set_visible(true);
        let bar = self.status_bar.clone();
        let _ = crate::spring::animate(&self.status_bar, 0.0, 1.0, move |value| {
            bar.set_opacity(value);
        });
    }
}

fn revalidate_exact_trash(groups: Vec<ExactTrashGroup>) -> (Vec<ExactTrashIdentity>, usize) {
    let mut valid = Vec::new();
    let mut rejected = 0usize;
    for group in groups {
        let keeper_matches = fileid_engine::util::content_hash::exact_file_sha256(
            &group.keeper.path,
            group.keeper.size,
        )
        .is_ok_and(|hash| hash == group.expected_hash);
        if !keeper_matches {
            rejected += group.selected.len();
            continue;
        }
        for selected in group.selected {
            if selected.id == group.keeper.id {
                rejected += 1;
                continue;
            }
            let matches =
                fileid_engine::util::content_hash::exact_file_sha256(&selected.path, selected.size)
                    .is_ok_and(|hash| hash == group.expected_hash);
            if matches {
                valid.push(ExactTrashIdentity {
                    file_id: selected.id,
                    path: selected.path.to_string_lossy().into_owned(),
                    size_bytes: selected.size as i64,
                    sha256_hex: hex(&group.expected_hash),
                    keeper_path: group.keeper.path.to_string_lossy().into_owned(),
                    keeper_size_bytes: group.keeper.size as i64,
                    keeper_sha256_hex: hex(&group.expected_hash),
                });
            } else {
                rejected += 1;
            }
        }
    }
    (valid, rejected)
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
    sel_lbl.set_text(&format!(
        "{cnt} selected · {:.1} MB",
        bytes as f64 / BYTES_PER_MB
    ));
    sel_lbl.set_visible(cnt > 0);
    del_btn.set_label(&format!("Delete {cnt} from this group"));
    del_btn.set_sensitive(cnt > 0);
}

fn build_warning_banner(text: &str) -> gtk::Widget {
    let row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["glass-card"])
        .build();
    row.append(
        &gtk::Image::builder()
            .icon_name("dialog-warning-symbolic")
            .valign(gtk::Align::Start)
            .css_classes(["gold-accent"])
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

fn load_until(mode_similar: bool, should_cancel: impl Fn() -> bool) -> LoadOutcome {
    let db_path = fileid_engine::paths::db_path()
        .map_err(|error| format!("Could not locate the FileID database: {error}"))?;
    match db_path.try_exists() {
        Ok(false) => return Ok(LoadResult::empty()),
        Ok(true) => {}
        Err(error) => {
            return Err(format!(
                "Could not access the FileID database path: {error}"
            ));
        }
    }
    let conn = fileid_engine::db::open_read(&db_path)
        .map_err(|error| format!("Could not open the FileID database: {error}"))?;
    if mode_similar {
        load_similar_until(&conn, should_cancel)
    } else {
        load_exact_until(&conn, should_cancel)
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
}

#[cfg(test)]
fn load_exact(conn: &rusqlite::Connection) -> LoadResult {
    load_exact_until(conn, || false).expect("exact cleanup query")
}

fn load_exact_until(conn: &rusqlite::Connection, should_cancel: impl Fn() -> bool) -> LoadOutcome {
    let candidate_count = conn
        .query_row("SELECT COUNT(*) FROM files WHERE failed=0", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| format!("Could not count indexed files: {error}"))?
        .max(0) as usize;
    let eligible_count = conn
        .query_row(
            "WITH sizes AS ( \
                 SELECT size_bytes FROM files WHERE failed=0 \
                 GROUP BY size_bytes HAVING COUNT(*)>1 \
             ) \
             SELECT COUNT(*) FROM files f JOIN sizes s ON s.size_bytes=f.size_bytes \
             WHERE f.failed=0",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(|error| format!("Could not count same-size cleanup candidates: {error}"))?
        .max(0) as usize;
    if eligible_count == 0 {
        return Ok(LoadResult {
            groups: Vec::new(),
            candidate_count,
            warning: None,
        });
    }

    let sql = "WITH top_sizes AS ( \
                   SELECT size_bytes,COUNT(*) AS n, \
                          (COUNT(*)-1)*MAX(size_bytes,0) AS payoff \
                   FROM files WHERE failed=0 \
                   GROUP BY size_bytes HAVING n>1 \
                   ORDER BY payoff DESC,size_bytes DESC LIMIT ?1 \
               ), ranked AS ( \
                   SELECT f.id,f.path_text,f.size_bytes,f.modified_at, \
                          f.created_at,f.aesthetic,f.kind,ts.payoff, \
                          ROW_NUMBER() OVER (PARTITION BY f.size_bytes \
                            ORDER BY COALESCE(f.aesthetic,0) DESC, \
                                     COALESCE(f.created_at,1e18),LENGTH(f.path_text),f.path_text) AS member_rank \
                   FROM files f JOIN top_sizes ts ON ts.size_bytes=f.size_bytes \
                   WHERE f.failed=0 \
               ) \
               SELECT id,path_text,size_bytes,modified_at,created_at,aesthetic,kind \
               FROM ranked WHERE member_rank<=?2 \
               ORDER BY payoff DESC,size_bytes DESC,member_rank LIMIT ?3";
    let mut stmt = conn
        .prepare(sql)
        .map_err(|error| format!("Could not prepare the exact-cleanup query: {error}"))?;
    let rows = stmt
        .query_map(
            rusqlite::params![
                MAX_GROUPS,
                MAX_VISIBLE_MEMBERS_PER_GROUP,
                MAX_VISIBLE_MEMBERS
            ],
            |row| {
                Ok(RawRow {
                    id: row.get(0)?,
                    path: row.get(1)?,
                    size: row.get(2)?,
                    modified: row.get(3)?,
                    created: row.get(4)?,
                    aesthetic: row.get(5)?,
                    kind: row.get(6)?,
                    phash: 0,
                })
            },
        )
        .map_err(|error| format!("Could not run the exact-cleanup query: {error}"))?;
    let raw: Vec<RawRow> = rows
        .collect::<Result<_, _>>()
        .map_err(|error| format!("Could not read an exact-cleanup row: {error}"))?;
    let mut selected_bytes = 0i64;
    let mut candidates = Vec::new();
    for row in &raw {
        let bytes = row.size.max(0);
        if bytes > EXACT_READ_BUDGET_BYTES - selected_bytes {
            continue;
        }
        selected_bytes += bytes;
        candidates.push(fileid_engine::util::content_hash::ExactDuplicateCandidate {
            id: row.id,
            path: std::path::PathBuf::from(&row.path),
            indexed_size: row.size,
        });
    }
    let selected_count = candidates.len();
    let grouping =
        fileid_engine::util::content_hash::group_exact_duplicates_until(candidates, should_cancel);
    let skipped = grouping.skipped;
    let index_by_id: HashMap<i64, usize> = raw
        .iter()
        .enumerate()
        .map(|(index, row)| (row.id, index))
        .collect();
    let mut groups = Vec::new();
    for exact in grouping.groups {
        let mut indices: Vec<usize> = exact
            .files
            .iter()
            .filter_map(|file| index_by_id.get(&file.id).copied())
            .collect();
        if indices.len() < 2 {
            continue;
        }
        rank_indices(&raw, &mut indices);
        groups.push(build_group(
            &raw,
            &indices,
            format!("dup-{}:{}", hex(&exact.hash), exact.size),
            false,
            false,
            Some(exact.hash),
        ));
    }
    groups.sort_by_key(|group| std::cmp::Reverse(group.total_members));
    let omitted_groups = groups.len().saturating_sub(MAX_GROUPS);
    if omitted_groups > 0 {
        groups.truncate(MAX_GROUPS);
    }
    let omitted = eligible_count.saturating_sub(selected_count);
    let verified = selected_count.saturating_sub(skipped);
    let warning = (omitted > 0 || skipped > 0 || omitted_groups > 0).then(|| {
        format!(
            "Exact results are partial: byte-verified {verified} of {eligible_count} same-size candidates; \
             {omitted} were outside the 200-size-class, 500-per-size, {MAX_VISIBLE_MEMBERS}-file, or 64 GiB refresh limits; \
             {skipped} were missing, unreadable, changed, or cancelled; {omitted_groups} verified groups were outside the {MAX_GROUPS}-group display limit. \
             No unverified file is shown as an exact duplicate."
        )
    });
    Ok(LoadResult {
        groups,
        candidate_count,
        warning,
    })
}

#[cfg(test)]
fn load_similar(conn: &rusqlite::Connection) -> LoadResult {
    load_similar_until(conn, || false).expect("similar cleanup query")
}

fn load_similar_until(
    conn: &rusqlite::Connection,
    should_cancel: impl Fn() -> bool,
) -> LoadOutcome {
    let candidate_count = conn
        .query_row(
            "SELECT COUNT(*) FROM files WHERE kind='image' AND failed=0 AND phash IS NOT NULL AND phash!=0",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map_err(|error| format!("Could not count similar-image candidates: {error}"))?
        .max(0) as usize;
    if should_cancel() {
        return Ok(LoadResult::empty());
    }
    if candidate_count > NEAR_DUP_IMAGE_CAP {
        return Ok(LoadResult {
            groups: Vec::new(),
            candidate_count,
            warning: Some(format!(
                "Visually similar comparison is unavailable for {candidate_count} images: \
                 the exact Hamming matcher is capped at {NEAR_DUP_IMAGE_CAP}. \
                 Exact duplicate cleanup remains available."
            )),
        });
    }
    // Only images carry a dHash; phash == 0 is the engine's "none / failed"
    // sentinel — exclude it so blank hashes don't collapse into one giant group.
    let sql = "SELECT id, path_text, size_bytes, modified_at, created_at, aesthetic, phash, kind \
               FROM files \
               WHERE kind = 'image' AND failed = 0 AND phash IS NOT NULL AND phash != 0";
    let mut stmt = conn
        .prepare(sql)
        .map_err(|error| format!("Could not prepare the similar-cleanup query: {error}"))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RawRow {
                id: row.get(0)?,
                path: row.get(1)?,
                size: row.get(2)?,
                modified: row.get(3)?,
                created: row.get(4)?,
                aesthetic: row.get(5)?,
                phash: row.get(6)?,
                kind: row.get(7)?,
            })
        })
        .map_err(|error| format!("Could not run the similar-cleanup query: {error}"))?;

    let raw: Vec<RawRow> = rows
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("Could not read a similar-cleanup row: {error}"))?
        .into_iter()
        .filter(|row| row.phash != 0)
        .collect();
    if raw.len() <= 1 || raw.len() > NEAR_DUP_IMAGE_CAP {
        // Empty or beyond the O(N²) cap — skip perceptual grouping (Exact stays).
        return Ok(LoadResult {
            groups: Vec::new(),
            candidate_count,
            warning: None,
        });
    }

    let max_hamming = near_dup_threshold();
    let mut index_by_id: HashMap<i64, usize> = HashMap::with_capacity(raw.len());
    let mut items: Vec<(i64, i64)> = Vec::with_capacity(raw.len());
    for (i, r) in raw.iter().enumerate() {
        index_by_id.insert(r.id, i);
        items.push((r.id, r.phash));
    }

    let matched = group_by_hamming_until(&items, max_hamming, &should_cancel);
    let mut groups: Vec<DupGroup> = Vec::new();
    let mut remaining = MAX_VISIBLE_MEMBERS;
    let mut omitted_for_members = 0usize;
    for ids in matched {
        if remaining < 2 {
            omitted_for_members += 1;
            continue;
        }
        let mut indices: Vec<usize> = ids
            .iter()
            .filter_map(|id| index_by_id.get(id).copied())
            .collect();
        if indices.len() < 2 {
            continue;
        }
        rank_indices(&raw, &mut indices);
        // Stable identity: smallest member id, independent of keeper re-ranks.
        let gid = indices.iter().map(|&i| raw[i].id).min().unwrap_or(0);
        let total_members = indices.len();
        let total_bytes = indices.iter().map(|&i| raw[i].size).sum();
        let visible = total_members
            .min(MAX_VISIBLE_MEMBERS_PER_GROUP)
            .min(remaining);
        let mut group = build_group(
            &raw,
            &indices[..visible],
            format!("sim-{gid}"),
            true,
            false,
            None,
        );
        group.total_members = total_members;
        group.total_bytes = total_bytes;
        remaining -= group.members.len();
        groups.push(group);
    }

    groups.sort_by_key(|group| std::cmp::Reverse(group.total_members));
    let omitted_for_groups = groups.len().saturating_sub(MAX_GROUPS);
    if omitted_for_groups > 0 {
        groups.truncate(MAX_GROUPS);
    }
    let omitted = omitted_for_members + omitted_for_groups;
    Ok(LoadResult {
        groups,
        candidate_count,
        warning: (omitted > 0).then(|| {
            format!(
                "Similar results are partial: {omitted} matched groups were outside the \
                 {MAX_VISIBLE_MEMBERS}-visible-member or {MAX_GROUPS}-group display limits."
            )
        }),
    })
}

fn build_group(
    raw: &[RawRow],
    indices: &[usize],
    key: String,
    is_similar: bool,
    is_approximate: bool,
    exact_hash: Option<[u8; 32]>,
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
        total_members: indices.len(),
        is_similar,
        is_approximate,
        total_bytes,
        keeper_bytes,
        exact_hash,
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

/// Union-find clustering of dHashes within `max_hamming` (transitively). Returns
/// groups of size ≥ 2 in first-seen order. (Direct port of `PerceptualGrouping`.)
fn group_by_hamming_until(
    items: &[(i64, i64)],
    max_hamming: u32,
    should_cancel: impl Fn() -> bool,
) -> Vec<Vec<i64>> {
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
        if should_cancel() {
            return Vec::new();
        }
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
    for (i, item) in items.iter().enumerate() {
        let r = find(&mut parent, i);
        members_by_root.entry(r).or_insert_with(|| {
            order.push(r);
            Vec::new()
        });
        members_by_root.get_mut(&r).unwrap().push(item.0);
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

fn trash_operation_is_current(current: u64, operation: u64, deleting: bool) -> bool {
    deleting && current == operation
}

fn is_trash_result(action: &str) -> bool {
    action == "trashFiles" || action.starts_with("trashFiles:")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        conn
    }

    #[test]
    fn stale_trash_preflight_cannot_join_a_new_operation() {
        assert!(trash_operation_is_current(7, 7, true));
        assert!(!trash_operation_is_current(8, 7, true));
        assert!(!trash_operation_is_current(7, 7, false));
    }

    #[test]
    fn trash_results_accept_engine_batch_suffixes() {
        assert!(is_trash_result("trashFiles"));
        assert!(is_trash_result("trashFiles:batch-id"));
        assert!(!is_trash_result("applyTags"));
    }

    #[test]
    fn cleanup_loaders_surface_schema_and_row_failures() {
        let missing_schema = rusqlite::Connection::open_in_memory().unwrap();
        assert!(load_exact_until(&missing_schema, || false)
            .err()
            .unwrap()
            .contains("Could not count indexed files"));
        assert!(load_similar_until(&missing_schema, || false)
            .err()
            .unwrap()
            .contains("Could not count similar-image candidates"));

        let malformed_row = rusqlite::Connection::open_in_memory().unwrap();
        malformed_row
            .execute_batch(
                "CREATE TABLE files(\
                    id INTEGER, path_text, size_bytes INTEGER, modified_at REAL,\
                    created_at REAL, aesthetic REAL, kind TEXT, failed INTEGER\
                 );\
                 INSERT INTO files VALUES(1,NULL,4,NULL,NULL,NULL,'other',0);\
                 INSERT INTO files VALUES(2,'/tmp/two',4,NULL,NULL,NULL,'other',0);",
            )
            .unwrap();
        assert!(load_exact_until(&malformed_row, || false)
            .err()
            .unwrap()
            .contains("Could not read an exact-cleanup row"));
    }

    #[test]
    fn exact_cleanup_reports_bounded_verified_totals_without_claiming_omitted_files() {
        let mut conn = database();
        let dir = std::env::temp_dir().join(format!(
            "fileid-linux-cleanup-{}-bounded",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let tx = conn.transaction().unwrap();
        for id in 1..=600i64 {
            let path = dir.join(format!("{id}.jpg"));
            std::fs::write(&path, b"same").unwrap();
            tx.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed,content_hash) \
                 VALUES (?1,?2,?1,4,1,'image','jpg',0,x'0102')",
                rusqlite::params![id, path.to_string_lossy().as_ref()],
            )
            .unwrap();
        }
        tx.commit().unwrap();

        let loaded = load_exact(&conn);
        assert_eq!(loaded.candidate_count, 600);
        assert_eq!(loaded.groups.len(), 1);
        assert_eq!(
            loaded.groups[0].total_members,
            MAX_VISIBLE_MEMBERS_PER_GROUP
        );
        assert_eq!(
            loaded.groups[0].members.len(),
            MAX_VISIBLE_MEMBERS_PER_GROUP
        );
        assert_eq!(loaded.groups[0].total_bytes, 2_000);
        let warning = loaded.warning.as_deref().unwrap_or_default();
        assert!(warning.contains("partial"));
        assert!(warning.contains("100 were outside"));
        assert!(warning.contains("500-per-size"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_cleanup_discloses_post_hash_group_display_truncation() {
        let mut conn = database();
        let dir = std::env::temp_dir().join(format!(
            "fileid-linux-cleanup-{}-groups",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let tx = conn.transaction().unwrap();
        for pair in 0..=MAX_GROUPS {
            let bytes = (pair as u64).to_le_bytes();
            for copy in 0..2usize {
                let id = (pair * 2 + copy) as i64 + 1;
                let path = dir.join(format!("{pair}-{copy}.bin"));
                std::fs::write(&path, bytes).unwrap();
                tx.execute(
                    "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed,content_hash) \
                     VALUES (?1,?2,?1,8,1,'other','bin',0,x'0102')",
                    rusqlite::params![id, path.to_string_lossy().as_ref()],
                )
                .unwrap();
            }
        }
        tx.commit().unwrap();

        let loaded = load_exact(&conn);
        assert_eq!(loaded.groups.len(), MAX_GROUPS);
        assert!(loaded
            .warning
            .as_deref()
            .unwrap_or_default()
            .contains("1 verified groups were outside"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_cleanup_live_verifies_rows_without_persisted_hashes() {
        let conn = database();
        let dir = std::env::temp_dir().join(format!(
            "fileid-linux-cleanup-{}-null-hash",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        for id in 1..=2i64 {
            let path = dir.join(format!("{id}.bin"));
            std::fs::write(&path, b"same").unwrap();
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed,content_hash) \
                 VALUES (?1,?2,?1,4,1,'other','bin',0,NULL)",
                rusqlite::params![id, path.to_string_lossy().as_ref()],
            )
            .unwrap();
        }
        let loaded = load_exact(&conn);
        assert_eq!(loaded.groups.len(), 1);
        assert_eq!(loaded.groups[0].total_members, 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_cleanup_rejects_persisted_hash_collisions_with_live_full_sha256() {
        let conn = database();
        let dir = std::env::temp_dir().join(format!(
            "fileid-linux-cleanup-{}-collision",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        for (id, bytes) in [(1i64, b"aaaa"), (2, b"bbbb"), (3, b"aaaa")] {
            let path = dir.join(format!("{id}.bin"));
            std::fs::write(&path, bytes).unwrap();
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed,content_hash) \
                 VALUES (?1,?2,?1,4,1,'other','bin',0,x'0102')",
                rusqlite::params![id, path.to_string_lossy().as_ref()],
            )
            .unwrap();
        }

        let loaded = load_exact(&conn);
        assert_eq!(loaded.groups.len(), 1);
        let ids: HashSet<i64> = loaded.groups[0]
            .members
            .iter()
            .map(|member| member.id)
            .collect();
        assert_eq!(ids, HashSet::from([1, 3]));
        assert!(!ids.contains(&2));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn exact_trash_revalidation_rejects_only_changed_victims() {
        let dir = std::env::temp_dir().join(format!(
            "fileid-linux-cleanup-{}-preflight",
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let keeper = dir.join("keeper.bin");
        let changed = dir.join("changed.bin");
        let unchanged = dir.join("unchanged.bin");
        std::fs::write(&keeper, b"same").unwrap();
        std::fs::write(&changed, b"same").unwrap();
        std::fs::write(&unchanged, b"same").unwrap();
        let expected_hash =
            fileid_engine::util::content_hash::exact_file_sha256(&keeper, 4).unwrap();
        std::fs::write(&changed, b"diff").unwrap();

        let (valid, rejected) = revalidate_exact_trash(vec![ExactTrashGroup {
            expected_hash,
            keeper: ExactTrashCandidate {
                id: 1,
                path: keeper.clone(),
                size: 4,
            },
            selected: vec![
                ExactTrashCandidate {
                    id: 2,
                    path: changed,
                    size: 4,
                },
                ExactTrashCandidate {
                    id: 3,
                    path: unchanged,
                    size: 4,
                },
            ],
        }]);
        assert_eq!(
            valid
                .iter()
                .map(|identity| identity.file_id)
                .collect::<Vec<_>>(),
            vec![3]
        );
        assert_eq!(valid[0].keeper_path, keeper.to_string_lossy());
        assert_eq!(rejected, 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn similar_cleanup_does_not_hide_equal_sampled_hashes() {
        let conn = database();
        for id in 1..=2i64 {
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed,content_hash,phash) \
                 VALUES (?1,printf('/library/%d.jpg',?1),?1,4,1,'image','jpg',0,x'0102',7)",
                [id],
            )
            .unwrap();
        }
        let loaded = load_similar(&conn);
        assert_eq!(loaded.groups.len(), 1);
        assert_eq!(loaded.groups[0].total_members, 2);
    }

    #[test]
    fn similar_hamming_grouping_honors_cancellation() {
        let calls = std::cell::Cell::new(0usize);
        let items: Vec<(i64, i64)> = (0..1_000).map(|id| (id, id)).collect();
        let groups = group_by_hamming_until(&items, 8, || {
            calls.set(calls.get() + 1);
            true
        });
        assert!(groups.is_empty());
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn similar_cleanup_rejects_oversized_input_before_materializing_rows() {
        let conn = database();
        conn.execute(
            "WITH RECURSIVE ids(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM ids WHERE x<?1) \
             INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed,phash) \
             SELECT x,printf('/library/%d.jpg',x),x,4,1,'image','jpg',0,x FROM ids",
            [NEAR_DUP_IMAGE_CAP as i64 + 1],
        )
        .unwrap();

        let loaded = load_similar(&conn);
        assert!(loaded.groups.is_empty());
        assert_eq!(loaded.candidate_count, NEAR_DUP_IMAGE_CAP + 1);
        assert!(loaded
            .warning
            .as_deref()
            .unwrap_or_default()
            .contains("unavailable"));
    }
}
