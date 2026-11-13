// Library tab — read-only DB-backed thumbnail grid, filename/tag/text
// search, and file preview.
//
//   * `gtk::SearchEntry` (debounced) → engine query → grid reload,
//   * gold segmented kind pills (All / Images / Videos / Docs / PDFs / Audio),
//   * a virtualized `gtk::GridView` backed by a `gio::ListStore` of
//     `BoxedAnyObject(FileRow)`, with a `SignalListItemFactory` that lazily
//     loads each visible tile's thumbnail off the main loop, and
//   * an `adw::Dialog` preview (large image + metadata) on activation.
//
// Live-scan behaviour mirrors macOS: the tab subscribes to engine events and
// throttle-reloads the grid as batches land so the room "fills in" during a
// scan, then does a final reload on completion.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;
use gtk::glib::BoxedAnyObject;

use super::library_bulk::BulkBar;
use super::library_pane::SelectedPane;
use super::library_selection::{self as picking, PaneView};
use super::offline_roots::{self, OfflineRoots};
use super::util::{decode_px, fmt_date, icon_for_kind, icon_paintable};
use fileid_engine::util::display::file_size_i64;
use crate::engine_client::{
    texture_from_decoded, CancelToken, EngineClient, EngineEvent, FileRow, QueryChannel, QuerySpec, ThumbReply,
};
use std::sync::atomic::Ordering;

const QUERY_LIMIT: i64 = 600;
const LIVE_RELOAD_MAX: i64 = 3000;
/// While a scan runs, the grid follows it this often, and only at the top.
const LIVE_RELOAD_EVERY: Duration = Duration::from_millis(1500);
/// A total counted this recently is reused by the next live reload.
const COUNT_FRESH_FOR: Duration = Duration::from_secs(5);
/// Scrolled further than this, new files are offered instead of inserted.
const AT_TOP_PX: f64 = 48.0;
/// The preview fetches its next page this many files before the end.
const PREVIEW_PAGE_AHEAD: usize = 8;

/// What a library change does to the grid while the page is on screen.
#[derive(Debug, PartialEq, Eq)]
enum LiveStep {
    Reload,
    OfferNewFiles,
    Wait,
}

/// New files are inserted only when the user is at the top of the grid;
/// further down they are offered, so the photos being looked at never move.
fn live_step(at_top: bool, throttled: bool, finished: bool) -> LiveStep {
    match (at_top, throttled && !finished) {
        (true, false) => LiveStep::Reload,
        (true, true) => LiveStep::Wait,
        (false, _) => LiveStep::OfferNewFiles,
    }
}

/// Events after which the grid may show something out of date.
fn changes_library(event: &EngineEvent) -> bool {
    matches!(
        event,
        EngineEvent::Ready
            | EngineEvent::BatchLanded(_)
            | EngineEvent::ScanComplete(_)
            | EngineEvent::BulkActionResult(_)
            | EngineEvent::RestructureApplyResult(_)
            | EngineEvent::DeepAnalyzeFileDone(_)
            | EngineEvent::DeepAnalyzeComplete(_)
            | EngineEvent::FaceClusteringComplete(_)
            | EngineEvent::ToolResponse(_)
    )
}

/// The preview's snapshot can grow when the grid handed over full pages.
fn preview_can_page(loaded: usize) -> bool {
    loaded > 0 && loaded.is_multiple_of(QUERY_LIMIT as usize)
}

/// Appends the rows of `page` not already in `rows`; false when none were new.
fn append_new_rows(rows: &mut Vec<FileRow>, page: Vec<FileRow>) -> bool {
    let known: HashSet<i64> = rows.iter().map(|row| row.id).collect();
    let before = rows.len();
    rows.extend(page.into_iter().filter(|row| !known.contains(&row.id)));
    rows.len() > before
}
const TILE_THUMB_PX: i32 = 256;
const PREVIEW_PX: i32 = 2400;

pub fn build(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    // ── Shared state ─────────────────────────────────────────────────────────
    let search_text = Rc::new(RefCell::new(String::new()));
    let kind_filter = Rc::new(RefCell::new(None::<String>));
    let query_gen = Rc::new(Cell::new(0u64));
    let debounce_gen = Rc::new(Cell::new(0u64));
    let similar = Rc::new(RefCell::new(None::<(i64, String)>));

    let model = gio::ListStore::new::<BoxedAnyObject>();
    let selection = gtk::MultiSelection::new(Some(model.clone()));
    let offline = Rc::new(RefCell::new(OfflineRoots::default()));

    // ── Grid factory ─────────────────────────────────────────────────────────
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, list_item| {
        if let Some(li) = list_item.downcast_ref::<gtk::ListItem>() {
            li.set_child(Some(&build_tile()));
        }
    });
    factory.connect_bind(clone!(
        #[strong]
        engine,
        #[strong]
        offline,
        move |_, list_item| {
            if let Some(li) = list_item.downcast_ref::<gtk::ListItem>() {
                bind_tile(&engine, li, &offline.borrow());
            }
        }
    ));
    factory.connect_unbind(|_, list_item| {
        if let Some(li) = list_item.downcast_ref::<gtk::ListItem>() {
            BOUND_TILES.with_borrow_mut(|tiles| tiles.remove(&tile_token_key(li)));
            cancel_tile_token(li);
            if let Some(tile) = li.child() {
                clear_tile(&tile);
            }
        }
    });

    let grid = gtk::GridView::new(Some(selection.clone()), Some(factory));
    grid.set_min_columns(1);
    grid.set_max_columns(12);
    grid.set_enable_rubberband(false);
    grid.add_css_class("fileid-tab");

    grid.connect_activate(clone!(
        #[strong]
        engine,
        #[strong]
        search_text,
        #[strong]
        kind_filter,
        move |gv, pos| {
            if let Some(model) = gv.model() {
                let query = (search_text.borrow().clone(), kind_filter.borrow().clone());
                open_preview(&engine, gv, snapshot_rows(&model), pos as usize, query);
            }
        }
    ));

    // Click selects one file and fills the Selected pane, Ctrl+click toggles,
    // Shift+click and Shift+arrows extend; double-click/Enter opens the
    // preview; Esc clears the selection.
    let restoring = Rc::new(Cell::new(false));
    let similar_hook: Rc<RefCell<Box<dyn Fn()>>> = Rc::new(RefCell::new(Box::new(|| {})));
    let pane = {
        let on_preview: Rc<dyn Fn()> = Rc::new(clone!(
            #[strong]
            engine,
            #[strong]
            search_text,
            #[strong]
            kind_filter,
            #[strong]
            selection,
            #[weak]
            grid,
            move || {
                let [(pos, _)] = selected_rows(&selection)[..] else {
                    return;
                };
                let query = (search_text.borrow().clone(), kind_filter.borrow().clone());
                open_preview(&engine, &grid, snapshot_rows(&selection), pos as usize, query);
            }
        ));
        let on_close: Rc<dyn Fn()> = Rc::new(clone!(
            #[strong]
            selection,
            move || {
                selection.unselect_all();
            }
        ));
        let on_similar: Rc<dyn Fn()> = Rc::new(clone!(
            #[strong]
            selection,
            #[strong]
            similar,
            #[strong]
            similar_hook,
            move || {
                let [(_, row)] = &selected_rows(&selection)[..] else {
                    return;
                };
                similar.replace(Some((row.id, row.name.clone())));
                similar_hook.borrow()();
            }
        ));
        SelectedPane::new(engine.clone(), offline.clone(), on_preview, on_close, on_similar)
    };
    let bulk_hook: Rc<RefCell<Box<dyn Fn(usize)>>> = Rc::new(RefCell::new(Box::new(|_| {})));
    selection.connect_selection_changed(clone!(
        #[strong]
        pane,
        #[strong]
        restoring,
        #[strong]
        bulk_hook,
        move |selection, _, _| {
            bulk_hook.borrow()(selection.selection().size() as usize);
            if !restoring.get() {
                show_selection(selection, &pane, false);
            }
        }
    ));
    let escape = gtk::EventControllerKey::new();
    escape.connect_key_pressed(clone!(
        #[strong]
        selection,
        move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape && selection.selection().size() > 0 {
                selection.unselect_all();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        }
    ));
    grid.add_controller(escape);

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&grid)
        .build();
    scroller.add_css_class("fileid-tab");

    // Empty state (no scan yet / no matches) ↔ the grid. Without this a fresh
    // install shows a silent black void where the Library should invite the
    // first scan (macOS/Windows both present an empty-state hint here).
    let empty_page = adw::StatusPage::builder()
        .icon_name("view-grid-symbolic")
        .title("No files yet")
        .vexpand(true)
        .build();
    let content_stack = gtk::Stack::new();
    content_stack.set_hexpand(true);
    content_stack.set_vexpand(true);
    content_stack.add_named(&scroller, Some("grid"));
    content_stack.add_named(&empty_page, Some("empty"));

    // ── Header (count + search + pills); the shell draws the page title ─────
    let count_label = gtk::Label::builder()
        .label("0 files")
        .xalign(0.0)
        .css_classes(["dim-label", "numeric"])
        .build();
    let title_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    title_row.append(&count_label);
    let new_files = gtk::Button::builder()
        .label("New files · show")
        .tooltip_text("More files have been read. Go to the top to see them.")
        .visible(false)
        .css_classes(["pill", "gold-accent"])
        .build();
    title_row.append(&new_files);
    let select_all = crate::v3::ghost("Select all");
    select_all.set_tooltip_text(Some("Select every file shown (Ctrl+click adds one, Shift+click a range)"));
    select_all.connect_clicked(clone!(
        #[strong]
        selection,
        move |_| {
            selection.select_all();
        }
    ));
    let clear_selection = crate::v3::ghost("Clear");
    clear_selection.set_tooltip_text(Some("Unselect every file"));
    clear_selection.update_property(&[gtk::accessible::Property::Label("Clear the selection")]);
    clear_selection.connect_clicked(clone!(
        #[strong]
        selection,
        move |_| {
            selection.unselect_all();
        }
    ));
    title_row.append(&select_all);
    title_row.append(&clear_selection);
    for (label, page) in [("Search & Moments", "moments"), ("File Tools", "tools")] {
        let button = gtk::Button::builder()
            .label(label)
            .action_name("win.navigate")
            .action_target(&page.to_variant())
            .build();
        title_row.append(&button);
    }

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search by name, tag or text in files")
        .hexpand(true)
        .css_classes(["fileid-search"])
        .build();

    // Reusable reload closure shared by search + pills + scan events.
    let gate = Rc::new(super::reload_gate::ReloadGate::default());
    let live_ctx: Rc<RefCell<Option<Rc<ReloadCtx>>>> = Rc::new(RefCell::new(None));
    let reload: Rc<dyn Fn()> = {
        let ctx = Rc::new(ReloadCtx {
            gate: gate.clone(),
            new_files: new_files.clone(),
            counted_at: Cell::new(None),
            engine: engine.clone(),
            model: model.clone(),
            count_label: count_label.clone(),
            content_stack: content_stack.clone(),
            empty_page: empty_page.clone(),
            search_text: search_text.clone(),
            kind_filter: kind_filter.clone(),
            query_gen: query_gen.clone(),
            has_more: Rc::new(Cell::new(false)),
            loading_more: Rc::new(Cell::new(false)),
            total: Rc::new(Cell::new(0)),
            last_query: RefCell::new(None),
            selection: selection.clone(),
            pane: pane.clone(),
            restoring: restoring.clone(),
            similar: similar.clone(),
        });
        let near_end = ctx.clone();
        scroller.vadjustment().connect_value_changed(move |adj| {
            if adj.value() + adj.page_size() * 3.0 >= adj.upper() {
                load_more(&near_end);
            }
        });
        live_ctx.replace(Some(ctx.clone()));
        Rc::new(move || run_reload(&ctx, false))
    };
    new_files.connect_clicked(clone!(
        #[weak]
        scroller,
        #[strong]
        reload,
        move |_| {
            scroller.vadjustment().set_value(0.0);
            reload();
        }
    ));

    let pills = build_pills(kind_filter.clone(), reload.clone());
    {
        let shown = Cell::new(super::library_scope::current_id());
        let reload = Rc::downgrade(&reload);
        super::library_scope::subscribe(Rc::new(move || {
            let now = super::library_scope::current_id();
            if shown.replace(now) != now
                && let Some(reload) = reload.upgrade()
            {
                reload();
            }
        }));
    }

    let action_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    action_row.append(&search);
    action_row.append(&pills);

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .build();
    header.append(&title_row);
    let bulk = BulkBar::new(
        engine.clone(),
        Rc::new(clone!(
            #[strong]
            selection,
            move || selected_rows(&selection).into_iter().map(|(_, row)| row).collect()
        )),
        reload.clone(),
        Rc::new(clone!(
            #[strong]
            selection,
            move || {
                selection.unselect_all();
            }
        )),
    );
    bulk.selection_changed(0);
    let _ = bulk_hook.replace(Box::new(clone!(
        #[strong]
        bulk,
        move |count| bulk.selection_changed(count)
    )));
    header.append(&bulk.root);
    header.append(&action_row);

    let banner_title = crate::v3::label("", &["heading"]);
    banner_title.set_xalign(0.0);
    banner_title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    let banner_text = crate::v3::vbox(1);
    banner_text.set_hexpand(true);
    banner_text.append(&banner_title);
    banner_text.append(&crate::v3::meta("Most similar first. Clear to see every file again."));
    let banner_clear = crate::v3::secondary("Clear", false);
    let banner = crate::v3::hbox(10);
    banner.add_css_class("glass-card");
    banner.append(&gtk::Image::from_icon_name("system-search-symbolic"));
    banner.append(&banner_text);
    banner.append(&banner_clear);
    banner.set_visible(false);
    header.append(&banner);
    {
        let (similar, reload, banner, banner_title) =
            (similar.clone(), reload.clone(), banner.clone(), banner_title.clone());
        let _ = similar_hook.replace(Box::new(move || {
            let seed = similar.borrow().clone();
            banner.set_visible(seed.is_some());
            if let Some((_, name)) = seed {
                banner_title.set_label(&format!("Photos similar to {name}"));
            }
            reload();
        }));
    }
    banner_clear.connect_clicked(clone!(
        #[strong]
        similar,
        #[strong]
        similar_hook,
        move |_| {
            similar.replace(None);
            similar_hook.borrow()();
        }
    ));

    // Debounced search → reload.
    search.connect_search_changed(clone!(
        #[strong]
        search_text,
        #[strong]
        reload,
        #[strong]
        debounce_gen,
        #[strong]
        similar,
        #[strong]
        similar_hook,
        move |entry| {
            *search_text.borrow_mut() = entry.text().to_string();
            if similar.borrow_mut().take().is_some() {
                similar_hook.borrow()();
                return;
            }
            let g = debounce_gen.get().wrapping_add(1);
            debounce_gen.set(g);
            let reload = reload.clone();
            let debounce_gen = debounce_gen.clone();
            glib::timeout_add_local_once(Duration::from_millis(220), move || {
                if debounce_gen.get() == g {
                    reload();
                }
            });
        }
    ));

    let refresh_offline: Rc<dyn Fn()> = {
        let generation = Rc::new(Cell::new(0u64));
        let offline = offline.clone();
        let pane = Rc::downgrade(&pane);
        Rc::new(move || {
            let current = generation.get().wrapping_add(1);
            generation.set(current);
            let rx = offline_roots::read();
            super::library_scope::refresh();
            let (generation, offline, pane) = (generation.clone(), offline.clone(), pane.clone());
            glib::MainContext::default().spawn_local(async move {
                let Ok(roots) = rx.recv().await else { return };
                if generation.get() != current || *offline.borrow() == roots {
                    return;
                }
                offline.replace(roots);
                refresh_offline_badges(&offline.borrow());
                if let Some(pane) = pane.upgrade() {
                    pane.apply_offline();
                }
            });
        })
    };
    let volumes = gio::VolumeMonitor::get();
    for signal in ["mount-added", "mount-removed"] {
        let refresh = Rc::downgrade(&refresh_offline);
        volumes.connect_local(signal, false, move |_| {
            if let Some(refresh) = refresh.upgrade() {
                refresh();
            }
            None
        });
    }

    // Live-scan reloads: throttle on batches, final reload on completion.
    let ev_rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(
        #[strong]
        reload,
        #[strong]
        pane,
        #[strong]
        refresh_offline,
        #[strong]
        gate,
        #[strong]
        live_ctx,
        #[weak]
        scroller,
        #[weak]
        new_files,
        async move {
            let mut last = Instant::now() - Duration::from_secs(10);
            while let Ok(ev) = ev_rx.recv().await {
                if !changes_library(&ev) {
                    continue;
                }
                // Off screen the grid only notes that it is out of date.
                if !gate.is_mapped() {
                    gate.mark_stale();
                    continue;
                }
                let finished = matches!(*ev, EngineEvent::ScanComplete(_));
                let scanning = matches!(*ev, EngineEvent::BatchLanded(_));
                let at_top = scroller.vadjustment().value() <= AT_TOP_PX;
                match live_step(at_top, scanning && last.elapsed() < LIVE_RELOAD_EVERY, finished) {
                    LiveStep::Reload => {
                        last = Instant::now();
                        match live_ctx.borrow().as_ref() {
                            Some(ctx) if scanning => run_reload(ctx, true),
                            _ => reload(),
                        }
                    }
                    LiveStep::OfferNewFiles => {
                        gate.mark_stale();
                        new_files.set_visible(true);
                    }
                    LiveStep::Wait => {}
                }
                if finished {
                    refresh_pane_details(&pane);
                    refresh_offline();
                }
            }
        }
    ));

    refresh_offline();

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
    let content_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    content_row.append(&content_stack);
    content_row.append(&pane.revealer);
    root.append(&content_row);
    // The grid is read the first time the page is shown, and again on a later
    // visit only when something changed while it was away; a user scrolled
    // down keeps their place and is offered the new files instead.
    {
        let reload = reload.clone();
        let gate = gate.clone();
        let refresh_offline = refresh_offline.clone();
        root.connect_map(clone!(
            #[weak]
            scroller,
            #[weak]
            new_files,
            move |_| {
                let first = !gate.has_loaded();
                if gate.on_map() {
                    if first || scroller.vadjustment().value() <= AT_TOP_PX {
                        reload();
                    } else {
                        gate.mark_stale();
                        new_files.set_visible(true);
                    }
                }
                refresh_offline();
            }
        ));
    }
    {
        let gate = gate.clone();
        root.connect_unmap(move |_| gate.on_unmap());
    }
    root.upcast()
}

// ── Reload ───────────────────────────────────────────────────────────────────
/// Appends the next page when the grid nears its end.
fn load_more(ctx: &Rc<ReloadCtx>) {
    if !ctx.has_more.get() || ctx.loading_more.get() {
        return;
    }
    ctx.loading_more.set(true);
    let g = ctx.query_gen.get();
    let offset = ctx.model.n_items() as i64;
    let spec = QuerySpec {
        search: ctx.search_text.borrow().clone(),
        kind: ctx.kind_filter.borrow().clone(),
        limit: QUERY_LIMIT,
        offset,
        scope: super::library_scope::current_param(),
        ..QuerySpec::default()
    };
    let rx = ctx.engine.borrow().query_files(spec);
    let ctx = ctx.clone();
    glib::MainContext::default().spawn_local(async move {
        let result = rx.recv().await;
        ctx.loading_more.set(false);
        if ctx.query_gen.get() != g || ctx.model.n_items() as i64 != offset {
            return;
        }
        let Ok(Ok((rows, total))) = result else {
            return;
        };
        let items: Vec<BoxedAnyObject> = rows.iter().cloned().map(BoxedAnyObject::new).collect();
        ctx.model.extend_from_slice(&items);
        let shown = ctx.model.n_items() as i64;
        ctx.total.set(total);
        ctx.has_more.set(!rows.is_empty() && shown < total);
        ctx.count_label.set_text(&if shown < total {
            format!("{shown} of {total} files")
        } else {
            format!("{shown} files")
        });
    });
}

struct ReloadCtx {
    gate: Rc<super::reload_gate::ReloadGate>,
    new_files: gtk::Button,
    /// When the total for the current query was last counted.
    counted_at: Cell<Option<Instant>>,
    engine: Rc<RefCell<EngineClient>>,
    model: gio::ListStore,
    count_label: gtk::Label,
    content_stack: gtk::Stack,
    empty_page: adw::StatusPage,
    search_text: Rc<RefCell<String>>,
    kind_filter: Rc<RefCell<Option<String>>>,
    query_gen: Rc<Cell<u64>>,
    has_more: Rc<Cell<bool>>,
    loading_more: Rc<Cell<bool>>,
    total: Rc<Cell<i64>>,
    last_query: RefCell<Option<(String, Option<String>)>>,
    selection: gtk::MultiSelection,
    pane: Rc<SelectedPane>,
    restoring: Rc<Cell<bool>>,
    similar: Rc<RefCell<Option<(i64, String)>>>,
}

/// Rows to re-read on a refresh: the loaded pages when the query is the same
/// (a live-scan reload must not throw a deep-scrolled user back to the top),
/// one page when the search or kind changed. macOS caps this at 3000.
fn reload_limit(same_query: bool, loaded: u32) -> i64 {
    if same_query {
        i64::from(loaded).clamp(QUERY_LIMIT, LIVE_RELOAD_MAX)
    } else {
        QUERY_LIMIT
    }
}

/// `live` is a reload driven by scan batches: it may reuse a total counted
/// moments ago instead of counting the whole library again.
fn run_reload(ctx: &Rc<ReloadCtx>, live: bool) {
    ctx.gate.loaded();
    ctx.new_files.set_visible(false);
    let g = ctx.query_gen.get().wrapping_add(1);
    ctx.query_gen.set(g);
    let seed = ctx.similar.borrow().clone();
    if let Some((seed_id, _)) = seed {
        ctx.has_more.set(false);
        run_similar(ctx, g, seed_id);
        return;
    }
    let query = (ctx.search_text.borrow().trim().to_owned(), ctx.kind_filter.borrow().clone());
    let same_query = ctx.last_query.borrow().as_ref() == Some(&query);
    let limit = reload_limit(same_query, ctx.model.n_items());
    ctx.last_query.replace(Some(query));
    let known_total = (live && same_query && ctx.counted_at.get().is_some_and(|at| at.elapsed() < COUNT_FRESH_FOR))
        .then(|| ctx.total.get());
    if known_total.is_none() {
        ctx.counted_at.set(Some(Instant::now()));
    }
    let spec = QuerySpec {
        search: ctx.search_text.borrow().clone(),
        kind: ctx.kind_filter.borrow().clone(),
        limit,
        offset: 0,
        scope: super::library_scope::current_param(),
        known_total,
        ..QuerySpec::default()
    };
    ctx.has_more.set(false);
    let searched = spec.search.trim().to_string();
    let filtered = !searched.is_empty() || spec.kind.is_some();
    let rx = ctx.engine.borrow().query_files(spec);
    let ctx = ctx.clone();
    glib::MainContext::default().spawn_local(async move {
        let result = rx.recv().await;
        if ctx.query_gen.get() != g {
            return;
        }
        let (rows, total) = match result {
            Ok(Ok(data)) => data,
            Ok(Err(error)) => {
                ctx.model.remove_all();
                ctx.count_label.set_text("Library not ready");
                ctx.empty_page.set_title("Couldn't open the library");
                ctx.empty_page.set_description(Some(&error.to_string()));
                ctx.content_stack.set_visible_child_name("empty");
                return;
            }
            Err(_) => return,
        };
        show_rows(&ctx, rows, total, &searched, filtered);
    });
}

fn show_rows(ctx: &Rc<ReloadCtx>, rows: Vec<FileRow>, total: i64, searched: &str, filtered: bool) {
    // One items-changed signal for the whole page instead of one per row.
    let items: Vec<BoxedAnyObject> = rows.iter().cloned().map(BoxedAnyObject::new).collect();
    // Keep the same files selected (and the pane on them) across a refresh.
    let selected: HashSet<i64> = selected_rows(&ctx.selection).into_iter().map(|(_, row)| row.id).collect();
    ctx.restoring.set(true);
    ctx.model.splice(0, ctx.model.n_items(), &items);
    let positions = picking::restore(&selected, rows.iter().map(|row| row.id));
    let chosen = gtk::Bitset::new_empty();
    for position in &positions {
        chosen.add(*position);
    }
    ctx.selection.set_selection(&chosen, &gtk::Bitset::new_range(0, ctx.model.n_items()));
    ctx.restoring.set(false);
    show_selection(&ctx.selection, &ctx.pane, true);
    ctx.total.set(total);
    ctx.has_more.set(total > rows.len() as i64);
    if total > rows.len() as i64 {
        ctx.count_label.set_text(&format!("{} of {} files", rows.len(), total));
    } else {
        ctx.count_label.set_text(&format!("{} files", rows.len()));
    }
    if rows.is_empty() {
        if ctx.similar.borrow().is_some() {
            ctx.empty_page.set_icon_name(Some("edit-find-symbolic"));
            ctx.empty_page.set_title("No similar files");
            ctx.empty_page.set_description(Some(
                "FileID found nothing like this file. Similar files need the file to be read by a scan first.",
            ));
        } else if filtered {
            ctx.empty_page.set_icon_name(Some("edit-find-symbolic"));
            ctx.empty_page.set_title("No matches");
            let description = if searched.is_empty() {
                "Nothing matches this filter. Try another kind, or choose All to see every file.".to_string()
            } else {
                format!("Nothing matches “{searched}”. Try other words, or choose All to see every file.")
            };
            ctx.empty_page.set_description(Some(&description));
        } else {
            ctx.empty_page.set_icon_name(Some("view-grid-symbolic"));
            ctx.empty_page.set_title("No files yet");
            ctx.empty_page.set_description(Some(
                "Files appear here as the scan reads them. If nothing is happening, press Scan in the sidebar. Scanning needs the two models in Settings; nothing downloads until you ask.",
            ));
        }
        ctx.content_stack.set_visible_child_name("empty");
    } else {
        ctx.content_stack.set_visible_child_name("grid");
    }
}

// ── Find Similar ─────────────────────────────────────────────────────────────

/// Tries a visual-index request this many times a second apart while the
/// engine is still building its index (macOS retries for two minutes).
const SIMILAR_TRIES: u32 = 120;
const SIMILAR_LIMIT: usize = 60;

/// Files most like `seed_id`, ranked by the engine's catalog search in
/// semantic mode on the seed file (the same call the Mac makes), kept to the
/// chosen location and kind.
fn run_similar(ctx: &Rc<ReloadCtx>, generation: u64, seed_id: i64) {
    let ctx = ctx.clone();
    glib::MainContext::default().spawn_local(async move {
        ctx.count_label.set_text("Finding similar files…");
        let mut response = None;
        for attempt in 0..SIMILAR_TRIES {
            let reply = similar_request(&ctx.engine, seed_id).await;
            if ctx.query_gen.get() != generation {
                return;
            }
            match reply {
                Some(reply) if similar_pending(&reply) && attempt + 1 < SIMILAR_TRIES => {
                    glib::timeout_future_seconds(1).await;
                    if ctx.query_gen.get() != generation {
                        return;
                    }
                }
                other => {
                    response = other;
                    break;
                }
            }
        }
        let Some(response) = response.filter(|r| r.status == "ok") else {
            ctx.model.remove_all();
            ctx.count_label.set_text("Couldn't find similar files");
            ctx.empty_page.set_icon_name(Some("edit-find-symbolic"));
            ctx.empty_page.set_title("Couldn't find similar files");
            ctx.empty_page.set_description(Some(
                "FileID is still getting ready, or this file hasn't been read yet. Try again in a moment.",
            ));
            ctx.content_stack.set_visible_child_name("empty");
            return;
        };
        let ids: Vec<i64> = response.hits.iter().map(|hit| hit.file_id).collect();
        let kind = ctx.kind_filter.borrow().clone();
        let rows = read_rows_in_order(ids, kind).recv().await.unwrap_or_default();
        if ctx.query_gen.get() != generation {
            return;
        }
        let total = rows.len() as i64;
        show_rows(&ctx, rows, total, "", true);
    });
}

fn similar_pending(response: &fileid_engine::ipc::CatalogResponse) -> bool {
    response.status == "indexing"
        || (response.status == "error"
            && response.message.as_deref().is_some_and(|m| m.starts_with("Preparing the local visual search index")))
}

async fn similar_request(
    engine: &Rc<RefCell<EngineClient>>,
    seed_id: i64,
) -> Option<fileid_engine::ipc::CatalogResponse> {
    let mut request = super::workbench_state::catalog_request("search");
    request.file_id = Some(seed_id);
    request.search_mode = Some("semantic".into());
    request.limit = Some(SIMILAR_LIMIT as _);
    request.root_id = super::library_scope::current_id();
    request.result_scope = Some("files".into());
    let id = request.request_id.clone();
    let events = engine.borrow_mut().subscribe();
    let _deadline = events.deadline(Duration::from_secs(10));
    let sent = engine.borrow_mut().send(fileid_engine::ipc::CommandPayload::CatalogRequest(Box::new(
        fileid_engine::ipc::CatalogRequestPayload { request },
    )));
    let mut found = None;
    if sent.is_ok() {
        while let Ok(event) = events.recv().await {
            match &*event {
                EngineEvent::CatalogResponse(response) if response.request_id == id => {
                    found = Some(response.clone());
                    break;
                }
                EngineEvent::Exited => break,
                _ => {}
            }
        }
    }
    found
}

/// The rows for `ids` in that order, of one `kind` when set; files gone from the catalog drop out.
fn read_rows_in_order(ids: Vec<i64>, kind: Option<String>) -> async_channel::Receiver<Vec<FileRow>> {
    crate::engine_client::read_db(move |conn| {
        let mut found = HashMap::new();
        let list = ids.iter().map(i64::to_string).collect::<Vec<_>>().join(",");
        let sql = format!(
            "SELECT f.id, f.path_text, f.kind, f.extension, f.size_bytes, f.created_at, f.modified_at, \
             f.has_faces, f.has_text, f.file_ref, f.content_hash, f.vlm_description, f.vlm_proposed_name, \
             f.vlm_inherited_from IS NOT NULL FROM files f WHERE f.failed = 0 AND f.id IN ({list})"
        );
        if let Ok(mut stmt) = conn.prepare(&sql)
            && let Ok(rows) = stmt.query_map([], |row| {
                let path: String = row.get(1)?;
                let name = std::path::Path::new(&path)
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone());
                Ok(FileRow {
                    id: row.get(0)?,
                    path,
                    name,
                    kind: row.get(2)?,
                    extension: row.get(3)?,
                    size_bytes: row.get(4)?,
                    created_at: row.get(5)?,
                    modified_at: row.get(6)?,
                    has_faces: row.get(7)?,
                    has_text: row.get(8)?,
                    file_ref: row.get(9)?,
                    content_hash: row.get(10)?,
                    description: row.get(11)?,
                    proposed_name: row.get(12)?,
                    caption_inherited: row.get(13)?,
                })
            })
        {
            for row in rows.flatten() {
                found.insert(row.id, row);
            }
        }
        ids.iter()
            .filter_map(|id| found.remove(id))
            .filter(|row| kind.as_deref().is_none_or(|kind| row.kind == kind))
            .collect()
    })
}

// ── Kind pills ───────────────────────────────────────────────────────────────

fn build_pills(kind_filter: Rc<RefCell<Option<String>>>, reload: Rc<dyn Fn()>) -> gtk::Box {
    let kinds: [(&str, Option<&str>); 6] = [
        ("All", None),
        ("Images", Some("image")),
        ("Videos", Some("video")),
        ("Docs", Some("doc")),
        ("PDFs", Some("pdf")),
        ("Audio", Some("audio")),
    ];
    let pillbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .build();
    let buttons: Rc<RefCell<Vec<gtk::Button>>> = Rc::new(RefCell::new(Vec::new()));

    for (i, (label, value)) in kinds.iter().enumerate() {
        let btn = gtk::Button::builder()
            .label(*label)
            .css_classes(["pill"])
            .build();
        if i == 0 {
            btn.add_css_class("pill-active");
        }
        let value = value.map(|s| s.to_string());
        btn.connect_clicked(clone!(
            #[strong]
            kind_filter,
            #[strong]
            buttons,
            #[strong]
            reload,
            move |b| {
                for other in buttons.borrow().iter() {
                    other.remove_css_class("pill-active");
                }
                b.add_css_class("pill-active");
                *kind_filter.borrow_mut() = value.clone();
                reload();
            }
        ));
        pillbox.append(&btn);
        buttons.borrow_mut().push(btn);
    }
    pillbox
}

// ── Tile widget ──────────────────────────────────────────────────────────────

fn build_tile() -> gtk::Box {
    let vbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .width_request(168)
        .css_classes(["file-tile"])
        .build();
    let pic = gtk::Picture::builder()
        .content_fit(gtk::ContentFit::Cover)
        .height_request(132)
        .hexpand(true)
        .css_classes(["tile-thumb"])
        .build();
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&pic));
    let play = gtk::Image::builder()
        .icon_name("media-playback-start-symbolic")
        .pixel_size(22)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .visible(false)
        .css_classes(["video-play-badge"])
        .build();
    overlay.add_overlay(&play);
    let offline = gtk::Label::builder()
        .label("OFFLINE")
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_start(6)
        .margin_top(6)
        .visible(false)
        .css_classes(["v3-pill", "tone-warning", "tile-offline"])
        .build();
    overlay.add_overlay(&offline);
    let name = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .build();
    let caption = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["tile-caption"])
        .build();
    vbox.append(&overlay);
    vbox.append(&name);
    vbox.append(&caption);
    vbox
}

struct TileParts {
    pic: gtk::Picture,
    play: gtk::Image,
    offline: gtk::Label,
    name: gtk::Label,
    caption: gtk::Label,
}

/// Resolve the fixed tile-widget structure built by `build_tile`.
fn tile_parts(vbox: &gtk::Box) -> Option<TileParts> {
    let overlay = vbox.first_child()?.downcast::<gtk::Overlay>().ok()?;
    let pic = overlay.child()?.downcast::<gtk::Picture>().ok()?;
    let play = pic.next_sibling()?.downcast::<gtk::Image>().ok()?;
    let offline = play.next_sibling()?.downcast::<gtk::Label>().ok()?;
    let name = overlay.next_sibling()?.downcast::<gtk::Label>().ok()?;
    let caption = name.next_sibling()?.downcast::<gtk::Label>().ok()?;
    Some(TileParts {
        pic,
        play,
        offline,
        name,
        caption,
    })
}

/// 512 tiles at 1x; fewer as HiDPI tiles grow.
const THUMB_CACHE_BYTES: usize = 512 * (TILE_THUMB_PX as usize).pow(2) * 4;

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct ThumbCacheKey {
    id: i64,
    path: String,
    size_bytes: i64,
    modified_bits: Option<u64>,
    file_ref: Option<i64>,
    content_hash: Option<Vec<u8>>,
}

impl ThumbCacheKey {
    fn for_row(row: &FileRow) -> Self {
        Self {
            id: row.id,
            path: row.path.clone(),
            size_bytes: row.size_bytes,
            modified_bits: row.modified_at.map(f64::to_bits),
            file_ref: row.file_ref,
            content_hash: row.content_hash.clone(),
        }
    }
}

thread_local! {
    static THUMB_CACHE: RefCell<(
        HashMap<ThumbCacheKey, gtk::gdk::Texture>,
        VecDeque<ThumbCacheKey>,
        usize,
    )> = RefCell::new((HashMap::new(), VecDeque::new(), 0));
}

fn thumb_cache_get(key: &ThumbCacheKey) -> Option<gtk::gdk::Texture> {
    THUMB_CACHE.with(|cache| cache.borrow().0.get(key).cloned())
}

fn texture_bytes(texture: &gtk::gdk::Texture) -> usize {
    texture.width().max(0) as usize * texture.height().max(0) as usize * 4
}

fn thumb_cache_put(key: ThumbCacheKey, texture: gtk::gdk::Texture) {
    THUMB_CACHE.with(|cache| {
        let (map, order, bytes) = &mut *cache.borrow_mut();
        *bytes += texture_bytes(&texture);
        match map.insert(key.clone(), texture) {
            Some(old) => *bytes -= texture_bytes(&old),
            None => order.push_back(key),
        }
        while *bytes > THUMB_CACHE_BYTES && order.len() > 1 {
            if let Some(evict) = order.pop_front()
                && let Some(old) = map.remove(&evict)
            {
                *bytes -= texture_bytes(&old);
            }
        }
    });
}

fn bind_tile(engine: &Rc<RefCell<EngineClient>>, list_item: &gtk::ListItem, offline: &OfflineRoots) {
    let Some(obj) = list_item.item() else { return };
    let Ok(boxed) = obj.downcast::<BoxedAnyObject>() else {
        return;
    };
    let row = boxed.borrow::<FileRow>();

    let Some(vbox) = list_item
        .child()
        .and_then(|w| w.downcast::<gtk::Box>().ok())
    else {
        return;
    };
    let Some(TileParts {
        pic,
        play,
        offline: offline_badge,
        name,
        caption,
    }) = tile_parts(&vbox)
    else {
        return;
    };
    BOUND_TILES.with_borrow_mut(|tiles| tiles.insert(tile_token_key(list_item), list_item.downgrade()));
    offline_badge.set_visible(offline.is_offline(&row.path));

    // Filename — the suggested name in gold when present (macOS parity).
    name.remove_css_class("gold-accent");
    match row.proposed_name.as_ref().filter(|s| !s.is_empty()) {
        Some(p) => {
            name.set_text(&format!("{}.{}", p, row.extension));
            name.add_css_class("gold-accent");
        }
        None => name.set_text(&row.name),
    }
    caption.set_text(&format!(
        "{} · {}",
        row.kind.to_uppercase(),
        file_size_i64(row.size_bytes)
    ));

    let is_video = row.kind == "video";
    play.set_visible(false);
    if row.kind == "image" || is_video {
        let key = ThumbCacheKey::for_row(&row);
        if let Some(tex) = thumb_cache_get(&key) {
            pic.set_paintable(Some(&tex));
            play.set_visible(is_video);
            return;
        }
        pic.set_paintable(icon_paintable(icon_for_kind(&row.kind), 96).as_ref());
        let parts = TileRequest {
            engine: engine.clone(),
            list_item: list_item.downgrade(),
            pic: pic.downgrade(),
            play: play.downgrade(),
            key,
            is_video,
        };
        request_tile_thumbnail(list_item, parts, true);
    } else {
        pic.set_paintable(icon_paintable(icon_for_kind(&row.kind), 96).as_ref());
    }
}

struct TileRequest {
    engine: Rc<RefCell<EngineClient>>,
    list_item: glib::WeakRef<gtk::ListItem>,
    pic: glib::WeakRef<gtk::Picture>,
    play: glib::WeakRef<gtk::Image>,
    key: ThumbCacheKey,
    is_video: bool,
}

thread_local! {
    static TILE_TOKENS: RefCell<HashMap<usize, CancelToken>> = RefCell::new(HashMap::new());
    static BOUND_TILES: RefCell<HashMap<usize, glib::WeakRef<gtk::ListItem>>> = RefCell::new(HashMap::new());
}

/// Shows or hides the OFFLINE badge on every bound tile in place, so a drive
/// coming or going never reloads the grid, its pages or its thumbnails.
fn refresh_offline_badges(offline: &OfflineRoots) {
    BOUND_TILES.with_borrow_mut(|tiles| {
        tiles.retain(|_, weak| {
            let Some(list_item) = weak.upgrade() else {
                return false;
            };
            let row = list_item
                .item()
                .and_downcast::<BoxedAnyObject>()
                .map(|boxed| boxed.borrow::<FileRow>().path.clone());
            let parts = list_item.child().and_downcast::<gtk::Box>().and_then(|vbox| tile_parts(&vbox));
            if let (Some(path), Some(parts)) = (row, parts) {
                parts.offline.set_visible(offline.is_offline(&path));
            }
            true
        });
    });
}

fn tile_token_key(list_item: &gtk::ListItem) -> usize {
    list_item.as_ptr() as usize
}

/// A fresh cancel token for the tile; the previous binding's work is dropped.
fn renew_tile_token(list_item: &gtk::ListItem) -> CancelToken {
    let token = CancelToken::default();
    let previous = TILE_TOKENS.with_borrow_mut(|tokens| tokens.insert(tile_token_key(list_item), token.clone()));
    if let Some(previous) = previous {
        previous.store(true, Ordering::Release);
    }
    token
}

fn cancel_tile_token(list_item: &gtk::ListItem) {
    if let Some(token) = TILE_TOKENS.with_borrow_mut(|tokens| tokens.remove(&tile_token_key(list_item))) {
        token.store(true, Ordering::Release);
    }
}

fn request_tile_thumbnail(list_item: &gtk::ListItem, request: TileRequest, retry: bool) {
    let cancel = renew_tile_token(list_item);
    let dim = request.pic.upgrade().map_or(TILE_THUMB_PX, |pic| decode_px(&pic, TILE_THUMB_PX));
    let rx = request
        .engine
        .borrow()
        .request_tile(request.key.path.clone(), dim, request.is_video, cancel);
    glib::MainContext::default().spawn_local(async move {
        let reply = rx.recv().await.unwrap_or(ThumbReply::Dropped);
        let still_wanted = tile_still_wants(&request.list_item, &request.key);
        let decoded = match reply {
            ThumbReply::Image(decoded) => decoded,
            ThumbReply::Dropped if retry && still_wanted => {
                if let Some(list_item) = request.list_item.upgrade() {
                    request_tile_thumbnail(&list_item, request, false);
                }
                return;
            }
            ThumbReply::Dropped | ThumbReply::Failed => return,
        };
        let tex = texture_from_decoded(decoded);
        thumb_cache_put(request.key.clone(), tex.clone());
        if !still_wanted {
            return;
        }
        if let Some(pic) = request.pic.upgrade() {
            pic.set_paintable(Some(&tex));
        }
        if let Some(play) = request.play.upgrade() {
            play.set_visible(request.is_video);
        }
    });
}

fn clear_tile(tile: &gtk::Widget) {
    let Some(vbox) = tile.downcast_ref::<gtk::Box>() else {
        return;
    };
    if let Some(parts) = tile_parts(vbox) {
        parts.pic.set_paintable(None::<&gtk::gdk::Texture>);
        parts.play.set_visible(false);
        parts.offline.set_visible(false);
    }
}

/// People and tags change at the end of a scan without touching the file
/// row, so the Selected pane re-reads its file then.
fn refresh_pane_details(pane: &Rc<SelectedPane>) {
    let Some(id) = pane.current_id() else { return };
    let rx = crate::engine_client::read_file_row(id);
    let pane = Rc::downgrade(pane);
    glib::MainContext::default().spawn_local(async move {
        if let (Ok(Some(row)), Some(pane)) = (rx.recv().await, pane.upgrade()) {
            pane.refresh(row, true);
        }
    });
}

// ── Preview dialog ───────────────────────────────────────────────────────────

/// The file preview dialog. Shows a large image + metadata and pages with the
/// ‹ / › buttons or the arrow keys. It navigates a frozen snapshot by file id,
/// upgraded to the whole filtered library, so live-scan reloads of the grid
/// (newest first) never shift what next/previous opens.
fn open_preview(
    engine: &Rc<RefCell<EngineClient>>,
    parent: &impl IsA<gtk::Widget>,
    snapshot: Vec<FileRow>,
    start: usize,
    query: (String, Option<String>),
) {
    if snapshot.is_empty() {
        return;
    }
    let idx = Rc::new(Cell::new(start.min(snapshot.len() - 1)));
    let rows = Rc::new(RefCell::new(snapshot));
    let preview_generation = Rc::new(Cell::new(0u64));

    let dialog = adw::Dialog::new();
    dialog.set_content_width(1000);
    dialog.set_content_height(680);

    let header = adw::HeaderBar::new();
    let prev_btn = gtk::Button::builder()
        .icon_name("go-previous-symbolic")
        .css_classes(["flat"])
        .tooltip_text("Previous (←)")
        .build();
    prev_btn.update_property(&[gtk::accessible::Property::Label("Previous")]);
    let next_btn = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .css_classes(["flat"])
        .tooltip_text("Next (→)")
        .build();
    next_btn.update_property(&[gtk::accessible::Property::Label("Next")]);
    let counter = gtk::Label::builder().css_classes(["dim-label", "numeric"]).build();
    header.pack_start(&prev_btn);
    header.pack_start(&counter);
    header.pack_end(&next_btn);
    let open_btn = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .css_classes(["flat"])
        .tooltip_text("Open in the default app")
        .build();
    open_btn.update_property(&[gtk::accessible::Property::Label("Open in the default app")]);
    let reveal_btn = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .css_classes(["flat"])
        .tooltip_text("Show in Files")
        .build();
    reveal_btn.update_property(&[gtk::accessible::Property::Label("Show in Files")]);
    header.pack_end(&open_btn);
    header.pack_end(&reveal_btn);

    let pic = gtk::Picture::builder()
        .hexpand(true)
        .vexpand(true)
        .content_fit(gtk::ContentFit::Contain)
        .build();
    let meta_scroll = gtk::ScrolledWindow::builder()
        .width_request(320)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    // Videos and audio play in place (GTK's media backend) instead of
    // launching another application.
    let video = gtk::Video::builder()
        .hexpand(true)
        .vexpand(true)
        .autoplay(false)
        .build();
    let canvas = gtk::Stack::builder().hexpand(true).vexpand(true).build();
    canvas.add_named(&pic, Some("image"));
    canvas.add_named(&video, Some("media"));
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();
    body.append(&canvas);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    body.append(&meta_scroll);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    let current = {
        let rows = rows.clone();
        let idx = idx.clone();
        move || rows.borrow().get(idx.get()).cloned()
    };
    open_btn.connect_clicked(clone!(
        #[strong]
        current,
        move |button| {
            if let Some(row) = current() {
                crate::v3::open_file(button, &row.path);
            }
        }
    ));
    reveal_btn.connect_clicked(clone!(
        #[strong]
        current,
        move |button| {
            if let Some(row) = current() {
                crate::v3::show_in_files(button, &row.path);
            }
        }
    ));

    // The preview starts with the rows the grid has loaded and reads the next
    // page only as the user nears the end of them.
    let more = Rc::new(Cell::new(preview_can_page(rows.borrow().len())));
    let update_nav: Rc<dyn Fn()> = {
        let rows = rows.clone();
        let idx = idx.clone();
        let counter = counter.clone();
        let prev_btn = prev_btn.clone();
        let next_btn = next_btn.clone();
        let more = more.clone();
        Rc::new(move || {
            let (pos, len) = (idx.get(), rows.borrow().len());
            let open_ended = if more.get() { "+" } else { "" };
            counter.set_label(&format!("{} of {}{open_ended}", pos + 1, len));
            prev_btn.set_sensitive(pos > 0);
            next_btn.set_sensitive(pos + 1 < len);
        })
    };
    let fetch_more: Rc<dyn Fn()> = {
        let fetching = Rc::new(Cell::new(false));
        let (engine, rows, more, update_nav) = (engine.clone(), rows.clone(), more.clone(), update_nav.clone());
        Rc::new(move || {
            if !more.get() || fetching.replace(true) {
                return;
            }
            let page = engine.borrow().query_files(QuerySpec {
                search: query.0.clone(),
                kind: query.1.clone(),
                limit: QUERY_LIMIT,
                offset: rows.borrow().len() as i64,
                channel: QueryChannel::Preview,
                scope: super::library_scope::current_param(),
                known_total: Some(0),
            });
            let (rows, more, update_nav, fetching) = (rows.clone(), more.clone(), update_nav.clone(), fetching.clone());
            glib::MainContext::default().spawn_local(async move {
                let page = page.recv().await;
                fetching.set(false);
                let grew = match page {
                    Ok(Ok((page, _))) => {
                        let full = page.len() as i64 >= QUERY_LIMIT;
                        append_new_rows(&mut rows.borrow_mut(), page) && full
                    }
                    _ => false,
                };
                more.set(grew);
                update_nav();
            });
        })
    };

    let load: Rc<dyn Fn()> = {
        let engine = engine.clone();
        let pic = pic.clone();
        let video = video.clone();
        let canvas = canvas.clone();
        let meta_scroll = meta_scroll.clone();
        let dialog = dialog.clone();
        let preview_generation = preview_generation.clone();
        let update_nav = update_nav.clone();
        let current = current.clone();
        Rc::new(move || {
            let Some(row) = current() else {
                return;
            };
            dialog.set_title(&row.name);
            update_nav();
            meta_scroll.set_child(Some(&build_meta(&row)));
            pic.set_paintable(None::<&gtk::gdk::Texture>);
            if let Some(stream) = video.media_stream() {
                stream.pause();
            }
            let generation = preview_generation.get().wrapping_add(1);
            preview_generation.set(generation);
            if row.kind == "video" || row.kind == "audio" {
                video.set_file(Some(&gio::File::for_path(&row.path)));
                canvas.set_visible_child_name("media");
            } else {
                video.set_file(None::<&gio::File>);
                canvas.set_visible_child_name("image");
                load_preview_image(&engine, &pic, &row, preview_generation.clone(), generation);
            }
        })
    };

    let step: Rc<dyn Fn(i64)> = {
        let rows = rows.clone();
        let idx = idx.clone();
        let load = load.clone();
        let fetch_more = fetch_more.clone();
        Rc::new(move |delta| {
            let len = rows.borrow().len() as i64;
            let next = idx.get() as i64 + delta;
            if (0..len).contains(&next) {
                idx.set(next as usize);
                load();
            }
            if idx.get() + PREVIEW_PAGE_AHEAD >= rows.borrow().len() {
                fetch_more();
            }
        })
    };
    prev_btn.connect_clicked(clone!(
        #[strong]
        step,
        move |_| step(-1)
    ));
    next_btn.connect_clicked(clone!(
        #[strong]
        step,
        move |_| step(1)
    ));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(clone!(
        #[strong]
        step,
        move |_, key, _, _| match key {
            gtk::gdk::Key::Left | gtk::gdk::Key::Up => {
                step(-1);
                glib::Propagation::Stop
            }
            gtk::gdk::Key::Right | gtk::gdk::Key::Down => {
                step(1);
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    ));
    dialog.add_controller(keys);

    load();

    // A scan that finishes while the preview is open refreshes only the row
    // on screen (new caption, smart name, faces), not the frozen list.
    let closed = Rc::new(Cell::new(false));
    let events = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(
        #[strong]
        rows,
        #[strong]
        idx,
        #[strong]
        closed,
        #[weak]
        meta_scroll,
        #[weak]
        dialog,
        async move {
            while let Ok(event) = events.recv().await {
                if closed.get() {
                    break;
                }
                if !matches!(*event, EngineEvent::ScanComplete(_)) {
                    continue;
                }
                let Some(id) = rows.borrow().get(idx.get()).map(|row| row.id) else { continue };
                glib::MainContext::default().spawn_local(clone!(
                    #[strong]
                    rows,
                    #[strong]
                    idx,
                    #[strong]
                    closed,
                    #[weak]
                    meta_scroll,
                    #[weak]
                    dialog,
                    async move {
                        let Ok(Some(row)) = crate::engine_client::read_file_row(id).recv().await else { return };
                        if closed.get() || rows.borrow().get(idx.get()).map(|row| row.id) != Some(id) {
                            return;
                        }
                        rows.borrow_mut()[idx.get()] = row.clone();
                        dialog.set_title(&row.name);
                        meta_scroll.set_child(Some(&build_meta(&row)));
                    }
                ));
            }
        }
    ));

    if idx.get() + PREVIEW_PAGE_AHEAD >= rows.borrow().len() {
        fetch_more();
    }

    dialog.connect_closed(clone!(
        #[weak]
        video,
        #[strong]
        closed,
        move |_| {
            closed.set(true);
            if let Some(stream) = video.media_stream() {
                stream.pause();
            }
            video.set_file(None::<&gio::File>);
        }
    ));
    dialog.present(Some(parent));

    // Springy fade-in of the body, on the shared brand spring.
    let body_weak = body.downgrade();
    let _ = crate::spring::animate(&body, 0.0, 1.0, move |v| {
        if let Some(b) = body_weak.upgrade() {
            b.set_opacity(v);
        }
    });
}

/// Every selected tile with its row, in grid order.
fn selected_rows(selection: &gtk::MultiSelection) -> Vec<(u32, FileRow)> {
    let set = selection.selection();
    (0..set.size())
        .filter_map(|nth| u32::try_from(nth).ok())
        .map(|nth| set.nth(nth))
        .filter_map(|position| row_at(selection, position).map(|row| (position, row)))
        .collect()
}

/// Shares the selection with Chat and fills the Selected pane. `reloaded`
/// redraws a single file in place instead of re-reading its picture.
fn show_selection(selection: &gtk::MultiSelection, pane: &SelectedPane, reloaded: bool) {
    let rows = selected_rows(selection);
    super::chat_handoff::set_files(rows.iter().map(|(_, row)| row.id).collect());
    pane.set_picked(rows.iter().map(|(_, row)| (row.id, row.kind.clone())).collect());
    let sizes: Vec<(u32, i64)> = rows.iter().map(|(position, row)| (*position, row.size_bytes)).collect();
    match picking::view(&sizes) {
        PaneView::Hidden => pane.hide(),
        PaneView::One(_) => {
            let row = rows[0].1.clone();
            if reloaded && pane.current_id() == Some(row.id) {
                pane.refresh(row, false);
            } else {
                pane.show(row);
            }
        }
        PaneView::Many { count, bytes } => pane.show_many(count, bytes),
    }
}

/// Resolve the `FileRow` at `pos` in the (selection) model.
fn row_at(model: &impl IsA<gio::ListModel>, pos: u32) -> Option<FileRow> {
    let boxed = model.item(pos)?.downcast::<BoxedAnyObject>().ok()?;
    let r = boxed.borrow::<FileRow>();
    Some(r.clone())
}

fn snapshot_rows(model: &impl IsA<gio::ListModel>) -> Vec<FileRow> {
    (0..model.n_items()).filter_map(|pos| row_at(model, pos)).collect()
}

/// Build the metadata glass-card for one row (rebuilt on each navigation).
fn build_meta(row: &FileRow) -> gtk::Box {
    let meta = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .css_classes(["glass-card"])
        .build();
    meta.append(&meta_row("Path", &row.path));
    meta.append(&meta_row("Kind", &row.kind));
    meta.append(&meta_row("Size", &file_size_i64(row.size_bytes)));
    if let Some(d) = fmt_date(row.modified_at) {
        meta.append(&meta_row("Modified", &d));
    }
    if let Some(d) = fmt_date(row.created_at) {
        meta.append(&meta_row("Created", &d));
    }
    if row.has_faces {
        meta.append(&meta_row("Faces", "Found in this file"));
    }
    if row.has_text {
        meta.append(&meta_row("Text", "Found in this file"));
    }
    if let Some(desc) = row.description.as_ref().filter(|s| !s.is_empty()) {
        meta.append(&meta_row("Description", desc));
        if row.caption_inherited {
            meta.append(&inherited_caption_note());
        }
    }
    if let Some(p) = row.proposed_name.as_ref().filter(|s| !s.is_empty()) {
        meta.append(&meta_row("Suggested name", &format!("{}.{}", p, row.extension)));
    }
    super::library_pane::append_details(&meta, row.id);
    meta
}

pub(super) const INHERITED_CAPTION: &str = "Same as a similar photo";

pub(super) fn inherited_caption_note() -> gtk::Label {
    gtk::Label::builder()
        .label(INHERITED_CAPTION)
        .xalign(0.0)
        .css_classes(["caption", "dim-label"])
        .build()
}

/// Load one row's image (or a kind icon) into the preview `Picture`, off-thread.
fn load_preview_image(
    engine: &Rc<RefCell<EngineClient>>,
    pic: &gtk::Picture,
    row: &FileRow,
    active_generation: Rc<Cell<u64>>,
    generation: u64,
) {
    if row.kind == "image" || row.kind == "video" {
        let rx = engine
            .borrow()
            .request_preview(row.path.clone(), decode_px(pic, PREVIEW_PX), row.kind == "video");
        let kind = row.kind.clone();
        let pic_weak = pic.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let decoded = rx.recv().await.ok().and_then(ThumbReply::into_image);
            if active_generation.get() != generation {
                return;
            }
            let Some(pic) = pic_weak.upgrade() else {
                return;
            };
            match decoded {
                Some(decoded) => pic.set_paintable(Some(&texture_from_decoded(decoded))),
                // A video with no extractable keyframe (ffmpeg absent) still
                // deserves its kind icon rather than an empty pane.
                None => pic.set_paintable(icon_paintable(icon_for_kind(&kind), 128).as_ref()),
            }
        });
    } else {
        pic.set_paintable(icon_paintable(icon_for_kind(&row.kind), 128).as_ref());
    }
}

fn meta_row(key: &str, value: &str) -> gtk::Box {
    let r = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let k = gtk::Label::builder()
        .label(key)
        .xalign(1.0)
        .width_request(72)
        .css_classes(["dim-label"])
        .build();
    let v = gtk::Label::builder()
        .label(value)
        .xalign(0.0)
        .wrap(true)
        .selectable(true)
        .hexpand(true)
        .build();
    r.append(&k);
    r.append(&v);
    r
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// True if `list_item` still represents the file at `want` — i.e. the recycled
/// `GridView` tile has not been rebound to another row while a thumbnail was in
/// flight. Used to drop stale decodes before they paint onto the wrong tile.
fn tile_still_wants(li_weak: &glib::WeakRef<gtk::ListItem>, want: &ThumbCacheKey) -> bool {
    li_weak
        .upgrade()
        .and_then(|li| li.item())
        .and_then(|o| o.downcast::<BoxedAnyObject>().ok())
        .map(|object| ThumbCacheKey::for_row(&object.borrow::<FileRow>()) == *want)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_files_are_inserted_only_at_the_top_of_the_grid() {
        assert_eq!(live_step(true, false, false), LiveStep::Reload);
        assert_eq!(live_step(true, true, false), LiveStep::Wait);
        assert_eq!(live_step(true, true, true), LiveStep::Reload);
        assert_eq!(live_step(false, false, false), LiveStep::OfferNewFiles);
        assert_eq!(live_step(false, false, true), LiveStep::OfferNewFiles);
    }

    #[test]
    fn progress_noise_never_marks_the_grid_out_of_date() {
        assert!(changes_library(&EngineEvent::BatchLanded(4)));
        assert!(changes_library(&EngineEvent::ScanComplete(4)));
        assert!(changes_library(&EngineEvent::Ready));
        assert!(!changes_library(&EngineEvent::Spawning));
        assert!(!changes_library(&EngineEvent::Scanning));
        assert!(!changes_library(&EngineEvent::Exited));
    }

    #[test]
    fn the_preview_pages_from_loaded_rows_without_repeats() {
        assert!(!preview_can_page(0));
        assert!(!preview_can_page(60));
        assert!(preview_can_page(QUERY_LIMIT as usize));
        assert!(preview_can_page(2 * QUERY_LIMIT as usize));
        let row = |id: i64| FileRow {
            id,
            path: format!("/p/{id}.jpg"),
            name: format!("{id}.jpg"),
            kind: "image".into(),
            extension: "jpg".into(),
            size_bytes: 1,
            created_at: None,
            modified_at: None,
            has_faces: false,
            has_text: false,
            file_ref: None,
            content_hash: None,
            description: None,
            proposed_name: None,
            caption_inherited: false,
        };
        let mut rows = vec![row(3), row(2)];
        assert!(append_new_rows(&mut rows, vec![row(2), row(1)]));
        assert_eq!(rows.iter().map(|row| row.id).collect::<Vec<_>>(), [3, 2, 1]);
        assert!(!append_new_rows(&mut rows, vec![row(3), row(1)]));
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn live_reload_keeps_loaded_pages_until_the_query_changes() {
        assert_eq!(reload_limit(true, 0), QUERY_LIMIT);
        assert_eq!(reload_limit(true, 1800), 1800);
        assert_eq!(reload_limit(true, 9000), LIVE_RELOAD_MAX);
        assert_eq!(reload_limit(false, 1800), QUERY_LIMIT);
    }
}
