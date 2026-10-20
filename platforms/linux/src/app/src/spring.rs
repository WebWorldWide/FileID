// Spring motion — maps the macOS SwiftUI spring (`response` / `dampingFraction`)
// onto libadwaita's `adw::SpringAnimation`, the GTK equivalent of the
// Composition `SpringScalarNaturalMotionAnimation` the Windows port uses.
//
//   damping_ratio = dampingFraction
//   mass          = 1
//   stiffness     = (2π / response)² × mass
//
// shared/docs/DESIGN_V3_PORT.md: the tight spring (0.35 / 0.78) drives
// toggles, segments, selection and pills; the standard spring (0.40 / 0.80)
// drives panes, panel insertion, expanding rows and aspect changes. With GTK
// animations disabled the tight spring lands instantly and the standard one
// lands at once behind a 0.15 s ease-out opacity fade.

use adw::prelude::*;
use std::f64::consts::PI;

const MASS: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Spring {
    Tight,
    Standard,
}

impl Spring {
    fn response_and_damping(self) -> (f64, f64) {
        match self {
            Spring::Tight => (0.35, 0.78),
            Spring::Standard => (0.40, 0.80),
        }
    }

    pub fn params(self) -> adw::SpringParams {
        let (response, damping) = self.response_and_damping();
        let stiffness = (2.0 * PI / response).powi(2) * MASS;
        adw::SpringParams::new(damping, MASS, stiffness)
    }
}

pub fn animations_enabled() -> bool {
    gtk::Settings::default().map_or(true, |settings| settings.is_gtk_enable_animations())
}

/// Animate a scalar on the standard spring, handing each value to `setter`.
pub fn animate<W, F>(widget: &W, from: f64, to: f64, setter: F) -> adw::SpringAnimation
where
    W: IsA<gtk::Widget>,
    F: Fn(f64) + 'static,
{
    animate_with(Spring::Standard, widget, from, to, setter)
}

/// Animate a scalar on `spring`; it begins playing immediately. With GTK
/// animations disabled it jumps to `to`, and a standard spring also fades
/// the widget in (Reduce Motion in DESIGN_V3_PORT "Motion").
pub fn animate_with<W, F>(
    spring: Spring,
    widget: &W,
    from: f64,
    to: f64,
    setter: F,
) -> adw::SpringAnimation
where
    W: IsA<gtk::Widget>,
    F: Fn(f64) + 'static,
{
    let enabled = animations_enabled();
    if !enabled {
        setter(to);
        if spring == Spring::Standard {
            reduced_motion_fade(widget.upcast_ref());
        }
    }
    let target = adw::CallbackAnimationTarget::new(setter);
    let anim = adw::SpringAnimation::new(widget, from, to, spring.params(), target);
    if enabled {
        anim.play();
    }
    anim
}

const FADE_MICROS: i64 = 150_000;

/// Ease-out cubic over the reduced-motion fade; 1 once it has elapsed.
pub fn fade_progress(elapsed_micros: i64) -> f64 {
    let t = (elapsed_micros as f64 / FADE_MICROS as f64).clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// Fades `widget` from transparent to its current opacity on the frame clock.
/// libadwaita skips its own animations when GTK animations are off, so this
/// drives the frames itself. Unmapped widgets are left alone: their tick
/// callback would never run and they would stay invisible.
fn reduced_motion_fade(widget: &gtk::Widget) {
    if !widget.is_mapped() {
        return;
    }
    let target = widget.opacity();
    let start = std::cell::Cell::new(None::<i64>);
    widget.set_opacity(0.0);
    widget.add_tick_callback(move |widget, clock| {
        let now = clock.frame_time();
        let begun = start.get().unwrap_or(now);
        start.set(Some(begun));
        let progress = fade_progress(now - begun);
        widget.set_opacity(target * progress);
        if progress >= 1.0 {
            gtk::glib::ControlFlow::Break
        } else {
            gtk::glib::ControlFlow::Continue
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn springs_map_response_to_stiffness() {
        let (response, damping) = Spring::Tight.response_and_damping();
        assert_eq!((response, damping), (0.35, 0.78));
        assert_eq!(Spring::Standard.response_and_damping(), (0.40, 0.80));
    }

    #[test]
    fn the_reduced_motion_fade_eases_out_over_150_ms() {
        assert_eq!(fade_progress(0), 0.0);
        assert!((fade_progress(75_000) - 0.875).abs() < 1e-9);
        assert_eq!(fade_progress(150_000), 1.0);
        assert_eq!(fade_progress(400_000), 1.0);
        assert_eq!(fade_progress(-5), 0.0);
    }
}
