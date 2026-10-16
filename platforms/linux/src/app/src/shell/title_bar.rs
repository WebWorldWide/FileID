use adw::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

use super::status::{percent, Job, JobAction, Status, Tone};
use super::{grouped, hbox, label, matching_pages, section_label, set_tone, spacer, vbox, Action, GrainBar};

#[derive(Debug, PartialEq)]
pub struct SearchItem {
    pub label: String,
    pub hint: String,
    pub tone: Option<Tone>,
    pub action: Action,
}

/// Ask Chat, Library hand-off, then matching pages; Enter runs the first row.
pub fn search_groups(query: &str) -> Vec<(&'static str, Vec<SearchItem>)> {
    let q = query.trim();
    let mut groups = Vec::new();
    if !q.is_empty() {
        groups.push((
            "Ask Chat",
            vec![SearchItem {
                label: format!("“{q}”"),
                hint: "↵".into(),
                tone: Some(Tone::Ai),
                action: Action::AskChat(q.to_owned()),
            }],
        ));
        groups.push((
            "Library",
            vec![SearchItem {
                label: format!("Find “{q}” in the library"),
                hint: "Library".into(),
                tone: None,
                action: Action::LibraryQuery(q.to_owned()),
            }],
        ));
    }
    let pages: Vec<SearchItem> = matching_pages(q)
        .into_iter()
        .map(|page| SearchItem {
            label: page.title.into(),
            hint: page.search_hint(),
            tone: None,
            action: Action::Navigate(page.id),
        })
        .collect();
    if !pages.is_empty() {
        groups.push(("Pages", pages));
    }
    groups
}

struct JobRow {
    sub: gtk::Label,
    state: gtk::Label,
    bar: GrainBar,
    action: Option<(gtk::Button, Rc<RefCell<Option<JobAction>>>)>,
}

pub struct TitleBar {
    pub header: adw::HeaderBar,
    act: Rc<dyn Fn(Action)>,
    folder_name: gtk::Label,
    folder_row: gtk::Box,
    folder_row_name: gtk::Label,
    folder_row_path: gtk::Label,
    folder_row_count: gtk::Label,
    no_folder: gtk::Label,
    rescan: gtk::Button,
    folder_footer: gtk::Box,
    search_text: gtk::Text,
    search_popover: gtk::Popover,
    search_list: gtk::Box,
    activity_dot: gtk::Box,
    activity_label: gtk::Label,
    activity_bar: GrainBar,
    activity_pct: gtk::Label,
    activity_popover: gtk::Popover,
    activity_count: gtk::Label,
    activity_list: gtk::Box,
    job_shape: RefCell<Vec<(&'static str, bool)>>,
    job_rows: RefCell<Vec<JobRow>>,
    settings: gtk::Button,
}

impl TitleBar {
    pub fn new(act: Rc<dyn Fn(Action)>) -> Rc<Self> {
        let header = adw::HeaderBar::builder().css_classes(["fileid-titlebar"]).build();

        let toggle = gtk::Button::builder()
            .icon_name("sidebar-show-symbolic")
            .css_classes(["tb-plain"])
            .tooltip_text("Toggle sidebar (F9)")
            .valign(gtk::Align::Center)
            .build();
        let act_toggle = act.clone();
        toggle.connect_clicked(move |_| act_toggle(Action::ToggleSidebar));
        header.pack_start(&toggle);

        let folder_content = hbox(10);
        folder_content.append(&label("FileID", &["wordmark"]));
        let rule = gtk::Box::builder().css_classes(["vrule"]).valign(gtk::Align::Center).build();
        folder_content.append(&rule);
        let folder_name = label("No folder", &["tb-folder", "empty"]);
        folder_name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        folder_name.set_max_width_chars(22);
        folder_content.append(&folder_name);
        folder_content.append(&label("▾", &["tb-caret"]));
        let folder_popover = popover(360);
        let folder_button = gtk::MenuButton::builder()
            .child(&folder_content)
            .popover(&folder_popover)
            .css_classes(["tb-control"])
            .tooltip_text("Switch folder")
            .valign(gtk::Align::Center)
            .build();
        header.pack_start(&folder_button);

        let folder_body = vbox(0);
        let head = section_label("Folders");
        head.add_css_class("pop-head");
        folder_body.append(&head);
        let folder_row = hbox(10);
        folder_row.add_css_class("pop-row");
        let mark = label("■", &["tone-gold"]);
        mark.set_valign(gtk::Align::Start);
        folder_row.append(&mark);
        let names = vbox(2);
        names.set_hexpand(true);
        let folder_row_name = label("", &["pop-title"]);
        folder_row_name.set_ellipsize(gtk::pango::EllipsizeMode::End);
        names.append(&folder_row_name);
        let folder_row_path = label("", &["pop-meta", "monospace"]);
        folder_row_path.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        names.append(&folder_row_path);
        folder_row.append(&names);
        let folder_row_count = label("", &["pop-meta"]);
        folder_row_count.set_valign(gtk::Align::Start);
        folder_row.append(&folder_row_count);
        folder_body.append(&folder_row);
        let no_folder = label("No folder picked yet.", &["pop-meta"]);
        no_folder.set_margin_start(12);
        no_folder.set_margin_bottom(10);
        folder_body.append(&no_folder);
        let actions = hbox(8);
        actions.add_css_class("pop-actions");
        let add = gtk::Button::builder().label("Add folder…").css_classes(["secondary"]).build();
        add.set_tooltip_text(Some("Add folder (Ctrl+O)"));
        let rescan = gtk::Button::builder().label("Rescan").css_classes(["secondary"]).build();
        actions.append(&add);
        actions.append(&rescan);
        folder_body.append(&actions);
        let folder_footer = hbox(8);
        folder_footer.add_css_class("pop-footer");
        let clear = gtk::Button::builder()
            .label("Clear folder")
            .css_classes(["ghost"])
            .tooltip_text("Drop the current folder selection. Doesn't touch the library.")
            .build();
        let wipe = gtk::Button::builder()
            .label("Wipe library + rescan…")
            .css_classes(["ghost", "danger"])
            .tooltip_text("Delete the current library and run a fresh scan against this folder.")
            .build();
        folder_footer.append(&clear);
        folder_footer.append(&spacer());
        folder_footer.append(&wipe);
        folder_body.append(&folder_footer);
        folder_popover.set_child(Some(&folder_body));
        for (button, action) in [
            (&add, Action::PickFolder),
            (&rescan, Action::StartScan),
            (&clear, Action::ClearFolder),
            (&wipe, Action::WipeAndRescan),
        ] {
            let act = act.clone();
            let popover = folder_popover.clone();
            button.connect_clicked(move |_| {
                popover.popdown();
                act(action.clone());
            });
        }

        let search_box = hbox(8);
        search_box.add_css_class("tb-search");
        search_box.set_size_request(360, -1);
        search_box.set_valign(gtk::Align::Center);
        let glass = gtk::Image::from_icon_name("system-search-symbolic");
        glass.add_css_class("tone-muted");
        search_box.append(&glass);
        let search_text = gtk::Text::builder().placeholder_text("Search or ask").hexpand(true).build();
        search_text.update_property(&[gtk::accessible::Property::Label("Search or ask")]);
        search_box.append(&search_text);
        let kbd = label("Ctrl+K", &["kbd"]);
        kbd.set_valign(gtk::Align::Center);
        search_box.append(&kbd);
        let search_popover = popover(400);
        search_popover.set_autohide(false);
        search_popover.set_can_focus(false);
        search_popover.set_parent(&search_box);
        search_box.connect_destroy(glib::clone!(
            #[weak]
            search_popover,
            move |_| search_popover.unparent()
        ));
        let search_list = vbox(0);
        search_list.set_margin_bottom(6);
        search_popover.set_child(Some(&search_list));
        header.set_title_widget(Some(&search_box));

        let settings = gtk::Button::builder()
            .icon_name("emblem-system-symbolic")
            .css_classes(["tb-control", "tb-icon"])
            .tooltip_text("Settings (Ctrl+,)")
            .valign(gtk::Align::Center)
            .build();
        settings.update_property(&[gtk::accessible::Property::Label("Settings")]);
        let act_settings = act.clone();
        settings.connect_clicked(move |_| act_settings(Action::Navigate("settings")));
        header.pack_end(&settings);

        let activity_content = hbox(8);
        let activity_dot = gtk::Box::builder().css_classes(["tone-dot", "tone-info"]).valign(gtk::Align::Center).build();
        activity_content.append(&activity_dot);
        let activity_label = label("Idle", &["tb-activity-label"]);
        activity_content.append(&activity_label);
        let activity_bar = GrainBar::new(true);
        activity_bar.bar.set_size_request(54, -1);
        activity_bar.set(1.0, Tone::Info);
        activity_content.append(&activity_bar.bar);
        let activity_pct = label("", &["tb-activity-pct"]);
        activity_pct.set_width_chars(4);
        activity_pct.set_xalign(1.0);
        activity_pct.set_visible(false);
        activity_content.append(&activity_pct);
        let activity_popover = popover(400);
        let activity_button = gtk::MenuButton::builder()
            .child(&activity_content)
            .popover(&activity_popover)
            .css_classes(["tb-control"])
            .tooltip_text("Activity")
            .valign(gtk::Align::Center)
            .build();
        header.pack_end(&activity_button);
        let activity_body = vbox(0);
        let activity_head = hbox(8);
        activity_head.add_css_class("pop-head");
        activity_head.set_margin_bottom(4);
        activity_head.append(&section_label("Activity"));
        activity_head.append(&spacer());
        let activity_count = label("Nothing running", &["pop-meta"]);
        activity_head.append(&activity_count);
        activity_body.append(&activity_head);
        activity_body.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        let activity_list = vbox(0);
        activity_body.append(&activity_list);
        activity_popover.set_child(Some(&activity_body));

        let bar = Rc::new(Self {
            header,
            act,
            folder_name,
            folder_row,
            folder_row_name,
            folder_row_path,
            folder_row_count,
            no_folder,
            rescan,
            folder_footer,
            search_text,
            search_popover,
            search_list,
            activity_dot,
            activity_label,
            activity_bar,
            activity_pct,
            activity_popover,
            activity_count,
            activity_list,
            job_shape: RefCell::new(vec![("", false)]),
            job_rows: RefCell::new(Vec::new()),
            settings,
        });
        bar.wire_search();
        bar.update(&Status::default(), None);
        bar
    }

    fn wire_search(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.search_text.connect_changed(move |text| {
            let Some(bar) = weak.upgrade() else { return };
            if text.has_focus() {
                bar.show_results();
            }
        });
        let weak = Rc::downgrade(self);
        self.search_text.connect_activate(move |_| {
            let Some(bar) = weak.upgrade() else { return };
            let first = search_groups(&bar.search_text.text())
                .into_iter()
                .next()
                .and_then(|(_, items)| items.into_iter().next());
            if let Some(item) = first {
                bar.run(item.action);
            }
        });
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |_, _, _, _| {
            if let Some(bar) = weak.upgrade() {
                bar.show_results();
            }
        });
        self.search_text.add_controller(click);
        let focus = gtk::EventControllerFocus::new();
        let weak = Rc::downgrade(self);
        focus.connect_leave(move |_| {
            if let Some(bar) = weak.upgrade() {
                bar.search_popover.popdown();
            }
        });
        self.search_text.add_controller(focus);
        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, _| {
            if key != gtk::gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            if let Some(bar) = weak.upgrade() {
                bar.dismiss_search();
            }
            glib::Propagation::Stop
        });
        self.search_text.add_controller(keys);
    }

    pub fn focus_search(self: &Rc<Self>) {
        self.search_text.grab_focus();
        self.show_results();
    }

    fn dismiss_search(&self) {
        self.search_text.set_text("");
        self.search_popover.popdown();
        if let Some(window) = self.header.root().and_downcast::<gtk::Window>() {
            gtk::prelude::GtkWindowExt::set_focus(&window, gtk::Widget::NONE);
        }
    }

    fn run(&self, action: Action) {
        self.dismiss_search();
        (self.act)(action);
    }

    fn show_results(self: &Rc<Self>) {
        while let Some(child) = self.search_list.first_child() {
            self.search_list.remove(&child);
        }
        let mut first = true;
        for (head, items) in search_groups(&self.search_text.text()) {
            let heading = section_label(head);
            heading.set_margin_start(12);
            heading.set_margin_top(10);
            heading.set_margin_bottom(4);
            self.search_list.append(&heading);
            for item in items {
                let content = hbox(12);
                let name = label(&item.label, &[]);
                name.set_hexpand(true);
                name.set_ellipsize(gtk::pango::EllipsizeMode::End);
                if let Some(tone) = item.tone {
                    set_tone(&name, tone);
                }
                content.append(&name);
                content.append(&label(&item.hint, &["pop-meta"]));
                let row = gtk::Button::builder()
                    .child(&content)
                    .css_classes(["result-row"])
                    .focus_on_click(false)
                    .build();
                if first {
                    row.add_css_class("first");
                    first = false;
                }
                let weak = Rc::downgrade(self);
                let action = item.action;
                row.connect_clicked(move |_| {
                    if let Some(bar) = weak.upgrade() {
                        bar.run(action.clone());
                    }
                });
                self.search_list.append(&row);
            }
        }
        self.search_popover.popup();
    }

    pub fn set_settings_active(&self, active: bool) {
        if active {
            self.settings.add_css_class("active");
        } else {
            self.settings.remove_css_class("active");
        }
    }

    pub fn update(self: &Rc<Self>, status: &Status, folder_path: Option<&str>) {
        match status.folder.as_deref() {
            Some(name) => {
                self.folder_name.set_text(name);
                self.folder_name.remove_css_class("empty");
                self.folder_row_name.set_text(name);
                self.folder_row_path.set_text(folder_path.unwrap_or_default());
                self.folder_row_path.set_tooltip_text(folder_path);
                self.folder_row_count.set_text(&if status.total_files > 0 {
                    format!("{} files", grouped(status.total_files))
                } else {
                    "Not scanned".to_owned()
                });
                self.rescan.set_label(&format!("Rescan {name}"));
            }
            None => {
                self.folder_name.set_text("No folder");
                self.folder_name.add_css_class("empty");
            }
        }
        let has_folder = status.folder.is_some();
        self.folder_row.set_visible(has_folder);
        self.no_folder.set_visible(!has_folder);
        self.rescan.set_visible(has_folder);
        self.rescan.set_sensitive(status.can_start());
        self.folder_footer.set_visible(has_folder);

        let activity = status.activity();
        set_tone(&self.activity_dot, activity.tone);
        self.activity_label.set_text(activity.label);
        self.activity_bar.set(activity.fraction, activity.tone);
        self.activity_pct.set_visible(!activity.idle);
        self.activity_pct.set_text(&percent(activity.fraction));
        set_tone(&self.activity_pct, activity.tone);

        let jobs = status.jobs();
        let running = jobs.iter().filter(|job| matches!(job.tone, Tone::Gold | Tone::Ai)).count();
        self.activity_count.set_text(&if running > 0 {
            format!("{running} running")
        } else {
            "Nothing running".to_owned()
        });
        let shape: Vec<(&'static str, bool)> = jobs.iter().map(|job| (job.name, job.action.is_some())).collect();
        if *self.job_shape.borrow() != shape {
            self.rebuild_jobs(&jobs);
            self.job_shape.replace(shape);
        }
        for (row, job) in self.job_rows.borrow().iter().zip(&jobs) {
            row.sub.set_text(&job.sub);
            row.state.set_text(&job.state);
            set_tone(&row.state, job.tone);
            row.bar.set(job.fraction, job.tone);
            if let (Some((button, slot)), Some(action)) = (&row.action, job.action) {
                button.set_label(match action {
                    JobAction::PauseScan => "Pause",
                    JobAction::ResumeScan => "Resume",
                    JobAction::CancelDeep => "Cancel",
                });
                slot.replace(Some(action));
            }
        }
    }

    fn rebuild_jobs(&self, jobs: &[Job]) {
        while let Some(child) = self.activity_list.first_child() {
            self.activity_list.remove(&child);
        }
        let mut rows = Vec::new();
        if jobs.is_empty() {
            let empty = label("Scans and captions appear here.", &["pop-meta"]);
            empty.set_margin_start(14);
            empty.set_margin_end(14);
            empty.set_margin_top(14);
            empty.set_margin_bottom(14);
            self.activity_list.append(&empty);
        }
        for job in jobs {
            let row = vbox(8);
            row.set_margin_start(14);
            row.set_margin_end(14);
            row.set_margin_top(12);
            row.set_margin_bottom(12);
            let head = hbox(10);
            head.append(&label(job.name, &["pop-title"]));
            let sub = label("", &["pop-meta"]);
            sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
            sub.set_hexpand(true);
            head.append(&sub);
            let state = label("", &["pop-meta"]);
            head.append(&state);
            row.append(&head);
            let bar = GrainBar::new(false);
            row.append(&bar.bar);
            let buttons = hbox(6);
            let action = job.action.map(|_| {
                let button = gtk::Button::builder().css_classes(["secondary", "small"]).build();
                let slot = Rc::new(RefCell::new(None::<JobAction>));
                let act = self.act.clone();
                let target = slot.clone();
                button.connect_clicked(move |_| {
                    let action = match *target.borrow() {
                        Some(JobAction::PauseScan) => Action::PauseScan,
                        Some(JobAction::ResumeScan) => Action::ResumeScan,
                        Some(JobAction::CancelDeep) => Action::CancelDeep,
                        None => return,
                    };
                    act(action);
                });
                buttons.append(&button);
                (button, slot)
            });
            let page = super::page(job.open).map_or(job.open, |page| page.title);
            let open = gtk::Button::builder()
                .label(format!("Open {page}"))
                .css_classes(["secondary", "small"])
                .build();
            let act = self.act.clone();
            let popover = self.activity_popover.clone();
            let target = job.open;
            open.connect_clicked(move |_| {
                popover.popdown();
                act(Action::Navigate(target));
            });
            buttons.append(&open);
            row.append(&buttons);
            self.activity_list.append(&row);
            self.activity_list.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            rows.push(JobRow { sub, state, bar, action });
        }
        self.job_rows.replace(rows);
    }
}

fn popover(width: i32) -> gtk::Popover {
    let popover = gtk::Popover::builder().css_classes(["fileid-popover"]).position(gtk::PositionType::Bottom).build();
    popover.set_size_request(width, -1);
    popover
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_search_lists_every_page() {
        let groups = search_groups("  ");
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].0, "Pages");
        assert_eq!(groups[0].1.len(), super::super::PAGES.len());
        assert_eq!(groups[0].1[0].hint, "Browse · Ctrl+1");
    }

    #[test]
    fn typed_search_offers_chat_then_library_then_pages() {
        let groups = search_groups("clean");
        let heads: Vec<_> = groups.iter().map(|(head, _)| *head).collect();
        assert_eq!(heads, ["Ask Chat", "Library", "Pages"]);
        assert_eq!(groups[0].1[0].action, Action::AskChat("clean".into()));
        assert_eq!(groups[0].1[0].tone, Some(Tone::Ai));
        assert_eq!(groups[1].1[0].action, Action::LibraryQuery("clean".into()));
        assert_eq!(groups[2].1[0].action, Action::Navigate("cleanup"));
        let no_pages = search_groups("sunset beach");
        assert_eq!(no_pages.len(), 2);
    }
}
