// Library tab — DB-backed thumbnail grid + FTS search + preview, the 1:1 port
// of macOS `LibraryView.swift`.
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

use super::util::{fmt_date, format_bytes, icon_for_kind, icon_paintable};
use crate::engine_client::{texture_from_decoded, EngineClient, EngineEvent, FileRow, QuerySpec};

const QUERY_LIMIT: i64 = 1000;
const TILE_THUMB_PX: i32 = 256;
const PREVIEW_PX: i32 = 1280;

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
            if let Some(tile) = li.child() {
                clear_tile(&tile);
            }
        }
    });

    let grid = gtk::GridView::new(Some(selection), Some(factory));
    grid.set_min_columns(1);
    grid.set_max_columns(12);
    grid.set_enable_rubberband(false);
    grid.add_css_class("fileid-tab");

    grid.connect_activate(clone!(
        #[strong]
        engine,
        move |gv, pos| {
            if let Some(model) = gv.model() {
                open_preview(&engine, gv, model, pos);
            }
        }
    ));

    let scroller = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .child(&grid)
        .build();
    scroller.add_css_class("fileid-tab");

    // ── Header (title + count + search + pills) ──────────────────────────────
    let title = gtk::Label::builder()
        .label("Library")
        .xalign(0.0)
        .css_classes(["title-1"])
        .build();
    let count_label = gtk::Label::builder()
        .label("0 files")
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    let title_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .build();
    title_row.append(&title);
    title_row.append(&count_label);

    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search filenames, tags, text in images…")
        .hexpand(true)
        .css_classes(["fileid-search"])
        .build();

    // Reusable reload closure shared by search + pills + scan events.
    let reload: Rc<dyn Fn()> = {
        let engine = engine.clone();
        let model = model.clone();
        let count_label = count_label.clone();
        let search_text = search_text.clone();
        let kind_filter = kind_filter.clone();
        let query_gen = query_gen.clone();
        Rc::new(move || {
            run_reload(
                &engine,
                &model,
                &count_label,
                &search_text,
                &kind_filter,
                &query_gen,
            );
        })
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
                    EngineEvent::BatchLanded(_) => {
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
    root.append(&scroller);
    root.upcast()
}

// ── Reload ───────────────────────────────────────────────────────────────────

fn run_reload(
    engine: &Rc<RefCell<EngineClient>>,
    model: &gio::ListStore,
    count_label: &gtk::Label,
    search_text: &Rc<RefCell<String>>,
    kind_filter: &Rc<RefCell<Option<String>>>,
    query_gen: &Rc<Cell<u64>>,
) {
    let g = query_gen.get().wrapping_add(1);
    query_gen.set(g);
    let spec = QuerySpec {
        search: search_text.borrow().clone(),
        kind: kind_filter.borrow().clone(),
        limit: QUERY_LIMIT,
    };
    let rx = engine.borrow().query_files(spec);
    let model = model.clone();
    let count_label = count_label.clone();
    let query_gen = query_gen.clone();
    glib::MainContext::default().spawn_local(async move {
        let rows = rx.recv().await.unwrap_or_default();
        // Latest-wins: a slower earlier query can't clobber a newer one.
        if query_gen.get() != g {
            return;
        }
        model.remove_all();
        for row in &rows {
            model.append(&BoxedAnyObject::new(row.clone()));
        }
        count_label.set_text(&format!("{} files", rows.len()));
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
    let name = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .single_line_mode(true)
        .build();
    let caption = gtk::Label::builder()
        .xalign(0.0)
        .css_classes(["tile-caption"])
        .build();
    vbox.append(&pic);
    vbox.append(&name);
    vbox.append(&caption);
    vbox
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
    let Some(pic) = vbox
        .first_child()
        .and_then(|w| w.downcast::<gtk::Picture>().ok())
    else {
        return;
    };
    let Some(name) = pic
        .next_sibling()
        .and_then(|w| w.downcast::<gtk::Label>().ok())
    else {
        return;
    };
    let Some(caption) = name
        .next_sibling()
        .and_then(|w| w.downcast::<gtk::Label>().ok())
    else {
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

    if row.kind == "image" {
        let key = ThumbCacheKey::for_row(&row);
        if let Some(tex) = thumb_cache_get(&key) {
            pic.set_paintable(Some(&tex));
            return;
        }
        pic.set_paintable(None::<&gtk::gdk::Texture>);
        let want = key.clone();
        let rx = engine
            .borrow()
            .request_scaled_thumbnail(key.path.clone(), TILE_THUMB_PX);
        let pic_weak = pic.downgrade();
        let li_weak = list_item.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let Ok(Some(decoded)) = rx.recv().await else {
                return;
            };
            let tex: gtk::gdk::Texture = texture_from_decoded(&decoded).upcast();
            thumb_cache_put(want.clone(), tex.clone());
            // Re-check: the tile may have been recycled while we were decoding.
            if !tile_still_wants(&li_weak, &want) {
                return;
            }
            if let Some(pic) = pic_weak.upgrade() {
                pic.set_paintable(Some(&tex));
            }
        });
    } else {
        pic.set_paintable(icon_paintable(icon_for_kind(&row.kind), 96).as_ref());
    }
}

fn clear_tile(tile: &gtk::Widget) {
    if let Some(pic) = tile
        .first_child()
        .and_then(|w| w.downcast::<gtk::Picture>().ok())
    {
        pic.set_paintable(None::<&gtk::gdk::Texture>);
    }
}

// ── Preview dialog ───────────────────────────────────────────────────────────

/// The file preview dialog. Shows a large image + metadata and lets you page
/// through the whole (filtered) library with the ‹ / › buttons or the ←/→ keys
/// — matching the macOS/Windows preview navigation.
fn open_preview(
    engine: &Rc<RefCell<EngineClient>>,
    parent: &impl IsA<gtk::Widget>,
    model: gtk::SelectionModel,
    start: u32,
) {
    let n = model.n_items();
    if n == 0 {
        return;
    }
    let idx = Rc::new(Cell::new(start.min(n - 1)));
    let preview_generation = Rc::new(Cell::new(0u64));

    let dialog = adw::Dialog::new();
    dialog.set_content_width(1000);
    dialog.set_content_height(680);

    // Header: ‹ prev · counter · next › — the navigation the preview was missing.
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
    let counter = gtk::Label::builder().css_classes(["dim-label"]).build();
    header.pack_start(&prev_btn);
    header.pack_start(&counter);
    header.pack_end(&next_btn);

    let pic = gtk::Picture::builder()
        .hexpand(true)
        .vexpand(true)
        .content_fit(gtk::ContentFit::Contain)
        .build();
    let meta_scroll = gtk::ScrolledWindow::builder()
        .width_request(320)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();

    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .build();
    body.append(&pic);
    body.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    body.append(&meta_scroll);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&body));
    dialog.set_child(Some(&toolbar));

    // Load the row at the current index into the dialog (title, image, metadata).
    let load: Rc<dyn Fn()> = {
        let engine = engine.clone();
        let model = model.clone();
        let pic = pic.clone();
        let meta_scroll = meta_scroll.clone();
        let counter = counter.clone();
        let dialog = dialog.clone();
        let prev_btn = prev_btn.clone();
        let next_btn = next_btn.clone();
        let idx = idx.clone();
        let preview_generation = preview_generation.clone();
        Rc::new(move || {
            let pos = idx.get();
            let Some(row) = row_at(&model, pos) else {
                return;
            };
            dialog.set_title(&row.name);
            counter.set_label(&format!("{} of {}", pos + 1, model.n_items()));
            prev_btn.set_sensitive(pos > 0);
            next_btn.set_sensitive(pos + 1 < model.n_items());
            meta_scroll.set_child(Some(&build_meta(&row)));
            pic.set_paintable(None::<&gtk::gdk::Texture>);
            let generation = preview_generation.get().wrapping_add(1);
            preview_generation.set(generation);
            load_preview_image(&engine, &pic, &row, preview_generation.clone(), generation);
        })
    };

    prev_btn.connect_clicked(clone!(
        #[strong]
        idx,
        #[strong]
        load,
        move |_| {
            if idx.get() > 0 {
                idx.set(idx.get() - 1);
                load();
            }
        }
    ));
    next_btn.connect_clicked(clone!(
        #[strong]
        idx,
        #[strong]
        model,
        #[strong]
        load,
        move |_| {
            if idx.get() + 1 < model.n_items() {
                idx.set(idx.get() + 1);
                load();
            }
        }
    ));

    // ←/→ arrow keys page through the library.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(clone!(
        #[strong]
        idx,
        #[strong]
        model,
        #[strong]
        load,
        move |_, key, _, _| {
            match key {
                gtk::gdk::Key::Left | gtk::gdk::Key::Up => {
                    if idx.get() > 0 {
                        idx.set(idx.get() - 1);
                        load();
                    }
                    glib::Propagation::Stop
                }
                gtk::gdk::Key::Right | gtk::gdk::Key::Down => {
                    if idx.get() + 1 < model.n_items() {
                        idx.set(idx.get() + 1);
                        load();
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    ));
    dialog.add_controller(keys);

    load();
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
fn row_at(model: &gtk::SelectionModel, pos: u32) -> Option<FileRow> {
    let boxed = model.item(pos)?.downcast::<BoxedAnyObject>().ok()?;
    let r = boxed.borrow::<FileRow>();
    Some(r.clone())
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
    if row.kind == "image" {
        let rx = engine
            .borrow()
            .request_scaled_thumbnail(row.path.clone(), PREVIEW_PX);
        let pic_weak = pic.downgrade();
        glib::MainContext::default().spawn_local(async move {
            let Ok(Some(decoded)) = rx.recv().await else {
                return;
            };
            if active_generation.get() != generation {
                return;
            }
            if let Some(pic) = pic_weak.upgrade() {
                pic.set_paintable(Some(&texture_from_decoded(&decoded)));
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
