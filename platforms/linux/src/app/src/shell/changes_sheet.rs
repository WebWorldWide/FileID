// "Changes this session": every change FileID made since launch, newest first,
// with Undo while it can still be put back (Windows `SessionChangesSheet`).

use adw::prelude::*;
use gtk::glib;
use std::rc::Rc;

use crate::changes::{self, Entry, Status};
use crate::v3::{hbox, label, spacer, vbox};

pub fn present(parent: &impl IsA<gtk::Widget>) {
    let dialog = adw::Dialog::builder()
        .title("Changes this session")
        .content_width(460)
        .content_height(420)
        .build();
    let list = vbox(6);
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(14);
    list.set_margin_end(14);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    let header = adw::HeaderBar::new();
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&scroll));
    dialog.set_child(Some(&view));

    let rebuild: Rc<dyn Fn()> = Rc::new({
        let list = list.clone();
        move || fill(&list)
    });
    rebuild();
    changes::subscribe(Rc::new({
        let weak = list.downgrade();
        let rebuild = rebuild.clone();
        move || {
            if weak.upgrade().is_some_and(|list| list.is_mapped()) {
                rebuild();
            }
        }
    }));
    dialog.present(Some(parent));
}

fn fill(list: &gtk::Box) {
    crate::v3::clear(list);
    let entries = changes::entries();
    if entries.is_empty() {
        list.append(&label("Nothing has changed yet.", &["v3-meta"]));
        return;
    }
    for entry in entries {
        list.append(&row(&entry));
    }
}

fn row(entry: &Entry) -> gtk::Box {
    let row = hbox(10);
    row.add_css_class("scan-card");
    let text = vbox(2);
    text.set_hexpand(true);
    let title = label(&entry.label, &[]);
    title.set_xalign(0.0);
    title.set_wrap(true);
    text.append(&title);
    let detail = match status_line(&entry.status) {
        Some(status) => format!("{} · {status}", clock(entry)),
        None => clock(entry),
    };
    let meta = label(&detail, &["v3-meta"]);
    meta.set_xalign(0.0);
    meta.set_wrap(true);
    text.append(&meta);
    row.append(&text);
    row.append(&spacer());
    if matches!(entry.status, Status::Undoable | Status::Failed(_)) {
        let undo = crate::v3::secondary(
            if matches!(entry.status, Status::Failed(_)) {
                "Try again"
            } else {
                "Undo"
            },
            false,
        );
        undo.set_valign(gtk::Align::Center);
        let id = entry.id;
        undo.connect_clicked(move |_| {
            changes::undo(id);
        });
        row.append(&undo);
    }
    row
}

fn status_line(status: &Status) -> Option<String> {
    let text = changes::status_text(status);
    (!text.is_empty()).then_some(text)
}

fn clock(entry: &Entry) -> String {
    let seconds = entry
        .at
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64);
    glib::DateTime::from_unix_local(seconds)
        .ok()
        .and_then(|time| time.format("%H:%M").ok())
        .map_or_else(String::new, |text| text.to_string())
}
