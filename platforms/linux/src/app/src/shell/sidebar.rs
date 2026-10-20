use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::status::{ScanControls, Status, Tone, STAGES};
use super::{hbox, label, set_tone, spacer, vbox, Action, Area, GrainBar};

struct NavRow {
    id: &'static str,
    button: gtk::Button,
    badge: gtk::Label,
}

struct ScanCard {
    stack: gtk::Stack,
    title: gtk::Label,
    pct: gtk::Label,
    bar: GrainBar,
    segments: Vec<gtk::Box>,
    step: gtk::Label,
    details_toggle: gtk::Button,
    sub: gtk::Label,
    details: gtk::Revealer,
    stats: gtk::Box,
    scan: gtk::Button,
    scan_content: adw::ButtonContent,
    rescan: gtk::Button,
    pause: gtk::Button,
    resume: gtk::Button,
    cancel: gtk::Button,
}

pub struct Sidebar {
    pub root: gtk::Box,
    area_buttons: Vec<gtk::Button>,
    pill: gtk::DrawingArea,
    pill_pos: Rc<Cell<f64>>,
    pill_anim: RefCell<Option<adw::SpringAnimation>>,
    area_stack: gtk::Stack,
    shown_area: Cell<Option<Area>>,
    rows: Vec<NavRow>,
    note: gtk::Box,
    note_label: gtk::Label,
    card: ScanCard,
}

impl Sidebar {
    pub fn new(act: Rc<dyn Fn(Action)>) -> Rc<Self> {
        let root = vbox(0);
        root.add_css_class("fileid-sidebar");

        let (switcher, area_buttons, pill, pill_pos) = area_switcher(&act);
        root.append(&switcher);

        let area_stack = gtk::Stack::builder()
            .transition_type(gtk::StackTransitionType::Crossfade)
            .transition_duration(140)
            .vhomogeneous(false)
            .build();
        let mut rows = Vec::new();
        for area in Area::ALL {
            let list = vbox(2);
            list.set_margin_bottom(10);
            for page in area.pages() {
                let row = nav_row(page.id, page.title, page.icon, &page.tooltip(), &act);
                list.append(&row.button);
                rows.push(row);
            }
            area_stack.add_named(&list, Some(area.name()));
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&area_stack)
            .build();
        root.append(&scroller);

        let note = hbox(6);
        note.add_css_class("engine-note");
        note.set_margin_start(12);
        note.set_margin_end(12);
        note.set_margin_bottom(8);
        note.set_visible(false);
        let note_icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
        note_icon.add_css_class("tone-warning");
        note_icon.set_valign(gtk::Align::Start);
        note.append(&note_icon);
        let note_label = label("", &[]);
        note_label.set_wrap(true);
        note_label.set_hexpand(true);
        note_label.set_lines(4);
        note_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        note.append(&note_label);
        let dismiss = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .css_classes(["tb-plain"])
            .valign(gtk::Align::Start)
            .tooltip_text("Dismiss")
            .build();
        let act_dismiss = act.clone();
        dismiss.connect_clicked(move |_| act_dismiss(Action::DismissNote));
        note.append(&dismiss);
        root.append(&note);

        let (frame, card) = scan_card(&act);
        root.append(&frame);

        Rc::new(Self {
            root,
            area_buttons,
            pill,
            pill_pos,
            pill_anim: RefCell::new(None),
            area_stack,
            shown_area: Cell::new(None),
            rows,
            note,
            note_label,
            card,
        })
    }

    /// Marks `id` selected and shows its area; Settings keeps the current area.
    pub fn show_page(&self, id: &str) {
        for row in &self.rows {
            if row.id == id {
                row.button.add_css_class("active");
            } else {
                row.button.remove_css_class("active");
            }
        }
        if let Some(area) = super::page(id).and_then(|page| page.area) {
            self.show_area(area);
        }
    }

    fn show_area(&self, area: Area) {
        self.area_stack.set_visible_child_name(area.name());
        for (index, button) in self.area_buttons.iter().enumerate() {
            if index == area.index() {
                button.add_css_class("on");
            } else {
                button.remove_css_class("on");
            }
        }
        let to = area.index() as f64;
        match self.shown_area.replace(Some(area)) {
            Some(previous) if previous == area => return,
            None => {
                self.pill_pos.set(to);
                self.pill.queue_draw();
                return;
            }
            Some(_) => {}
        }
        let pill = self.pill.clone();
        let pos = self.pill_pos.clone();
        let from = pos.get();
        if let Some(previous) = self.pill_anim.take() {
            previous.pause();
        }
        let anim = crate::spring::animate(&self.pill, from, to, move |value| {
            pos.set(value);
            pill.queue_draw();
        });
        self.pill_anim.replace(Some(anim));
    }

    pub fn set_note(&self, text: Option<&str>) {
        match text {
            Some(text) => {
                self.note_label.set_text(text);
                self.note_label.set_tooltip_text(Some(text));
                self.note.set_visible(true);
            }
            None => self.note.set_visible(false),
        }
    }

    pub fn update(&self, status: &Status) {
        for row in &self.rows {
            match status.badge(row.id) {
                Some((text, tone)) => {
                    row.badge.set_text(&text);
                    set_tone(&row.badge, tone);
                    row.badge.set_visible(true);
                }
                None => row.badge.set_visible(false),
            }
        }
        self.card.update(status);
    }
}

fn area_switcher(
    act: &Rc<dyn Fn(Action)>,
) -> (
    gtk::Overlay,
    Vec<gtk::Button>,
    gtk::DrawingArea,
    Rc<Cell<f64>>,
) {
    const GAP: f64 = 2.0;
    let pill_pos = Rc::new(Cell::new(0.0_f64));
    let pill = gtk::DrawingArea::builder()
        .content_height(26)
        .hexpand(true)
        .can_target(false)
        .build();
    let pos = pill_pos.clone();
    pill.set_draw_func(move |_, cr, width, height| {
        let segment = (f64::from(width) - GAP * 2.0) / 3.0;
        let x = pos.get().clamp(-0.2, 2.2) * (segment + GAP);
        rounded_rect(cr, x, 0.0, segment, f64::from(height), 6.0);
        let (r, g, b) = crate::theme::rgb::GOLD;
        cr.set_source_rgb(r, g, b);
        let _ = cr.fill();
    });
    let buttons_box = gtk::Box::builder()
        .homogeneous(true)
        .spacing(GAP as i32)
        .build();
    let mut buttons = Vec::new();
    for area in Area::ALL {
        let button = gtk::Button::builder()
            .label(area.name())
            .tooltip_text(format!("{} pages ({})", area.name(), area.shortcut_hint()))
            .build();
        let act = act.clone();
        button.connect_clicked(move |_| act(Action::SelectArea(area)));
        buttons_box.append(&button);
        buttons.push(button);
    }
    let overlay = gtk::Overlay::builder()
        .child(&pill)
        .css_classes(["area-switcher"])
        .build();
    overlay.add_overlay(&buttons_box);
    overlay.set_measure_overlay(&buttons_box, true);
    overlay.set_margin_start(10);
    overlay.set_margin_end(10);
    overlay.set_margin_top(12);
    overlay.set_margin_bottom(10);
    (overlay, buttons, pill, pill_pos)
}

fn rounded_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, PI + FRAC_PI_2);
    cr.close_path();
}

fn nav_row(
    id: &'static str,
    title: &str,
    icon: &str,
    tooltip: &str,
    act: &Rc<dyn Fn(Action)>,
) -> NavRow {
    let content = hbox(10);
    let image = gtk::Image::from_icon_name(icon);
    image.set_pixel_size(14);
    image.set_size_request(18, -1);
    content.append(&image);
    let name = label(title, &[]);
    name.set_hexpand(true);
    name.set_ellipsize(gtk::pango::EllipsizeMode::End);
    content.append(&name);
    let badge = label("", &["nav-badge"]);
    badge.set_visible(false);
    content.append(&badge);
    let button = gtk::Button::builder()
        .child(&content)
        .css_classes(["nav-row"])
        .tooltip_text(tooltip)
        .build();
    button.update_property(&[gtk::accessible::Property::Label(&format!("{title} page"))]);
    let act = act.clone();
    button.connect_clicked(move |_| act(Action::Navigate(id)));
    NavRow { id, button, badge }
}

fn icon_button(icon: &str, text: &str, classes: &[&str]) -> (gtk::Button, adw::ButtonContent) {
    let content = adw::ButtonContent::builder()
        .icon_name(icon)
        .label(text)
        .build();
    let button = gtk::Button::builder().child(&content).build();
    for class in classes {
        button.add_css_class(class);
    }
    (button, content)
}

fn scan_card(act: &Rc<dyn Fn(Action)>) -> (gtk::Box, ScanCard) {
    let frame = vbox(0);
    frame.add_css_class("scan-card");
    frame.set_margin_start(10);
    frame.set_margin_end(10);
    frame.set_margin_bottom(10);
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .transition_duration(160)
        .vhomogeneous(false)
        .interpolate_size(true)
        .build();
    frame.append(&stack);

    let pick = vbox(8);
    pick.append(&label("Pick a folder to start", &["scan-title"]));
    let (pick_button, _) = icon_button(
        "folder-new-symbolic",
        "Pick a folder…",
        &["gold-button", "compact"],
    );
    pick_button.set_halign(gtk::Align::Start);
    pick_button.set_tooltip_text(Some("Pick a folder (Ctrl+O)"));
    let act_pick = act.clone();
    pick_button.connect_clicked(move |_| act_pick(Action::PickFolder));
    pick.append(&pick_button);
    pick.append(&label("Or drop a folder on the window.", &["scan-sub"]));
    stack.add_named(&pick, Some("pick"));

    let pipeline = vbox(8);
    let head = hbox(8);
    let title = label("", &["scan-title"]);
    title.set_hexpand(true);
    title.set_ellipsize(gtk::pango::EllipsizeMode::End);
    head.append(&title);
    let pct = label("0%", &["scan-pct"]);
    head.append(&pct);
    pipeline.append(&head);
    let bar = GrainBar::new(false);
    pipeline.append(&bar.bar);
    let segments_box = gtk::Box::builder().homogeneous(true).spacing(4).build();
    let segments: Vec<gtk::Box> = STAGES
        .iter()
        .map(|_| {
            let segment = gtk::Box::builder()
                .css_classes(["step-seg"])
                .valign(gtk::Align::Center)
                .build();
            segments_box.append(&segment);
            segment
        })
        .collect();
    pipeline.append(&segments_box);
    let step_row = hbox(4);
    let step = label("", &["scan-step"]);
    step.set_hexpand(true);
    step_row.append(&step);
    let details_toggle = gtk::Button::builder()
        .icon_name("pan-down-symbolic")
        .css_classes(["tb-plain"])
        .tooltip_text("Show scan details")
        .visible(false)
        .build();
    step_row.append(&details_toggle);
    pipeline.append(&step_row);
    let sub = label("", &["scan-sub"]);
    sub.set_wrap(true);
    sub.set_lines(2);
    sub.set_ellipsize(gtk::pango::EllipsizeMode::End);
    pipeline.append(&sub);
    let stats = vbox(4);
    stats.add_css_class("scan-stats");
    let details = gtk::Revealer::builder()
        .transition_type(gtk::RevealerTransitionType::SlideDown)
        .transition_duration(200)
        .child(&stats)
        .build();
    pipeline.append(&details);
    details_toggle.connect_clicked(glib::clone!(
        #[weak]
        details,
        #[weak]
        stats,
        move |toggle| {
            let open = !details.reveals_child();
            details.set_reveal_child(open);
            toggle.set_icon_name(if open {
                "pan-up-symbolic"
            } else {
                "pan-down-symbolic"
            });
            toggle.set_tooltip_text(Some(if open {
                "Hide scan details"
            } else {
                "Show scan details"
            }));
            if open {
                let target = stats.clone();
                let _ = crate::spring::animate(&stats, 0.0, 1.0, move |v| {
                    target.set_opacity(v.clamp(0.0, 1.0))
                });
            }
        }
    ));

    let controls = hbox(6);
    let (scan, scan_content) = icon_button(
        "media-playback-start-symbolic",
        "Scan",
        &["gold-button", "compact"],
    );
    scan.set_tooltip_text(Some("Scan (Ctrl+R)"));
    let (rescan, _) = icon_button("view-refresh-symbolic", "Rescan", &["secondary"]);
    rescan.set_tooltip_text(Some("Rescan (Ctrl+R)"));
    let (pause, _) = icon_button("media-playback-pause-symbolic", "Pause", &["secondary"]);
    let (resume, _) = icon_button(
        "media-playback-start-symbolic",
        "Resume",
        &["gold-button", "compact"],
    );
    let cancel = gtk::Button::builder()
        .label("Cancel")
        .css_classes(["ghost"])
        .build();
    for (button, action) in [
        (&scan, Action::StartScan),
        (&rescan, Action::StartScan),
        (&pause, Action::PauseScan),
        (&resume, Action::ResumeScan),
        (&cancel, Action::CancelScan),
    ] {
        let act = act.clone();
        button.connect_clicked(move |_| act(action.clone()));
        controls.append(button);
    }
    controls.append(&spacer());
    pipeline.append(&controls);
    stack.add_named(&pipeline, Some("pipeline"));

    let card = ScanCard {
        stack,
        title,
        pct,
        bar,
        segments,
        step,
        details_toggle,
        sub,
        details,
        stats,
        scan,
        scan_content,
        rescan,
        pause,
        resume,
        cancel,
    };
    (frame, card)
}

impl ScanCard {
    fn update(&self, status: &Status) {
        let controls = status.controls();
        if controls == ScanControls::PickFolder {
            self.stack.set_visible_child_name("pick");
            return;
        }
        self.stack.set_visible_child_name("pipeline");
        let p = status.pipeline();
        self.title.set_text(&p.title);
        self.pct.set_text(&super::status::percent(p.fraction));
        set_tone(&self.pct, p.accent);
        self.bar.set(p.fraction, p.bar);
        for (index, segment) in self.segments.iter().enumerate() {
            segment.remove_css_class("past");
            segment.remove_css_class("current");
            if index < p.stage {
                segment.add_css_class("past");
            } else if index == p.stage {
                segment.add_css_class("current");
                set_tone(segment, p.stage_tone);
            }
        }
        self.step
            .set_text(&format!("Step {} of 5 · {}", p.stage + 1, STAGES[p.stage]));
        self.sub.set_text(&p.sub);

        let rows = status.stats();
        self.details_toggle.set_visible(!rows.is_empty());
        if rows.is_empty() {
            self.details.set_reveal_child(false);
        }
        while let Some(child) = self.stats.first_child() {
            self.stats.remove(&child);
        }
        for (key, value, trailing, tone) in rows {
            let row = hbox(6);
            let key = label(key, &["stat-key"]);
            key.set_width_chars(8);
            row.append(&key);
            let value = label(&value, &["stat-value"]);
            if matches!(tone, Tone::Warning | Tone::Danger) {
                set_tone(&value, tone);
            }
            row.append(&value);
            row.append(&spacer());
            if let Some(trailing) = trailing {
                row.append(&label(&trailing, &["stat-trailing"]));
            }
            self.stats.append(&row);
        }

        let can_start = status.can_start();
        let running = matches!(controls, ScanControls::Running { .. });
        let paused = matches!(controls, ScanControls::Running { paused: true });
        self.scan.set_visible(controls == ScanControls::Scan);
        self.scan.set_sensitive(can_start);
        let folder = status.folder.as_deref().unwrap_or("folder");
        self.scan_content.set_label(&format!("Scan {folder}"));
        self.rescan.set_visible(controls == ScanControls::Rescan);
        self.rescan.set_sensitive(can_start);
        self.pause.set_visible(running && !paused);
        self.resume.set_visible(paused);
        self.cancel.set_visible(running);
        for button in [&self.pause, &self.resume, &self.cancel] {
            button.set_sensitive(status.engine_ready);
        }
    }
}
