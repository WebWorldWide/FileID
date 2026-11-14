// Settings cards that sit around the model rows: Appearance, Organize Folders,
// Folders not to describe, and the Advanced troubleshooting section. Ports of
// the macOS `AppearanceCard`, `DeepAnalyzeExclusionsCard` and Advanced group.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::thread;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;

use crate::app_settings;
use crate::engine_client::{EngineClient, EngineState};
use crate::v3;

fn card(title: &str) -> gtk::Box {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["fileid-glass"])
        .build();
    card.append(
        &gtk::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["heading"])
            .build(),
    );
    card
}

fn note(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .css_classes(["dim-label"])
        .build()
}

fn keep_alive(segmented: &Rc<v3::Segmented>) {
    let keep = segmented.clone();
    segmented.root.connect_destroy(move |_| {
        let _ = &keep;
    });
}

/// A title and caption on the left, a segmented control on the right.
fn segment_row(
    title: &str,
    caption: &str,
    options: &[&str],
    selected: usize,
) -> (gtk::Box, Rc<v3::Segmented>) {
    let row = v3::hbox(12);
    let text = v3::vbox(2);
    text.set_hexpand(true);
    text.append(&v3::label(title, &["v3-body"]));
    text.append(&v3::wrapped(caption, &["dim-label"]));
    row.append(&text);
    let segmented = v3::Segmented::new(options, selected);
    segmented.root.set_valign(gtk::Align::Center);
    segmented
        .root
        .update_property(&[gtk::accessible::Property::Label(title)]);
    keep_alive(&segmented);
    row.append(&segmented.root);
    (row, segmented)
}

fn index_of(ids: &[&str], id: &str) -> usize {
    ids.iter().position(|known| *known == id).unwrap_or(0)
}

pub fn appearance() -> gtk::Widget {
    let card = card("Appearance");
    let (lava_row, lava) = segment_row(
        "Lava lamp",
        "Wax glows rise behind the panes. Reduce Motion pauses it.",
        &["Full", "Off"],
        usize::from(!app_settings::lava_lamp_on()),
    );
    lava.connect_changed(|index| {
        let on = index == 0;
        app_settings::remember_lava_lamp(on);
        crate::lavalamp::set_enabled(on);
    });
    let lava_for_map = lava.clone();
    card.connect_map(move |_| {
        lava_for_map.select(usize::from(!app_settings::lava_lamp_on()), false)
    });
    card.append(&lava_row);
    let motion = note("");
    let refresh = {
        let motion = motion.clone();
        move || {
            motion.set_label(if crate::lavalamp::reduce_motion() {
                "Reduce Motion is on (animations are off in your desktop settings), so the lava lamp is paused."
            } else {
                "Reduce Motion is off. FileID follows your desktop's animation setting."
            });
        }
    };
    refresh();
    if let Some(settings) = gtk::Settings::default() {
        settings.connect_gtk_enable_animations_notify(move |_| refresh());
    }
    card.append(&motion);

    let labels: Vec<&str> = crate::theme::TEXT_SIZES
        .iter()
        .map(|(_, label, _)| *label)
        .collect();
    let ids: Vec<&str> = crate::theme::TEXT_SIZES
        .iter()
        .map(|(id, _, _)| *id)
        .collect();
    let (size_row, size) = segment_row(
        "Text size",
        "File names, details, descriptions and chat. The title bar and sidebar stay the same size.",
        &labels,
        index_of(&ids, &app_settings::text_size()),
    );
    size.connect_changed(move |index| {
        let id = ids[index];
        app_settings::remember_text_size(id);
        crate::theme::apply_text_size(id);
    });
    card.append(&size_row);
    card.upcast()
}

pub fn organize_folders() -> gtk::Widget {
    let card = card("Organize Folders");
    let ids = app_settings::GRANULARITY_IDS;
    let (row, granularity) = segment_row(
        "Folder granularity",
        "",
        &["Looser", "Balanced", "Tighter"],
        index_of(&ids, &app_settings::restructure_granularity()),
    );
    granularity
        .connect_changed(move |index| app_settings::remember_restructure_granularity(ids[index]));
    card.append(&row);
    card.append(&note("How finely Organize Folders splits your files. Looser makes fewer, broader folders; Tighter makes more, smaller ones. Takes effect after you quit and reopen FileID."));
    card.upcast()
}

fn abbreviated(path: &str) -> String {
    let home = std::env::var("HOME").ok();
    super::locations::abbreviated(path, home.as_deref())
}

pub fn excluded_folders() -> gtk::Widget {
    let card = card("Folders not to describe");
    card.append(&note("Describe Files skips these folders when it describes the whole library. Their files still show up and can be searched. Describing a single file ignores this list."));
    let message = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .visible(false)
        .build();
    card.append(&message);
    let list = v3::vbox(4);
    card.append(&list);
    let add = v3::secondary("Add folder…", false);
    add.set_halign(gtk::Align::Start);
    card.append(&add);

    let render: Rc<RefCell<Option<Rc<dyn Fn()>>>> = Rc::new(RefCell::new(None));
    let draw: Rc<dyn Fn()> = {
        let list = list.clone();
        let render = render.clone();
        Rc::new(move || {
            v3::clear(&list);
            let folders = app_settings::deep_analyze_excluded_folders();
            if folders.is_empty() {
                list.append(&v3::label("No folders are skipped.", &["dim-label"]));
            }
            for folder in folders {
                let row = v3::hbox(8);
                let label = v3::label(&abbreviated(&folder), &["monospace"]);
                label.set_hexpand(true);
                label.set_xalign(0.0);
                label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                label.set_tooltip_text(Some(&folder));
                row.append(&label);
                let remove =
                    v3::icon_button("window-close-symbolic", &format!("Describe {folder} again"));
                let render = render.clone();
                remove.connect_clicked(move |_| {
                    let mut folders = app_settings::deep_analyze_excluded_folders();
                    folders.retain(|known| known != &folder);
                    app_settings::remember_deep_analyze_excluded_folders(&folders);
                    if let Some(render) = render.borrow().as_ref() {
                        render();
                    }
                });
                row.append(&remove);
                list.append(&row);
            }
        })
    };
    *render.borrow_mut() = Some(draw.clone());
    draw();
    // The closure above holds `render`, which holds the closure; break the
    // cycle when the card goes away.
    let render_for_destroy = render.clone();
    card.connect_destroy(move |_| {
        render_for_destroy.borrow_mut().take();
    });

    add.connect_clicked(move |button| {
        let dialog = gtk::FileDialog::builder()
            .title("Choose a folder for Describe Files to skip")
            .accept_label("Exclude")
            .modal(true)
            .build();
        let parent = button.root().and_downcast::<gtk::Window>();
        let (message, draw) = (message.clone(), draw.clone());
        dialog.select_folder(
            parent.as_ref(),
            gtk::gio::Cancellable::NONE,
            move |result| {
                let Some(path) = result.ok().and_then(|folder| folder.path()) else {
                    return;
                };
                let (text, is_error) = add_excluded_folder(&path);
                message.set_label(text);
                message.set_visible(true);
                message.remove_css_class("tone-danger");
                message.remove_css_class("dim-label");
                message.add_css_class(if is_error { "tone-danger" } else { "dim-label" });
                draw();
            },
        );
    });
    card.upcast()
}

/// Returns the message to show and whether it is an error.
fn add_excluded_folder(path: &Path) -> (&'static str, bool) {
    let text = path.to_string_lossy().into_owned();
    if !path.is_absolute() || !path.is_dir() {
        return (
            "Couldn't skip that folder. Choose a folder on this computer.",
            true,
        );
    }
    let mut folders = app_settings::deep_analyze_excluded_folders();
    let normalized = text.trim_end_matches('/').to_owned();
    if folders.iter().any(|known| known == &normalized) {
        return ("That folder is already skipped.", false);
    }
    folders.push(normalized);
    app_settings::remember_deep_analyze_excluded_folders(&folders);
    (
        "Done. Describe Files will skip this folder from the next run.",
        false,
    )
}

#[derive(Default)]
struct Overview {
    total_files: i64,
    images: i64,
    sessions: Vec<Session>,
}

struct Session {
    root: String,
    started: f64,
    status: String,
    total: Option<i64>,
}

fn read_overview(conn: &rusqlite::Connection) -> Overview {
    let count = |sql: &str| {
        conn.query_row(sql, [], |row| row.get::<_, i64>(0))
            .unwrap_or(0)
    };
    let mut overview = Overview {
        total_files: count("SELECT COUNT(*) FROM files WHERE failed = 0"),
        images: count("SELECT COUNT(*) FROM files WHERE failed = 0 AND kind = 'image'"),
        sessions: Vec::new(),
    };
    if let Ok(mut statement) = conn.prepare(
        "SELECT COALESCE(root_path, ''), started_at, COALESCE(status, ''), total_files \
         FROM scan_sessions ORDER BY started_at DESC LIMIT 5",
    ) && let Ok(rows) = statement.query_map([], |row| {
        Ok(Session {
            root: row.get(0)?,
            started: row.get(1)?,
            status: row.get(2)?,
            total: row.get(3)?,
        })
    }) {
        overview.sessions = rows.flatten().collect();
    }
    overview
}

/// Total size of the regular files under `path`; symlinks are not followed.
fn tree_bytes(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| tree_bytes(&entry.path()))
                .sum()
        })
        .unwrap_or(0)
}

fn storage_sizes() -> Vec<(&'static str, String, u64)> {
    use fileid_engine::paths;
    let mut rows = Vec::new();
    if let Ok(db) = paths::db_path() {
        let bytes = ["", "-wal", "-shm"]
            .iter()
            .map(|suffix| {
                let mut name = db.clone().into_os_string();
                name.push(suffix);
                tree_bytes(Path::new(&name))
            })
            .sum();
        rows.push(("Database", db.to_string_lossy().into_owned(), bytes));
    }
    for (label, path) in [
        ("Models", paths::models_dir()),
        ("Thumbnails", paths::thumbs_dir()),
        ("Face crops", paths::faces_dir()),
        ("Logs", paths::logs_dir()),
    ] {
        if let Ok(path) = path {
            let bytes = tree_bytes(&path);
            rows.push((label, path.to_string_lossy().into_owned(), bytes));
        }
    }
    rows
}

fn when(started: f64) -> String {
    glib::DateTime::from_unix_local(started as i64)
        .ok()
        .and_then(|time| time.format("%Y-%m-%d %H:%M").ok())
        .map_or_else(|| "unknown time".to_owned(), |text| text.to_string())
}

fn kv_row(key: &str, value: &str) -> gtk::Box {
    let row = v3::hbox(12);
    let key = v3::label(key, &["dim-label"]);
    key.set_width_request(150);
    key.set_xalign(0.0);
    key.set_valign(gtk::Align::Start);
    row.append(&key);
    let value = v3::label(value, &["monospace"]);
    value.set_xalign(0.0);
    value.set_hexpand(true);
    value.set_wrap(true);
    value.set_selectable(true);
    row.append(&value);
    row
}

fn heading(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .css_classes(["heading"])
        .build()
}

fn engine_label(state: &EngineState) -> Option<String> {
    Some(match state {
        EngineState::Spawning => "Starting…".to_owned(),
        EngineState::Ready => "Ready".to_owned(),
        EngineState::Failed(why) => format!("Crashed — {why}"),
        EngineState::Exited => "Stopped".to_owned(),
        _ => return None,
    })
}

pub fn advanced(engine: &Rc<RefCell<EngineClient>>) -> gtk::Widget {
    let expander = gtk::Expander::builder()
        .label("Advanced (troubleshooting: background helper, storage, scan history, logs)")
        .build();
    let body = v3::vbox(14);
    body.set_margin_top(10);
    expander.set_child(Some(&body));

    // Engine
    body.append(&heading("Engine"));
    let status_row = kv_row(
        "Status",
        if engine.borrow().is_ready() {
            "Ready"
        } else {
            "Starting…"
        },
    );
    let status_value = status_row.last_child().and_downcast::<gtk::Label>();
    body.append(&status_row);
    let buttons = v3::hbox(8);
    let restart = v3::secondary("Restart Engine", false);
    restart.set_tooltip_text(Some(
        "Restart FileID's background helper. Stops any scan in progress.",
    ));
    let stop = v3::secondary("Stop Engine", false);
    stop.set_tooltip_text(Some("Stop FileID's background helper."));
    stop.set_sensitive(engine.borrow().is_ready());
    buttons.append(&restart);
    buttons.append(&stop);
    body.append(&buttons);
    {
        let engine = engine.clone();
        restart.connect_clicked(move |_| EngineClient::restart(&engine));
    }
    {
        let engine = engine.clone();
        stop.connect_clicked(move |button| {
            engine.borrow_mut().stop_engine();
            button.set_sensitive(false);
        });
    }
    let rx = engine.borrow_mut().subscribe();
    {
        let stop = stop.clone();
        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = rx.recv().await {
                if let (Some(label), Some(value)) = (engine_label(&event), status_value.as_ref()) {
                    value.set_label(&label);
                    stop.set_sensitive(matches!(*event, EngineState::Ready));
                }
            }
        });
    }

    // Storage
    body.append(&heading("Storage"));
    let storage = v3::vbox(6);
    storage.append(&v3::label("Reading sizes…", &["dim-label"]));
    body.append(&storage);

    // Recent scans
    body.append(&heading("Recent scans"));
    let scans = v3::vbox(6);
    scans.append(&v3::label("No scans recorded yet.", &["dim-label"]));
    body.append(&scans);

    let refresh = {
        let (storage, scans) = (storage.clone(), scans.clone());
        Rc::new(move || {
            let overview = crate::engine_client::read_db(read_overview);
            let (tx, sizes) = async_channel::bounded(1);
            thread::spawn(move || {
                let _ = tx.send_blocking(storage_sizes());
            });
            let (storage, scans) = (storage.clone(), scans.clone());
            glib::MainContext::default().spawn_local(async move {
                let overview = overview.recv().await.unwrap_or_default();
                let sizes = sizes.recv().await.unwrap_or_default();
                v3::clear(&storage);
                storage.append(&kv_row("Total files", &overview.total_files.to_string()));
                storage.append(&kv_row("Images", &overview.images.to_string()));
                for (label, path, bytes) in sizes {
                    storage.append(&kv_row(
                        label,
                        &format!(
                            "{} · {path}",
                            fileid_engine::util::display::file_size(bytes)
                        ),
                    ));
                }
                v3::clear(&scans);
                if overview.sessions.is_empty() {
                    scans.append(&v3::label("No scans recorded yet.", &["dim-label"]));
                }
                for session in overview.sessions {
                    let row = v3::vbox(2);
                    let root = v3::label(&abbreviated(&session.root), &["monospace"]);
                    root.set_xalign(0.0);
                    root.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                    root.set_tooltip_text(Some(&session.root));
                    row.append(&root);
                    let mut detail = format!("{} · {}", when(session.started), session.status);
                    if let Some(total) = session.total {
                        detail.push_str(&format!(" · {total} files"));
                    }
                    row.append(&v3::label(&detail, &["meta"]));
                    scans.append(&row);
                }
            });
        })
    };
    {
        let refresh = refresh.clone();
        expander.connect_expanded_notify(move |expander| {
            if expander.is_expanded() {
                refresh();
            }
        });
    }

    // Logs
    body.append(&heading("Logs"));
    body.append(&note("Detailed scan and app logs for troubleshooting."));
    let open_logs = v3::secondary("Open logs folder", false);
    open_logs.set_halign(gtk::Align::Start);
    open_logs.connect_clicked(|button| {
        if let Ok(dir) = fileid_engine::paths::logs_dir() {
            let _ = std::fs::create_dir_all(&dir);
            v3::open_file(button, &dir.to_string_lossy());
        }
    });
    body.append(&open_logs);

    // Danger zone
    let danger = gtk::Label::builder()
        .label("Danger Zone")
        .xalign(0.0)
        .css_classes(["heading", "tone-danger"])
        .build();
    body.append(&danger);
    body.append(&note(
        "Permanently erase FileID's library, settings and caches.",
    ));
    let reset = v3::danger("Factory Reset & Quit");
    reset.set_halign(gtk::Align::Start);
    let status = note("");
    status.set_visible(false);
    {
        let (engine, status) = (engine.clone(), status.clone());
        reset.connect_clicked(move |button| confirm_reset(button, &engine, &status));
    }
    body.append(&reset);
    body.append(&status);
    expander.upcast()
}

fn confirm_reset(anchor: &gtk::Button, engine: &Rc<RefCell<EngineClient>>, status: &gtk::Label) {
    let dialog = adw::AlertDialog::new(
        Some("Are you sure you want to completely erase FileID?"),
        Some(
            "This permanently deletes everything FileID learned (tags, faces, descriptions), its settings and caches. Downloaded models are kept. Your own files are not touched. This can't be undone.",
        ),
    );
    dialog.add_responses(&[("cancel", "Cancel"), ("erase", "Erase Everything and Quit")]);
    dialog.set_response_appearance("erase", adw::ResponseAppearance::Destructive);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    let (engine, status) = (engine.clone(), status.clone());
    dialog.connect_response(None, move |_, response| {
        if response == "erase" {
            run_reset(&engine, &status);
        }
    });
    dialog.present(Some(anchor));
}

fn run_reset(engine: &Rc<RefCell<EngineClient>>, status: &gtk::Label) {
    let Ok(root) = fileid_engine::paths::root() else {
        status.set_label("Couldn't find FileID's data folder. Nothing was erased.");
        status.set_visible(true);
        return;
    };
    let keep: Vec<PathBuf> = [
        fileid_engine::paths::models_dir(),
        fileid_engine::paths::engine_models_dir(),
        fileid_engine::paths::runtime_dir(),
    ]
    .into_iter()
    .flatten()
    .collect();
    let child = engine.borrow_mut().take_engine_for_reset();
    status.set_label("Erasing…");
    status.set_visible(true);
    let (tx, rx) = async_channel::bounded(1);
    thread::spawn(move || {
        if let Some(mut child) = child {
            let deadline = Instant::now() + Duration::from_secs(20);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = tx.send_blocking(erase_state(&root, &keep));
    });
    let status = status.clone();
    glib::MainContext::default().spawn_local(async move {
        match rx.recv().await {
            Ok(Ok(())) => std::process::exit(0),
            Ok(Err(error)) => {
                status.set_label(&format!(
                    "Couldn't erase everything: {error}. Quit and reopen FileID."
                ));
            }
            Err(_) => {}
        }
    });
}

/// Delete everything in FileID's data folder except the `keep` paths (and any
/// folder that contains one). Refuses a folder that isn't named `FileID`.
fn erase_state(root: &Path, keep: &[PathBuf]) -> std::io::Result<()> {
    if root.file_name().is_none_or(|name| name != "FileID") || root.components().count() < 3 {
        return Err(std::io::Error::other("unexpected data folder"));
    }
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if keep.iter().any(|kept| kept.starts_with(&path)) {
            continue;
        }
        if std::fs::symlink_metadata(&path)?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("fileid-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn erase_keeps_models_and_removes_the_rest() {
        let root = scratch("erase").join("FileID");
        std::fs::create_dir_all(root.join("Models/ram_plus")).unwrap();
        std::fs::create_dir_all(root.join("thumbs.cache")).unwrap();
        std::fs::write(root.join("fileid.sqlite"), b"db").unwrap();
        std::fs::write(root.join("app-settings.json"), b"{}").unwrap();
        erase_state(&root, &[root.join("Models")]).unwrap();
        assert!(root.join("Models/ram_plus").is_dir());
        assert!(!root.join("thumbs.cache").exists());
        assert!(!root.join("fileid.sqlite").exists());
        assert!(!root.join("app-settings.json").exists());
        std::fs::remove_dir_all(root.parent().unwrap()).unwrap();
    }

    #[test]
    fn erase_refuses_other_folders() {
        let dir = scratch("refuse");
        std::fs::write(dir.join("keep.txt"), b"x").unwrap();
        assert!(erase_state(&dir, &[]).is_err());
        assert!(dir.join("keep.txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn tree_bytes_sums_nested_files() {
        let dir = scratch("sizes");
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/one"), [0u8; 10]).unwrap();
        std::fs::write(dir.join("a/b/two"), [0u8; 5]).unwrap();
        assert_eq!(tree_bytes(&dir), 15);
        assert_eq!(tree_bytes(&dir.join("missing")), 0);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
