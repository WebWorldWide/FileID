// Spring-driven stand-ins for GTK's timed transitions: GtkRevealer and
// GtkStack crossfades run fixed-duration eases, so panes, expanding rows and
// view switches use these instead and move on the brand springs.

use std::cell::{Cell, RefCell};

use adw::prelude::*;
use gtk::subclass::prelude::*;
use gtk::{glib, graphene, gsk};

use crate::spring::{animate_with, Spring};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Slide {
    #[default]
    Down,
    Up,
    Left,
}

impl Slide {
    fn axis(self) -> gtk::Orientation {
        match self {
            Slide::Down | Slide::Up => gtk::Orientation::Vertical,
            Slide::Left => gtk::Orientation::Horizontal,
        }
    }
}

/// `size` scaled by spring progress; overshoot past 1 is kept, undershoot
/// below 0 reads as closed.
pub fn revealed_size(size: i32, progress: f64) -> i32 {
    (f64::from(size) * progress.max(0.0)).ceil() as i32
}

/// Child offset along the slide axis for a child of `child` px shown in
/// `shown` px: Down keeps the bottom edge in view, Up and Left the leading one.
pub fn slide_offset(slide: Slide, shown: i32, child: i32) -> i32 {
    match slide {
        Slide::Down => shown - child,
        Slide::Up | Slide::Left => 0,
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct SpringRevealer {
        pub child: RefCell<Option<gtk::Widget>>,
        pub slide: Cell<Slide>,
        pub open: Cell<bool>,
        pub progress: Cell<f64>,
        pub animation: RefCell<Option<adw::SpringAnimation>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SpringRevealer {
        const NAME: &'static str = "FileIDSpringRevealer";
        type Type = super::SpringRevealer;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SpringRevealer {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().set_overflow(gtk::Overflow::Hidden);
        }

        fn dispose(&self) {
            if let Some(animation) = self.animation.take() {
                animation.pause();
            }
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SpringRevealer {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let Some(child) = self.child.borrow().clone().filter(|child| child.should_layout()) else {
                return (0, 0, -1, -1);
            };
            let (min, nat, _, _) = child.measure(orientation, for_size);
            if orientation == self.slide.get().axis() {
                let progress = self.progress.get();
                (revealed_size(min, progress), revealed_size(nat, progress), -1, -1)
            } else {
                (min, nat, -1, -1)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, _baseline: i32) {
            let Some(child) = self.child.borrow().clone().filter(|child| child.should_layout()) else {
                return;
            };
            let slide = self.slide.get();
            let (child_width, child_height, point) = if slide.axis() == gtk::Orientation::Vertical {
                let (min, nat, _, _) = child.measure(gtk::Orientation::Vertical, width);
                let full = nat.max(min).max(height);
                (width, full, graphene::Point::new(0.0, slide_offset(slide, height, full) as f32))
            } else {
                let (min, nat, _, _) = child.measure(gtk::Orientation::Horizontal, height);
                let full = nat.max(min).max(width);
                (full, height, graphene::Point::new(slide_offset(slide, width, full) as f32, 0.0))
            };
            child.allocate(child_width, child_height, -1, Some(gsk::Transform::new().translate(&point)));
        }
    }

    #[derive(Default)]
    pub struct SpringScale {
        pub child: RefCell<Option<gtk::Widget>>,
        pub scale: Cell<f64>,
        pub target: Cell<f64>,
        pub animation: RefCell<Option<adw::SpringAnimation>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SpringScale {
        const NAME: &'static str = "FileIDSpringScale";
        type Type = super::SpringScale;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for SpringScale {
        fn constructed(&self) {
            self.parent_constructed();
            self.scale.set(1.0);
            self.target.set(1.0);
        }

        fn dispose(&self) {
            if let Some(animation) = self.animation.take() {
                animation.pause();
            }
            if let Some(child) = self.child.take() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for SpringScale {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.child
                .borrow()
                .as_ref()
                .filter(|child| child.should_layout())
                .map_or((0, 0, -1, -1), |child| child.measure(orientation, for_size))
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let Some(child) = self.child.borrow().clone().filter(|child| child.should_layout()) else {
                return;
            };
            let scale = self.scale.get() as f32;
            let center = graphene::Point::new(width as f32 / 2.0, height as f32 / 2.0);
            let transform = gsk::Transform::new()
                .translate(&center)
                .scale(scale, scale)
                .translate(&graphene::Point::new(-center.x(), -center.y()));
            child.allocate(width, height, baseline, Some(transform));
        }
    }
}

glib::wrapper! {
    /// A GtkRevealer whose slide runs on the standard spring.
    pub struct SpringRevealer(ObjectSubclass<imp::SpringRevealer>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SpringRevealer {
    pub fn new(slide: Slide) -> Self {
        let revealer: Self = glib::Object::new();
        revealer.imp().slide.set(slide);
        revealer
    }

    pub fn with_child(slide: Slide, child: &impl IsA<gtk::Widget>) -> Self {
        let revealer = Self::new(slide);
        revealer.set_child(Some(child));
        revealer
    }

    pub fn set_child(&self, child: Option<&impl IsA<gtk::Widget>>) {
        if let Some(old) = self.imp().child.take() {
            old.unparent();
        }
        if let Some(child) = child {
            let child = child.as_ref().clone();
            child.set_parent(self);
            child.set_child_visible(self.imp().open.get());
            self.imp().child.replace(Some(child));
        }
        self.queue_resize();
    }

    pub fn reveals_child(&self) -> bool {
        self.imp().open.get()
    }

    pub fn set_reveal_child(&self, open: bool) {
        let imp = self.imp();
        if imp.open.replace(open) == open {
            return;
        }
        if let Some(previous) = imp.animation.take() {
            previous.pause();
        }
        if open && let Some(child) = imp.child.borrow().as_ref() {
            child.set_child_visible(true);
        }
        let weak = self.downgrade();
        let animation = animate_with(
            Spring::Standard,
            self,
            imp.progress.get(),
            if open { 1.0 } else { 0.0 },
            move |value| {
                let Some(revealer) = weak.upgrade() else { return };
                let imp = revealer.imp();
                imp.progress.set(value);
                if value <= 0.0
                    && !imp.open.get()
                    && let Some(child) = imp.child.borrow().as_ref()
                {
                    child.set_child_visible(false);
                }
                revealer.queue_resize();
            },
        );
        animation.set_clamp(!open);
        imp.animation.replace(Some(animation));
    }
}

glib::wrapper! {
    /// Scales its child about the centre on the tight spring (selection).
    pub struct SpringScale(ObjectSubclass<imp::SpringScale>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SpringScale {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let holder: Self = glib::Object::new();
        child.set_parent(&holder);
        holder.imp().child.replace(Some(child.as_ref().clone()));
        holder
    }

    pub fn set_scale(&self, target: f64) {
        let imp = self.imp();
        if imp.target.replace(target) == target {
            return;
        }
        if let Some(previous) = imp.animation.take() {
            previous.pause();
        }
        let weak = self.downgrade();
        let animation = animate_with(Spring::Tight, self, imp.scale.get(), target, move |value| {
            if let Some(holder) = weak.upgrade() {
                holder.imp().scale.set(value);
                holder.queue_allocate();
            }
        });
        imp.animation.replace(Some(animation));
    }
}

/// Switches `stack` to `name` and fades the new child in on the standard
/// spring; a no-op when it is already showing.
pub fn show_child(stack: &gtk::Stack, name: &str) {
    if stack.visible_child_name().as_deref() == Some(name) {
        return;
    }
    stack.set_visible_child_name(name);
    let Some(child) = stack.visible_child() else { return };
    let target = child.downgrade();
    let _ = crate::spring::animate(&child, 0.0, 1.0, move |value| {
        if let Some(child) = target.upgrade() {
            child.set_opacity(value.clamp(0.0, 1.0));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revealed_size_follows_the_spring_and_never_goes_negative() {
        assert_eq!(revealed_size(300, 0.0), 0);
        assert_eq!(revealed_size(300, 0.5), 150);
        assert_eq!(revealed_size(300, 1.0), 300);
        assert_eq!(revealed_size(300, 1.02), 306);
        assert_eq!(revealed_size(300, -0.05), 0);
        assert_eq!(revealed_size(301, 0.5), 151);
    }

    #[test]
    #[ignore = "requires a display; run alone with XDG_DATA_HOME pointing at scratch"]
    fn revealer_opens_to_its_child_and_closes_to_nothing() {
        adw::init().unwrap();
        let child = gtk::Label::new(Some("pane"));
        child.set_size_request(300, 40);
        let revealer = SpringRevealer::with_child(Slide::Left, &child);
        assert_eq!(revealer.measure(gtk::Orientation::Horizontal, -1).1, 0);
        revealer.set_reveal_child(true);
        assert!(revealer.reveals_child() && child.is_child_visible());
        revealer.imp().progress.set(1.0);
        assert_eq!(revealer.measure(gtk::Orientation::Horizontal, -1).1, 300);
        revealer.set_reveal_child(false);
        revealer.imp().progress.set(0.0);
        assert_eq!(revealer.measure(gtk::Orientation::Horizontal, -1).1, 0);
    }

    #[test]
    fn slides_keep_the_entering_edge_in_view() {
        assert_eq!(slide_offset(Slide::Down, 40, 100), -60);
        assert_eq!(slide_offset(Slide::Up, 40, 100), 0);
        assert_eq!(slide_offset(Slide::Left, 120, 300), 0);
        assert_eq!(Slide::Left.axis(), gtk::Orientation::Horizontal);
        assert_eq!(Slide::Down.axis(), gtk::Orientation::Vertical);
    }
}
