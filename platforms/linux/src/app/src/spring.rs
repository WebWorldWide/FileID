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
// animations disabled both land instantly.

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

/// Animate a scalar on `spring`; it begins playing immediately, or jumps to
/// `to` when GTK animations are disabled.
pub fn animate_with<W, F>(spring: Spring, widget: &W, from: f64, to: f64, setter: F) -> adw::SpringAnimation
where
    W: IsA<gtk::Widget>,
    F: Fn(f64) + 'static,
{
    let enabled = animations_enabled();
    if !enabled {
        setter(to);
    }
    let target = adw::CallbackAnimationTarget::new(setter);
    let anim = adw::SpringAnimation::new(widget, from, to, spring.params(), target);
    if enabled {
        anim.play();
    }
    anim
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
}
