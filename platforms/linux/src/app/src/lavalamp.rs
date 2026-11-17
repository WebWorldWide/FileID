// LavaLampBackground — port of the macOS/Windows signature background
// (`platforms/apple/.../Theme/LavaLampBackground.swift`,
// `platforms/windows/.../Motion/LavaLampBackground.cs`): a near-black #141414
// base and three slowly drifting glows in the canonical recipe — warm GOLD,
// ORANGE #FF6600 and a large DARK one that mottles the centre.
//
// The glows are GSK radial-gradient nodes, so the renderer draws them on the
// GPU; nothing is rasterised on the CPU per frame. The lamp advances at most
// 20 times a second (the Mac cap), and its frame-clock callback exists only
// while it can be seen moving: the window is on screen, active and not
// suspended, the lamp is on, and neither Reduce Motion nor high contrast is
// set. Otherwise it holds its last frame and costs nothing.

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(true);

thread_local! {
    static LAMP: RefCell<glib::WeakRef<LavaLamp>> = RefCell::default();
}

/// Settings ▸ Appearance ▸ Lava lamp: Full (true) or Off (flat base colour).
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if let Some(lamp) = LAMP.with_borrow(glib::WeakRef::upgrade) {
        lamp.sync();
        lamp.queue_draw();
    }
}

pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The Mac app's cap: the glows drift too slowly for more to be visible.
const FRAME_MICROS: i64 = 50_000;

struct Blob {
    colour: (f64, f64, f64),
    speed: (f64, f64),
    drift: (f64, f64),
    radius: f64,
    alpha: f64,
}

/// Speeds, drifts and alphas match the macOS + Windows recipe 1:1.
const BLOBS: [Blob; 3] = [
    Blob { colour: crate::theme::rgb::GOLD, speed: (0.20, 0.23), drift: (0.30, 0.30), radius: 0.46, alpha: 0.44 },
    Blob { colour: crate::theme::rgb::ORANGE, speed: (0.15, 0.18), drift: (0.40, 0.40), radius: 0.38, alpha: 0.34 },
    Blob { colour: crate::theme::rgb::DARK, speed: (0.10, 0.12), drift: (0.20, 0.20), radius: 0.50, alpha: 0.35 },
];

/// Centre and radius of blob `index` at lamp time `t` in a `w`×`h` window.
/// Each blob's phase is offset so they never pulse in unison.
fn blob_frame(index: usize, t: f64, w: f64, h: f64) -> (f64, f64, f64) {
    let blob = &BLOBS[index];
    let phase = index as f64 * 1.7;
    (
        w * 0.5 + (t * blob.speed.0 + phase).sin() * w * blob.drift.0,
        h * 0.5 + (t * blob.speed.1 + phase).cos() * h * blob.drift.1,
        w.max(h) * blob.radius,
    )
}

#[derive(Clone, Copy)]
struct Conditions {
    enabled: bool,
    animations: bool,
    high_contrast: bool,
    mapped: bool,
    active: bool,
    suspended: bool,
}

/// Whether the lamp should be moving right now.
fn should_animate(c: Conditions) -> bool {
    c.enabled && c.animations && !c.high_contrast && c.mapped && c.active && !c.suspended
}

/// Lamp time after a frame at `now`: it advances by the real time since the
/// last frame, but a long gap (the lamp was paused) is not caught up, so the
/// glows resume from where they stopped instead of jumping.
fn advance(time: f64, last: Option<i64>, now: i64) -> f64 {
    match last {
        Some(last) if now > last => time + ((now - last) as f64 / 1_000_000.0).min(0.25),
        _ => time,
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct LavaLamp {
        pub time: Cell<f64>,
        pub last_frame: Cell<Option<i64>>,
        pub tick: RefCell<Option<gtk::TickCallbackId>>,
        pub window: RefCell<Option<(gtk::Window, Vec<glib::SignalHandlerId>)>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LavaLamp {
        const NAME: &'static str = "FileIDLavaLamp";
        type Type = super::LavaLamp;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for LavaLamp {
        fn dispose(&self) {
            self.obj().stop();
            self.obj().unwatch_window();
        }
    }

    impl WidgetImpl for LavaLamp {
        fn map(&self) {
            self.parent_map();
            self.obj().watch_window();
            self.obj().sync();
        }

        fn unmap(&self) {
            self.obj().stop();
            self.obj().unwatch_window();
            self.parent_unmap();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let (w, h) = (f64::from(widget.width()), f64::from(widget.height()));
            if w <= 0.0 || h <= 0.0 {
                return;
            }
            let bounds = gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
            let (r, g, b) = crate::theme::rgb::BASE;
            snapshot.append_color(&gtk::gdk::RGBA::new(r as f32, g as f32, b as f32, 1.0), &bounds);
            if !is_enabled() {
                return;
            }
            let t = self.time.get();
            for (index, blob) in BLOBS.iter().enumerate() {
                let (cx, cy, radius) = blob_frame(index, t, w, h);
                let (r, g, b) = blob.colour;
                let stop = |offset: f32, alpha: f64| {
                    gtk::gsk::ColorStop::new(offset, gtk::gdk::RGBA::new(r as f32, g as f32, b as f32, alpha as f32))
                };
                snapshot.append_radial_gradient(
                    &bounds,
                    &gtk::graphene::Point::new(cx as f32, cy as f32),
                    radius as f32,
                    radius as f32,
                    0.0,
                    1.0,
                    &[stop(0.0, blob.alpha), stop(0.55, blob.alpha * 0.35), stop(1.0, 0.0)],
                );
            }
        }
    }
}

glib::wrapper! {
    pub struct LavaLamp(ObjectSubclass<imp::LavaLamp>) @extends gtk::Widget;
}

impl LavaLamp {
    fn conditions(&self) -> Conditions {
        let window = self.root().and_downcast::<gtk::Window>();
        Conditions {
            enabled: is_enabled(),
            animations: gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations()),
            high_contrast: crate::theme::high_contrast(),
            mapped: self.is_mapped(),
            active: window.as_ref().is_none_or(|window| window.is_active()),
            suspended: window.as_ref().is_some_and(|window| window.is_suspended()),
        }
    }

    /// Starts or stops the frame callback to match what can be seen.
    fn sync(&self) {
        if should_animate(self.conditions()) {
            self.start();
        } else {
            self.stop();
        }
    }

    fn start(&self) {
        let imp = self.imp();
        if imp.tick.borrow().is_some() {
            return;
        }
        imp.last_frame.set(None);
        let id = self.add_tick_callback(|lamp, clock| {
            let imp = lamp.imp();
            let now = clock.frame_time();
            if imp.last_frame.get().is_none_or(|last| now - last >= FRAME_MICROS) {
                imp.time.set(advance(imp.time.get(), imp.last_frame.get(), now));
                imp.last_frame.set(Some(now));
                lamp.queue_draw();
            }
            glib::ControlFlow::Continue
        });
        imp.tick.replace(Some(id));
    }

    fn stop(&self) {
        if let Some(id) = self.imp().tick.take() {
            id.remove();
        }
    }

    fn watch_window(&self) {
        self.unwatch_window();
        let Some(window) = self.root().and_downcast::<gtk::Window>() else { return };
        let weak = self.downgrade();
        let on_change = move || {
            if let Some(lamp) = weak.upgrade() {
                lamp.sync();
            }
        };
        let handlers = vec![
            window.connect_is_active_notify({
                let on_change = on_change.clone();
                move |_| on_change()
            }),
            window.connect_suspended_notify(move |_| on_change()),
        ];
        self.imp().window.replace(Some((window, handlers)));
    }

    fn unwatch_window(&self) {
        if let Some((window, handlers)) = self.imp().window.take() {
            for handler in handlers {
                window.disconnect(handler);
            }
        }
    }
}

/// The lamp, meant as the bottom layer of a `gtk::Overlay`; it never takes input.
pub fn build() -> LavaLamp {
    ENABLED.store(crate::app_settings::lava_lamp_on(), Ordering::Relaxed);
    let lamp: LavaLamp = glib::Object::new();
    lamp.set_hexpand(true);
    lamp.set_vexpand(true);
    lamp.set_can_target(false);
    lamp.set_can_focus(false);
    LAMP.with_borrow_mut(|slot| slot.set(Some(&lamp)));

    // Reduce Motion and high contrast are read when they change, not per frame.
    let weak = lamp.downgrade();
    let on_setting = move || {
        if let Some(lamp) = weak.upgrade() {
            lamp.sync();
            lamp.queue_draw();
        }
    };
    if let Some(settings) = gtk::Settings::default() {
        let on_setting = on_setting.clone();
        settings.connect_gtk_enable_animations_notify(move |_| on_setting());
    }
    adw::StyleManager::default().connect_high_contrast_notify(move |_| on_setting());
    lamp
}

/// The system "Reduce Motion" preference (`gtk-enable-animations` off).
pub fn reduce_motion() -> bool {
    !gtk::Settings::default().is_none_or(|settings| settings.is_gtk_enable_animations())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VISIBLE: Conditions =
        Conditions { enabled: true, animations: true, high_contrast: false, mapped: true, active: true, suspended: false };

    #[test]
    fn the_lamp_moves_only_when_it_can_be_seen_moving() {
        assert!(should_animate(VISIBLE));
        for still in [
            Conditions { enabled: false, ..VISIBLE },
            Conditions { animations: false, ..VISIBLE },
            Conditions { high_contrast: true, ..VISIBLE },
            Conditions { mapped: false, ..VISIBLE },
            Conditions { active: false, ..VISIBLE },
            Conditions { suspended: true, ..VISIBLE },
        ] {
            assert!(!should_animate(still));
        }
    }

    #[test]
    fn a_paused_lamp_resumes_where_it_stopped() {
        assert_eq!(advance(3.0, None, 9_000_000), 3.0);
        assert!((advance(3.0, Some(1_000_000), 1_050_000) - 3.05).abs() < 1e-9);
        assert!((advance(3.0, Some(1_000_000), 900_000_000) - 3.25).abs() < 1e-9);
        assert_eq!(advance(3.0, Some(5), 5), 3.0);
    }

    #[test]
    fn the_blobs_keep_the_reference_recipe() {
        let (cx, cy, radius) = blob_frame(0, 0.0, 1000.0, 500.0);
        assert!((cx - 500.0).abs() < 1e-9);
        assert!((cy - (250.0 + 500.0 * 0.30)).abs() < 1e-9);
        assert!((radius - 460.0).abs() < 1e-9);
        let (alphas, radii): (Vec<f64>, Vec<f64>) = BLOBS.iter().map(|blob| (blob.alpha, blob.radius)).unzip();
        assert_eq!(alphas, [0.44, 0.34, 0.35]);
        assert_eq!(radii, [0.46, 0.38, 0.50]);
        for index in 0..3 {
            for step in 0..200 {
                let (cx, cy, _) = blob_frame(index, f64::from(step) * 0.7, 1000.0, 500.0);
                assert!((0.0..=1000.0).contains(&cx) && (0.0..=500.0).contains(&cy));
            }
        }
    }
}
