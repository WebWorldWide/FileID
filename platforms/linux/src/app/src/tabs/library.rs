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
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use gtk::glib::clone;
use gtk::glib::BoxedAnyObject;

use super::library_pane::SelectedPane;
use super::util::{fmt_date, format_bytes, icon_for_kind, icon_paintable};
use crate::engine_client::{
    texture_from_decoded, CancelToken, EngineClient, EngineEvent, FileRow, QueryChannel, QuerySpec, ThumbReply,
    QUERY_MAX_ROWS,
};
use std::sync::atomic::Ordering;

const QUERY_LIMIT: i64 = 600;
const LIVE_RELOAD_MAX: i64 = 3000;
const TILE_THUMB_PX: i32 = 256;
const PREVIEW_PX: i32 = 2400;

pub fn build(engine: Rc<RefCell<EngineClient>>) -> gtk::Widget {
    // ── Shared state ─────────────────────────────────────────────────────────
    let search_text = Rc::new(RefCell::new(String::new()));
    let kind_filter = Rc::new(RefCell::new(None::<String>));
    let query_gen = Rc::new(Cell::new(0u64));
    let debounce_gen = Rc::new(Cell::new(0u64));

    let model = gio::ListStore::new::<BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(model.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);

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
        move |_, list_item| {
            if let Some(li) = list_item.downcast_ref::<gtk::ListItem>() {
                bind_tile(&engine, li);
            }
        }
    ));
    factory.connect_unbind(|_, list_item| {
        if let Some(li) = list_item.downcast_ref::<gtk::ListItem>() {
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

    // Single click selects and fills the Selected pane; double-click/Enter
    // opens the preview; Esc clears the selection.
    let restoring = Rc::new(Cell::new(false));
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
                let pos = selection.selected();
                if pos == gtk::INVALID_LIST_POSITION {
                    return;
                }
                let query = (search_text.borrow().clone(), kind_filter.borrow().clone());
                open_preview(&engine, &grid, snapshot_rows(&selection), pos as usize, query);
            }
        ));
        let on_close: Rc<dyn Fn()> = Rc::new(clone!(
            #[strong]
            selection,
            move || selection.set_selected(gtk::INVALID_LIST_POSITION)
        ));
        SelectedPane::new(engine.clone(), on_preview, on_close)
    };
    selection.connect_selected_notify(clone!(
        #[strong]
        pane,
        #[strong]
        restoring,
        move |selection| {
            if restoring.get() {
                return;
            }
            match selection.selected_item().and_downcast::<BoxedAnyObject>() {
                Some(object) => pane.show(object.borrow::<FileRow>().clone()),
                None => pane.hide(),
            }
        }
    ));
    let escape = gtk::EventControllerKey::new();
    escape.connect_key_pressed(clone!(
        #[strong]
        selection,
        move |_, key, _, _| {
            if key == gtk::gdk::Key::Escape && selection.selected() != gtk::INVALID_LIST_POSITION {
                selection.set_selected(gtk::INVALID_LIST_POSITION);
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
    for (label, page) in [("Search & Moments", "moments"), ("File Tools", "tools")] {
        let button = gtk::Button::builder()
            .label(label)
            .action_name("win.navigate")
            .action_target(&page.to_variant())
            .build();
        title_row.append(&button);
    }

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search filenames, tags, text in images…")
        .hexpand(true)
        .css_classes(["fileid-search"])
        .build();

    // Reusable reload closure shared by search + pills + scan events.
    let reload: Rc<dyn Fn()> = {
        let ctx = Rc::new(ReloadCtx {
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
        });
        let near_end = ctx.clone();
        scroller.vadjustment().connect_value_changed(move |adj| {
            if adj.value() + adj.page_size() * 3.0 >= adj.upper() {
                load_more(&near_end);
            }
        });
        Rc::new(move || run_reload(&ctx))
    };

    let pills = build_pills(kind_filter.clone(), reload.clone());

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
    header.append(&action_row);

    // Debounced search → reload.
    search.connect_search_changed(clone!(
        #[strong]
        search_text,
        #[strong]
        reload,
        #[strong]
        debounce_gen,
        move |entry| {
            *search_text.borrow_mut() = entry.text().to_string();
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

    // Live-scan reloads: throttle on batches, final reload on completion.
    let ev_rx = engine.borrow_mut().subscribe();
    glib::MainContext::default().spawn_local(clone!(
        #[strong]
        reload,
        async move {
            let mut last = Instant::now() - Duration::from_secs(10);
            while let Ok(ev) = ev_rx.recv().await {
                match ev {
                    EngineEvent::Ready | EngineEvent::BatchLanded(_) => {
                        if last.elapsed() >= Duration::from_millis(900) {
                            last = Instant::now();
                            reload();
                        }
                    }
                    EngineEvent::ScanComplete(_) => reload(),
                    _ => {}
                }
            }
        }
    ));

    // Initial fill.
    reload();

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
    // Re-read on every tab switch: the startup read can race the engine's DB
    // open, and renames/deletes done from other tabs must show on return.
    {
        let reload = reload.clone();
        root.connect_map(move |_| reload());
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
    selection: gtk::SingleSelection,
    pane: Rc<SelectedPane>,
    restoring: Rc<Cell<bool>>,
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

fn run_reload(ctx: &Rc<ReloadCtx>) {
    let g = ctx.query_gen.get().wrapping_add(1);
    ctx.query_gen.set(g);
    let query = (ctx.search_text.borrow().trim().to_owned(), ctx.kind_filter.borrow().clone());
    let same_query = ctx.last_query.borrow().as_ref() == Some(&query);
    let limit = reload_limit(same_query, ctx.model.n_items());
    ctx.last_query.replace(Some(query));
    let spec = QuerySpec {
        search: ctx.search_text.borrow().clone(),
        kind: ctx.kind_filter.borrow().clone(),
        limit,
        offset: 0,
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
                ctx.count_label.set_text("Library unavailable");
                ctx.empty_page.set_title("Could not read library");
                ctx.empty_page.set_description(Some(&error.to_string()));
                ctx.content_stack.set_visible_child_name("empty");
                return;
            }
            Err(_) => return,
        };
        // One items-changed signal for the whole page instead of one per row.
        let items: Vec<BoxedAnyObject> = rows.iter().cloned().map(BoxedAnyObject::new).collect();
        // Keep the selection (and the pane) on the same file across a refresh.
        let selected_id = ctx.pane.current_id();
        ctx.restoring.set(true);
        ctx.model.splice(0, ctx.model.n_items(), &items);
        let position = selected_id.and_then(|id| rows.iter().position(|row| row.id == id));
        ctx.selection
            .set_selected(position.map_or(gtk::INVALID_LIST_POSITION, |position| position as u32));
        ctx.restoring.set(false);
        if position.is_none() {
            ctx.pane.hide();
        }
        ctx.total.set(total);
        ctx.has_more.set(total > rows.len() as i64);
        if total > rows.len() as i64 {
            ctx.count_label
                .set_text(&format!("{} of {} files", rows.len(), total));
        } else {
            ctx.count_label.set_text(&format!("{} files", rows.len()));
        }
        if rows.is_empty() {
            if filtered {
                ctx.empty_page.set_icon_name(Some("edit-find-symbolic"));
                ctx.empty_page.set_title("No matches");
                let description = if searched.is_empty() {
                    "Nothing of this kind in the library yet — try another filter.".to_string()
                } else {
                    format!("Nothing matches “{searched}” — try a different search or filter.")
                };
                ctx.empty_page.set_description(Some(&description));
            } else {
                ctx.empty_page.set_icon_name(Some("view-grid-symbolic"));
                ctx.empty_page.set_title("No files yet");
                ctx.empty_page.set_description(Some(
                    "Choose a folder above and select Start scan. First install the two required scan models in Settings; downloads begin only when you press Install. Already indexed files can be searched here by name, tags, and text.",
                ));
            }
            ctx.content_stack.set_visible_child_name("empty");
        } else {
            ctx.content_stack.set_visible_child_name("grid");
        }
    });
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

/// Resolve the fixed tile-widget structure built by `build_tile`:
/// `(picture, play-badge, name, caption)`.
fn tile_parts(vbox: &gtk::Box) -> Option<(gtk::Picture, gtk::Image, gtk::Label, gtk::Label)> {
    let overlay = vbox.first_child()?.downcast::<gtk::Overlay>().ok()?;
    let pic = overlay.child()?.downcast::<gtk::Picture>().ok()?;
    let play = pic.next_sibling()?.downcast::<gtk::Image>().ok()?;
    let name = overlay.next_sibling()?.downcast::<gtk::Label>().ok()?;
    let caption = name.next_sibling()?.downcast::<gtk::Label>().ok()?;
    Some((pic, play, name, caption))
}

const THUMB_CACHE_CAP: usize = 512;

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
    )> = RefCell::new((HashMap::new(), VecDeque::new()));
}

fn thumb_cache_get(key: &ThumbCacheKey) -> Option<gtk::gdk::Texture> {
    THUMB_CACHE.with(|cache| cache.borrow().0.get(key).cloned())
}

fn thumb_cache_put(key: ThumbCacheKey, texture: gtk::gdk::Texture) {
    THUMB_CACHE.with(|cache| {
        let (map, order) = &mut *cache.borrow_mut();
        if map.insert(key.clone(), texture).is_none() {
            order.push_back(key);
            while order.len() > THUMB_CACHE_CAP {
                if let Some(evict) = order.pop_front() {
                    map.remove(&evict);
                }
            }
        }
    });
}

fn bind_tile(engine: &Rc<RefCell<EngineClient>>, list_item: &gtk::ListItem) {
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
    let Some((pic, play, name, caption)) = tile_parts(&vbox) else {
        return;
    };

    // Filename — the Deep Analyze smart-name in gold when present (macOS parity).
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
        format_bytes(row.size_bytes)
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
    let rx = request
        .engine
        .borrow()
        .request_tile(request.key.path.clone(), TILE_THUMB_PX, request.is_video, cancel);
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
    if let Some((pic, play, _, _)) = tile_parts(vbox) {
        pic.set_paintable(None::<&gtk::gdk::Texture>);
        play.set_visible(false);
    }
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
    let next_btn = gtk::Button::builder()
        .icon_name("go-next-symbolic")
        .css_classes(["flat"])
        .tooltip_text("Next (→)")
        .build();
    let counter = gtk::Label::builder().css_classes(["dim-label", "numeric"]).build();
    header.pack_start(&prev_btn);
    header.pack_start(&counter);
    header.pack_end(&next_btn);
    let open_btn = gtk::Button::builder()
        .icon_name("document-open-symbolic")
        .css_classes(["flat"])
        .tooltip_text("Open in the default app")
        .build();
    let reveal_btn = gtk::Button::builder()
        .icon_name("folder-open-symbolic")
        .css_classes(["flat"])
        .tooltip_text("Show in Files")
        .build();
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
                open_default(button, &row.path);
            }
        }
    ));
    reveal_btn.connect_clicked(clone!(
        #[strong]
        current,
        move |button| {
            if let Some(row) = current() {
                show_in_files(button, &row.path);
            }
        }
    ));

    let update_nav: Rc<dyn Fn()> = {
        let rows = rows.clone();
        let idx = idx.clone();
        let counter = counter.clone();
        let prev_btn = prev_btn.clone();
        let next_btn = next_btn.clone();
        Rc::new(move || {
            let (pos, len) = (idx.get(), rows.borrow().len());
            counter.set_label(&format!("{} of {}", pos + 1, len));
            prev_btn.set_sensitive(pos > 0);
            next_btn.set_sensitive(pos + 1 < len);
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
        Rc::new(move |delta| {
            let len = rows.borrow().len() as i64;
            let next = idx.get() as i64 + delta;
            if (0..len).contains(&next) {
                idx.set(next as usize);
                load();
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

    let full = engine.borrow().query_files(QuerySpec {
        search: query.0,
        kind: query.1,
        limit: QUERY_MAX_ROWS,
        offset: 0,
        channel: QueryChannel::Preview,
    });
    glib::MainContext::default().spawn_local(clone!(
        #[strong]
        rows,
        #[strong]
        idx,
        #[strong]
        update_nav,
        async move {
            let Ok(Ok((library, _))) = full.recv().await else {
                return;
            };
            let current_id = rows.borrow().get(idx.get()).map(|row| row.id);
            let Some(position) = current_id.and_then(|id| library.iter().position(|row| row.id == id)) else {
                return;
            };
            if library.len() > rows.borrow().len() {
                rows.replace(library);
                idx.set(position);
                update_nav();
            }
        }
    ));

    dialog.connect_closed(clone!(
        #[weak]
        video,
        move |_| {
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

/// Resolve the `FileRow` at `pos` in the (selection) model.
fn row_at(model: &impl IsA<gio::ListModel>, pos: u32) -> Option<FileRow> {
    let boxed = model.item(pos)?.downcast::<BoxedAnyObject>().ok()?;
    let r = boxed.borrow::<FileRow>();
    Some(r.clone())
}

fn snapshot_rows(model: &impl IsA<gio::ListModel>) -> Vec<FileRow> {
    (0..model.n_items()).filter_map(|pos| row_at(model, pos)).collect()
}

pub(super) fn open_default(widget: &impl IsA<gtk::Widget>, path: &str) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    let window = widget.root().and_downcast::<gtk::Window>();
    launcher.launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
}

/// Show the file selected in the file manager (FileManager1.ShowItems through
/// the portal-aware launcher), falling back to opening its folder.
pub(super) fn show_in_files(widget: &impl IsA<gtk::Widget>, path: &str) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    let window = widget.root().and_downcast::<gtk::Window>();
    let folder = std::path::Path::new(path).parent().map(gio::File::for_path);
    let fallback_window = window.clone();
    launcher.open_containing_folder(window.as_ref(), gio::Cancellable::NONE, move |result| {
        if result.is_err() {
            if let Some(folder) = folder {
                gtk::FileLauncher::new(Some(&folder)).launch(
                    fallback_window.as_ref(),
                    gio::Cancellable::NONE,
                    |_| {},
                );
            }
        }
    });
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
    meta.append(&meta_row("Size", &format_bytes(row.size_bytes)));
    if let Some(d) = fmt_date(row.modified_at) {
        meta.append(&meta_row("Modified", &d));
    }
    if let Some(d) = fmt_date(row.created_at) {
        meta.append(&meta_row("Created", &d));
    }
    if row.has_faces {
        meta.append(&meta_row("Faces", "Detected"));
    }
    if row.has_text {
        meta.append(&meta_row("Text", "Detected (OCR)"));
    }
    if let Some(desc) = row.description.as_ref().filter(|s| !s.is_empty()) {
        meta.append(&meta_row("Caption", desc));
    }
    if let Some(p) = row.proposed_name.as_ref().filter(|s| !s.is_empty()) {
        meta.append(&meta_row("Smart name", &format!("{}.{}", p, row.extension)));
    }
    super::library_pane::append_details(&meta, row.id);
    meta
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
            .request_preview(row.path.clone(), PREVIEW_PX, row.kind == "video");
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
    fn live_reload_keeps_loaded_pages_until_the_query_changes() {
        assert_eq!(reload_limit(true, 0), QUERY_LIMIT);
        assert_eq!(reload_limit(true, 1800), 1800);
        assert_eq!(reload_limit(true, 9000), LIVE_RELOAD_MAX);
        assert_eq!(reload_limit(false, 1800), QUERY_LIMIT);
    }
}
