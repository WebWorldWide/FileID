// Settings → Locations (macOS `LocationsCard`): every folder or drive the
// catalog has scanned and whether it is reachable now. An unplugged drive
// reads OFFLINE and its catalog rows are kept.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use fileid_engine::ipc::LibraryRoot;
use gtk::glib;

use super::workbenches::Engine;
use crate::engine_client::{read_db, EngineState};
use crate::shell::status::Tone;
use crate::v3;

pub(super) fn relative(seconds_ago: f64) -> String {
    let seconds = seconds_ago.max(0.0) as u64;
    let (count, unit) = match seconds {
        0..60 => return "scanned just now".to_owned(),
        60..3_600 => (seconds / 60, "minute"),
        3_600..86_400 => (seconds / 3_600, "hour"),
        86_400..604_800 => (seconds / 86_400, "day"),
        604_800..2_592_000 => (seconds / 604_800, "week"),
        2_592_000..31_536_000 => (seconds / 2_592_000, "month"),
        _ => (seconds / 31_536_000, "year"),
    };
    format!("scanned {count} {unit}{} ago", if count == 1 { "" } else { "s" })
}

pub(super) fn summary(root: &LibraryRoot, now: f64) -> String {
    let files = root.file_count.max(0) as u64;
    let scanned = root
        .last_scan_at
        .map_or_else(|| "not scanned yet".to_owned(), |at| relative(now - at));
    format!(
        "{} {} · {} · {scanned}",
        crate::shell::grouped(files),
        if files == 1 { "file" } else { "files" },
        fileid_engine::util::display::file_size_i64(root.total_bytes)
    )
}

pub(super) fn abbreviated(path: &str, home: Option<&str>) -> String {
    match home.filter(|home| !home.is_empty() && *home != "/") {
        Some(home) if path == home => "~".to_owned(),
        Some(home) => path
            .strip_prefix(home)
            .filter(|rest| rest.starts_with('/'))
            .map_or_else(|| path.to_owned(), |rest| format!("~{rest}")),
        None => path.to_owned(),
    }
}

fn icon(kind: &str) -> &'static str {
    match kind {
        "network" => "network-server-symbolic",
        "external" => "drive-removable-media-symbolic",
        _ => "folder-symbolic",
    }
}

struct Locations {
    engine: Engine,
    list: gtk::Box,
    status: gtk::Label,
    generation: Cell<u64>,
    request: RefCell<String>,
}

impl Locations {
    /// Asks the engine (`listRoots`); before it is ready, reads the catalog directly.
    fn refresh(self: &Rc<Self>) {
        let generation = self.generation.get().wrapping_add(1);
        self.generation.set(generation);
        let request_id = format!("locations-{generation}");
        let request = fileid_engine::ipc::ListRootsPayload { request_id: request_id.clone() };
        let sent = self.engine.borrow().is_ready()
            && self.engine.borrow_mut().send(fileid_engine::ipc::CommandPayload::ListRoots(request)).is_ok();
        if sent {
            *self.request.borrow_mut() = request_id;
            return;
        }
        self.request.borrow_mut().clear();
        let rx = read_db(|conn| fileid_engine::commands::roots::list(conn).ok());
        let weak = Rc::downgrade(self);
        glib::MainContext::default().spawn_local(async move {
            let Ok(roots) = rx.recv().await else { return };
            let Some(this) = weak.upgrade() else { return };
            if this.generation.get() == generation {
                this.render(roots);
            }
        });
    }

    fn render(self: &Rc<Self>, roots: Option<Vec<LibraryRoot>>) {
        v3::clear(&self.list);
        let Some(roots) = roots else {
            self.status.set_label("FileID couldn't read its locations. Press Refresh to try again.");
            self.status.set_visible(true);
            return;
        };
        self.status.set_visible(false);
        if roots.is_empty() {
            self.list.append(&v3::wrapped(
                "No locations yet. Scan a folder or drive and it appears here.",
                &["dim-label"],
            ));
            return;
        }
        let now = glib::real_time() as f64 / 1_000_000.0;
        let home = std::env::var("HOME").ok();
        for (index, root) in roots.iter().enumerate() {
            if index > 0 {
                self.list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            self.list.append(&self.row(root, now, home.as_deref()));
        }
        self.list.append(&v3::wrapped(
            "Offline locations keep their tags, faces and descriptions. They come back when the drive does.",
            &["dim-label"],
        ));
    }

    fn row(self: &Rc<Self>, root: &LibraryRoot, now: f64, home: Option<&str>) -> gtk::Box {
        let row = v3::hbox(12);
        let image = gtk::Image::from_icon_name(icon(&root.kind));
        image.set_valign(gtk::Align::Start);
        image.add_css_class("dim-label");
        row.append(&image);
        let text = v3::vbox(2);
        text.set_hexpand(true);
        let title = v3::hbox(8);
        let name = v3::label(&root.display, &["heading"]);
        name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        title.append(&name);
        if !root.online {
            title.append(&v3::pill("OFFLINE", Tone::Warning));
        }
        text.append(&title);
        let line = summary(root, now);
        text.append(&v3::label(&line, &["dim-label"]));
        let path = v3::label(&abbreviated(&root.path, home), &["dim-label"]);
        path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        path.set_tooltip_text(Some(&root.path));
        text.append(&path);
        row.append(&text);
        let rescan = v3::secondary("Rescan", false);
        rescan.set_valign(gtk::Align::Center);
        rescan.set_sensitive(root.online);
        rescan.set_tooltip_text(Some(if root.online {
            "Scan this location again"
        } else {
            "Reconnect the drive to rescan"
        }));
        let weak = Rc::downgrade(self);
        let target = root.path.clone();
        rescan.connect_clicked(move |_| {
            let Some(this) = weak.upgrade() else { return };
            let started = this.engine.borrow_mut().start_scan(&target);
            this.status.set_label(match started {
                Ok(()) => "Rescanning. New and changed files appear as the scan goes.",
                Err(_) => "FileID is still getting ready. Try again in a moment.",
            });
            this.status.set_visible(true);
        });
        row.append(&rescan);
        let spoken = format!(
            "{}, {}, {line}",
            root.display,
            if root.online { "online" } else { "offline" }
        );
        row.update_property(&[gtk::accessible::Property::Label(&spoken)]);
        row
    }
}

pub(super) fn build(engine: Engine) -> gtk::Widget {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .margin_top(16)
        .margin_bottom(16)
        .margin_start(16)
        .margin_end(16)
        .css_classes(["fileid-glass"])
        .build();
    let header = v3::hbox(8);
    let title = v3::label("Locations", &["heading"]);
    title.set_hexpand(true);
    header.append(&title);
    let refresh = v3::secondary("Refresh", false);
    refresh.update_property(&[gtk::accessible::Property::Label("Refresh locations")]);
    header.append(&refresh);
    card.append(&header);
    let status = v3::wrapped("", &["dim-label"]);
    status.set_visible(false);
    card.append(&status);
    let list = v3::vbox(8);
    card.append(&list);
    let this = Rc::new(Locations {
        engine: engine.clone(),
        list,
        status,
        generation: Cell::new(0),
        request: RefCell::new(String::new()),
    });
    let weak = Rc::downgrade(&this);
    refresh.connect_clicked(move |_| {
        if let Some(this) = weak.upgrade() {
            this.refresh();
        }
    });
    let weak = Rc::downgrade(&this);
    card.connect_map(move |_| {
        if let Some(this) = weak.upgrade() {
            this.refresh();
        }
    });
    let rx = engine.borrow_mut().subscribe();
    let weak = Rc::downgrade(&this);
    glib::MainContext::default().spawn_local(async move {
        while let Ok(event) = rx.recv().await {
            let Some(this) = weak.upgrade() else { break };
            let waiting = !this.request.borrow().is_empty();
            match event {
                EngineState::ScanComplete(_) if this.list.is_mapped() => this.refresh(),
                EngineState::RootsList(list) if waiting && list.request_id == *this.request.borrow() => {
                    this.request.borrow_mut().clear();
                    this.render(Some(list.roots));
                }
                EngineState::Error { kind, .. }
                    if waiting && (kind == "list_roots_failed" || kind == "invalid_list_roots_request") =>
                {
                    this.request.borrow_mut().clear();
                    this.render(None);
                }
                _ => {}
            }
        }
    });
    card.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(count: i64, bytes: i64, last_scan_at: Option<f64>) -> LibraryRoot {
        LibraryRoot {
            id: 1,
            path: "/media/a/Camera".into(),
            display: "Camera".into(),
            kind: "external".into(),
            enabled: true,
            online: false,
            file_count: count,
            total_bytes: bytes,
            last_scan_at,
            last_status: None,
        }
    }

    #[test]
    fn relative_scan_times_read_like_macos() {
        assert_eq!(relative(5.0), "scanned just now");
        assert_eq!(relative(-30.0), "scanned just now");
        assert_eq!(relative(60.0), "scanned 1 minute ago");
        assert_eq!(relative(7_200.0), "scanned 2 hours ago");
        assert_eq!(relative(3.0 * 86_400.0), "scanned 3 days ago");
        assert_eq!(relative(14.0 * 86_400.0), "scanned 2 weeks ago");
        assert_eq!(relative(400.0 * 86_400.0), "scanned 1 year ago");
    }

    #[test]
    fn summaries_count_files_size_and_last_scan() {
        let never = summary(&root(1, 0, None), 1_000.0);
        assert!(never.starts_with("1 file · "), "{never}");
        assert!(never.ends_with(" · not scanned yet"), "{never}");
        let scanned = summary(&root(12_345, 2_000_000, Some(1_000.0)), 1_000.0 + 3_600.0);
        assert!(scanned.starts_with("12,345 files · "), "{scanned}");
        assert!(scanned.ends_with(" · scanned 1 hour ago"), "{scanned}");
    }

    #[test]
    fn paths_under_home_are_abbreviated() {
        assert_eq!(abbreviated("/home/ada/Pictures", Some("/home/ada")), "~/Pictures");
        assert_eq!(abbreviated("/home/ada", Some("/home/ada")), "~");
        assert_eq!(abbreviated("/home/adam/Pictures", Some("/home/ada")), "/home/adam/Pictures");
        assert_eq!(abbreviated("/media/x", Some("/")), "/media/x");
        assert_eq!(abbreviated("/media/x", None), "/media/x");
    }
}
