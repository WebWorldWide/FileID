// Prototype v3 controls (shared/docs/DESIGN_V3_PORT.md, "Controls"): buttons,
// segmented bar, panel, rail row, status pill, fields and timecodes, so every
// page builds the same 28 px targets, the same gold focus ring and the same
// springs. The look lives in theme.rs under the `v3-*` classes.

use adw::prelude::*;
use gtk::glib;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::shell::status::Tone;
use crate::spring::{animate_with, Spring};

fn classed<W: IsA<gtk::Widget>>(widget: W, classes: &[&str]) -> W {
    for class in classes {
        widget.add_css_class(class);
    }
    widget
}

fn labelled(text: &str, classes: &[&str]) -> gtk::Button {
    classed(gtk::Button::builder().label(text).build(), classes)
}

/// Gold primary, 30 tall (`small` = 28).
pub fn primary(text: &str, small: bool) -> gtk::Button {
    labelled(text, if small { &["v3-primary", "h28"] } else { &["v3-primary"] })
}

/// Hairline secondary, 28 tall (`tall` = 30).
pub fn secondary(text: &str, tall: bool) -> gtk::Button {
    labelled(text, if tall { &["v3-secondary", "h30"] } else { &["v3-secondary"] })
}

pub fn ghost(text: &str) -> gtk::Button {
    labelled(text, &["v3-ghost"])
}

/// Tinted outline (Accept, Undo, Resume), 28 tall (`tall` = 30).
pub fn accent(text: &str, tone: Tone, tall: bool) -> gtk::Button {
    let button = labelled(text, &["v3-accent", tone.css()]);
    if tall {
        button.add_css_class("h30");
    }
    button
}

pub fn icon_button(icon: &str, label: &str) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name(icon)
        .tooltip_text(label)
        .css_classes(["v3-icon"])
        .build();
    button.update_property(&[gtk::accessible::Property::Label(label)]);
    button
}

pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    classed(gtk::Label::builder().label(text).xalign(0.0).build(), classes)
}

pub fn wrapped(text: &str, classes: &[&str]) -> gtk::Label {
    let label = label(text, classes);
    label.set_wrap(true);
    label.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    label
}

pub fn section(text: &str) -> gtk::Label {
    label(&text.to_uppercase(), &["section-label"])
}

pub fn meta(text: &str) -> gtk::Label {
    label(text, &["v3-meta"])
}

pub fn mono(text: &str, size_class: &str) -> gtk::Label {
    let label = label(text, &["v3-mono", size_class]);
    label.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    label
}

pub fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

pub fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

pub fn spacer() -> gtk::Box {
    let spacer = hbox(0);
    spacer.set_hexpand(true);
    spacer
}

pub fn clear(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

/// Status pill: 10.5 semibold in its colour with a 1 px outline.
pub fn pill(text: &str, tone: Tone) -> gtk::Label {
    let pill = label(text, &["v3-pill", tone.css()]);
    pill.set_valign(gtk::Align::Center);
    pill
}

pub fn text_field(placeholder: &str) -> gtk::Entry {
    gtk::Entry::builder()
        .placeholder_text(placeholder)
        .hexpand(true)
        .css_classes(["v3-field"])
        .build()
}

/// Mono number field, 96 × 28, no digit grouping.
pub fn number_field(min: f64, max: f64, step: f64, digits: u32) -> gtk::SpinButton {
    let field = gtk::SpinButton::with_range(min, max, step);
    field.set_digits(digits);
    field.set_numeric(true);
    field.set_width_chars(6);
    field.add_css_class("v3-number");
    field
}

/// Dashed empty-state notice: serif title and a subtitle.
pub fn dashed_notice(title: &str, subtitle: &str) -> gtk::Box {
    let notice = vbox(6);
    notice.add_css_class("v3-dashed");
    notice.append(&label(title, &["v3-serif-28"]));
    notice.append(&wrapped(subtitle, &["v3-body", "tone-text2"]));
    notice
}

/// An indeterminate bar that pulses only while visible.
pub fn busy_bar(width: i32) -> gtk::ProgressBar {
    let bar = gtk::ProgressBar::builder()
        .pulse_step(0.12)
        .width_request(width)
        .valign(gtk::Align::Center)
        .visible(false)
        .css_classes(["grain-bar", "mini", "tone-gold"])
        .build();
    let weak = bar.downgrade();
    glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
        let Some(bar) = weak.upgrade() else { return glib::ControlFlow::Break };
        if bar.is_mapped() {
            bar.pulse();
        }
        glib::ControlFlow::Continue
    });
    bar
}

/// m:ss, h:mm:ss past an hour.
pub fn timecode(seconds: f64) -> String {
    let total = if seconds.is_finite() { seconds.max(0.0).floor() as u64 } else { 0 };
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// "N minutes N seconds" for screen readers.
pub fn spoken_timecode(seconds: f64) -> String {
    let total = if seconds.is_finite() { seconds.max(0.0).floor() as u64 } else { 0 };
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    let unit = |n: u64, word: &str| format!("{n} {word}{}", if n == 1 { "" } else { "s" });
    let mut parts = Vec::new();
    if h > 0 {
        parts.push(unit(h, "hour"));
    }
    if m > 0 || h > 0 {
        parts.push(unit(m, "minute"));
    }
    parts.push(unit(s, "second"));
    parts.join(" ")
}

pub struct Panel {
    pub root: gtk::Box,
    pub title: gtk::Label,
    pub trailing: gtk::Box,
    pub body: gtk::Box,
    pub footer: gtk::Box,
}

/// Panel: frosted fill, radius 10, header label + trailing controls over a
/// hairline, body, and an optional footer on white 0.02.
pub fn panel(title: &str) -> Panel {
    let root = vbox(0);
    root.add_css_class("v3-panel");
    let header = hbox(8);
    header.add_css_class("v3-panel-header");
    let title = section(title);
    title.set_hexpand(true);
    header.append(&title);
    let trailing = hbox(6);
    header.append(&trailing);
    root.append(&header);
    let body = vbox(0);
    root.append(&body);
    let footer = hbox(8);
    footer.add_css_class("v3-panel-footer");
    footer.set_visible(false);
    root.append(&footer);
    Panel { root, title, trailing, body, footer }
}

impl Panel {
    pub fn set_title(&self, text: &str) {
        self.title.set_text(&text.to_uppercase());
    }

    pub fn show_footer(&self) -> &gtk::Box {
        self.footer.set_visible(true);
        &self.footer
    }
}

/// Rail row: a selectable list row (min 28, radius 8) whose child is free.
pub fn rail_row(child: &impl IsA<gtk::Widget>) -> gtk::Button {
    gtk::Button::builder().child(child).css_classes(["v3-rail"]).build()
}

pub fn set_selected(widget: &impl IsA<gtk::Widget>, selected: bool) {
    if selected {
        widget.add_css_class("selected");
    } else {
        widget.remove_css_class("selected");
    }
}

/// Segmented bar: 28 tall, a gold pill (radius 4, inset 2) that slides to the
/// selected segment on the tight spring.
pub struct Segmented {
    pub root: gtk::Overlay,
    buttons: Vec<gtk::Button>,
    selected: Cell<usize>,
    pill: gtk::DrawingArea,
    position: Rc<Cell<f64>>,
    animation: RefCell<Option<adw::SpringAnimation>>,
    handlers: RefCell<Vec<Rc<dyn Fn(usize)>>>,
}

impl Segmented {
    pub fn new(options: &[&str], selected: usize) -> Rc<Self> {
        let position = Rc::new(Cell::new(selected as f64));
        let pill = gtk::DrawingArea::builder().can_target(false).hexpand(true).build();
        let buttons_box = gtk::Box::builder().homogeneous(true).build();
        let count = options.len().max(1) as f64;
        let draw_position = position.clone();
        pill.set_draw_func(move |_, cr, width, height| {
            let inset = 2.0;
            let segment = f64::from(width) / count;
            let x = draw_position.get() * segment + inset;
            draw_round_rect(cr, x, inset, segment - inset * 2.0, f64::from(height) - inset * 2.0, 4.0);
            let (r, g, b) = crate::theme::rgb::GOLD;
            cr.set_source_rgb(r, g, b);
            let _ = cr.fill();
        });
        let mut buttons = Vec::new();
        for option in options {
            let button = gtk::Button::builder().label(*option).build();
            buttons_box.append(&button);
            buttons.push(button);
        }
        let root = gtk::Overlay::builder().child(&pill).css_classes(["v3-segmented"]).build();
        root.add_overlay(&buttons_box);
        root.set_measure_overlay(&buttons_box, true);
        let segmented = Rc::new(Self {
            root,
            buttons,
            selected: Cell::new(selected),
            pill,
            position,
            animation: RefCell::new(None),
            handlers: RefCell::new(Vec::new()),
        });
        for (index, button) in segmented.buttons.iter().enumerate() {
            let weak = Rc::downgrade(&segmented);
            button.connect_clicked(move |_| {
                if let Some(segmented) = weak.upgrade() {
                    segmented.select(index, true);
                }
            });
        }
        segmented.mark();
        segmented
    }

    pub fn selected(&self) -> usize {
        self.selected.get()
    }

    /// Changes the selection; `notify` runs the change handlers.
    pub fn select(&self, index: usize, notify: bool) {
        if index >= self.buttons.len() || index == self.selected.get() {
            return;
        }
        self.selected.set(index);
        self.mark();
        let pill = self.pill.clone();
        let position = self.position.clone();
        if let Some(previous) = self.animation.take() {
            previous.pause();
        }
        let from = position.get();
        let animation = animate_with(Spring::Tight, &self.pill, from, index as f64, move |value| {
            position.set(value);
            pill.queue_draw();
        });
        self.animation.replace(Some(animation));
        if notify {
            let handlers: Vec<_> = self.handlers.borrow().clone();
            for handler in handlers {
                handler(index);
            }
        }
    }

    fn mark(&self) {
        for (index, button) in self.buttons.iter().enumerate() {
            let on = index == self.selected.get();
            set_selected(button, on);
            button.update_state(&[gtk::accessible::State::Selected(Some(on))]);
        }
    }
}

pub fn draw_round_rect(cr: &gtk::cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    use std::f64::consts::{FRAC_PI_2, PI};
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let r = r.min(w / 2.0).min(h / 2.0);
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -FRAC_PI_2, 0.0);
    cr.arc(x + w - r, y + h - r, r, 0.0, FRAC_PI_2);
    cr.arc(x + r, y + h - r, r, FRAC_PI_2, PI);
    cr.arc(x + r, y + r, r, PI, PI + FRAC_PI_2);
    cr.close_path();
}

pub fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_owned())
}

pub fn parent_name(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Opens a file in the default app, or shows it in the file manager.
pub fn open_file(widget: &impl IsA<gtk::Widget>, path: &str) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    let window = widget.root().and_downcast::<gtk::Window>();
    launcher.launch(window.as_ref(), gio::Cancellable::NONE, |_| {});
}

pub fn show_in_files(widget: &impl IsA<gtk::Widget>, path: &str) {
    let launcher = gtk::FileLauncher::new(Some(&gio::File::for_path(path)));
    let window = widget.root().and_downcast::<gtk::Window>();
    let folder = std::path::Path::new(path).parent().map(gio::File::for_path);
    let fallback = window.clone();
    launcher.open_containing_folder(window.as_ref(), gio::Cancellable::NONE, move |result| {
        if let (Err(_), Some(folder)) = (result, folder) {
            gtk::FileLauncher::new(Some(&folder)).launch(fallback.as_ref(), gio::Cancellable::NONE, |_| {});
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timecodes_use_minutes_until_an_hour() {
        assert_eq!(timecode(0.0), "0:00");
        assert_eq!(timecode(65.9), "1:05");
        assert_eq!(timecode(3725.0), "1:02:05");
        assert_eq!(timecode(f64::NAN), "0:00");
        assert_eq!(spoken_timecode(65.0), "1 minute 5 seconds");
        assert_eq!(spoken_timecode(7.0), "7 seconds");
        assert_eq!(spoken_timecode(3600.0), "1 hour 0 minutes 0 seconds");
    }

    #[test]
    fn paths_split_into_name_and_folder() {
        assert_eq!(file_name("/a/b/clip.mp4"), "clip.mp4");
        assert_eq!(parent_name("/a/b/clip.mp4"), "/a/b");
    }
}
