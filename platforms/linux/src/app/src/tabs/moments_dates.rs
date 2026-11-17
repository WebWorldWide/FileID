// Search & Moments capture-date window: From / To buttons under the search
// mode, each opening a calendar. Sent as `capturedFrom` / `capturedTo`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::v3;

pub(super) type Day = (i32, i32, i32);

pub(super) const REVERSED: &str = "The From date is after the To date. Change one to search.";

fn day_start(day: Day) -> Option<glib::DateTime> {
    glib::DateTime::from_local(day.0, day.1, day.2, 0, 0, 0.0).ok()
}

/// Local midnight at the start of `from` through the last second of `to`.
pub(super) fn window(from: Option<Day>, to: Option<Day>) -> Result<(Option<f64>, Option<f64>), &'static str> {
    let start = from.and_then(day_start).map(|day| day.to_unix() as f64);
    let end = to
        .and_then(day_start)
        .and_then(|day| day.add_days(1).ok())
        .map(|next| next.to_unix() as f64 - 1.0);
    match (start, end) {
        (Some(start), Some(end)) if start > end => Err(REVERSED),
        window => Ok(window),
    }
}

pub(super) fn button_label(prefix: &str, day: Option<Day>) -> String {
    match day {
        Some((year, month, day)) => format!("{prefix} {year:04}-{month:02}-{day:02}"),
        None => format!("{prefix} any date"),
    }
}

struct DateButton {
    prefix: &'static str,
    button: gtk::Button,
    day: Cell<Option<Day>>,
}

impl DateButton {
    fn set(&self, day: Option<Day>) -> bool {
        let changed = self.day.replace(day) != day;
        self.button.set_label(&button_label(self.prefix, day));
        changed
    }
}

pub(super) struct DateFilter {
    pub(super) root: gtk::Box,
    from: Rc<DateButton>,
    to: Rc<DateButton>,
    changed: RefCell<Option<Rc<dyn Fn()>>>,
}

impl DateFilter {
    pub(super) fn new() -> Rc<Self> {
        let root = v3::hbox(6);
        let side = |prefix: &'static str, spoken: &str| {
            let button = v3::secondary(&button_label(prefix, None), false);
            button.set_tooltip_text(Some(spoken));
            button.update_property(&[gtk::accessible::Property::Label(spoken)]);
            root.append(&button);
            Rc::new(DateButton { prefix, button, day: Cell::new(None) })
        };
        let from = side("From", "Only files captured on or after this day");
        let to = side("To", "Only files captured on or before this day");
        let this = Rc::new(Self { root, from, to, changed: RefCell::new(None) });
        for side in [&this.from, &this.to] {
            let filter = Rc::downgrade(&this);
            let weak = Rc::downgrade(side);
            side.button.connect_clicked(move |button| {
                if let (Some(filter), Some(side)) = (filter.upgrade(), weak.upgrade()) {
                    filter.pick(&side, button);
                }
            });
        }
        this
    }

    fn pick(self: &Rc<Self>, side: &Rc<DateButton>, anchor: &gtk::Button) {
        let calendar = gtk::Calendar::new();
        if let Some(day) = side.day.get().and_then(day_start) {
            calendar.select_day(&day);
        }
        let touched = Rc::new(Cell::new(false));
        let cleared = Rc::new(Cell::new(false));
        let flag = touched.clone();
        calendar.connect_day_selected(move |_| flag.set(true));
        let any = v3::ghost("Any date");
        let content = v3::vbox(6);
        content.append(&calendar);
        content.append(&any);
        let popover = gtk::Popover::builder()
            .css_classes(["fileid-popover"])
            .child(&content)
            .build();
        popover.set_parent(anchor);
        let weak = popover.downgrade();
        let flag = cleared.clone();
        any.connect_clicked(move |_| {
            flag.set(true);
            if let Some(popover) = weak.upgrade() {
                popover.popdown();
            }
        });
        let filter = Rc::downgrade(self);
        let side = Rc::downgrade(side);
        popover.connect_closed(move |popover| {
            if let (Some(filter), Some(side)) = (filter.upgrade(), side.upgrade()) {
                let next = if cleared.get() {
                    None
                } else if touched.get() {
                    Some(calendar.date().ymd())
                } else {
                    side.day.get()
                };
                if side.set(next) {
                    filter.notify();
                }
            }
            popover.unparent();
        });
        popover.popup();
    }

    fn notify(&self) {
        let callback = self.changed.borrow().clone();
        if let Some(callback) = callback {
            callback();
        }
    }

    pub(super) fn connect_changed(&self, callback: impl Fn() + 'static) {
        self.changed.replace(Some(Rc::new(callback)));
    }

    pub(super) fn window(&self) -> Result<(Option<f64>, Option<f64>), &'static str> {
        window(self.from.day.get(), self.to.day.get())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_day_spans_midnight_to_the_last_second() {
        let (from, to) = window(Some((2024, 9, 19)), Some((2024, 9, 19))).unwrap();
        let (from, to) = (from.unwrap(), to.unwrap());
        assert!(to > from);
        assert!((to - from - 86_399.0).abs() <= 3_600.0, "one local day, give or take a DST hour");
    }

    #[test]
    fn open_ends_are_omitted_and_reversed_days_refused() {
        assert_eq!(window(None, None), Ok((None, None)));
        let (from, to) = window(Some((2023, 1, 1)), None).unwrap();
        assert!(from.is_some() && to.is_none());
        let (from, to) = window(None, Some((2023, 1, 1))).unwrap();
        assert!(from.is_none() && to.is_some());
        assert_eq!(window(Some((2024, 3, 2)), Some((2024, 3, 1))), Err(REVERSED));
        assert!(window(Some((2024, 3, 1)), Some((2024, 3, 2))).is_ok());
    }

    #[test]
    fn labels_show_iso_days_or_any_date() {
        assert_eq!(button_label("From", None), "From any date");
        assert_eq!(button_label("To", Some((2024, 9, 5))), "To 2024-09-05");
    }
}
