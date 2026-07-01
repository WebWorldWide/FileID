// Spring motion — maps the macOS SwiftUI spring (`response` / `dampingFraction`)
// onto libadwaita's `adw::SpringAnimation`, the GTK equivalent of the
// Composition `SpringScalarNaturalMotionAnimation` the Windows port uses.
//
// macOS uses response 0.35–0.40 / dampingFraction 0.78–0.80 everywhere. The
// conversion (per `platforms/linux/CLAUDE.md`):
//
//   damping_ratio = dampingFraction
//   mass          = 1
//   stiffness     = (2π / response)² × mass
//
// so a SwiftUI `.spring(response: 0.38, dampingFraction: 0.79)` becomes
// `SpringParams::new(0.79, 1.0, (2π/0.38)²)`.

use adw::prelude::*;
use std::f64::consts::PI;

const RESPONSE: f64 = 0.38;
const DAMPING: f64 = 0.79;
const MASS: f64 = 1.0;

/// The shared brand spring. Reused for preview reveals and any springy
/// property animation so the whole app moves with one motion signature.
pub fn brand_params() -> adw::SpringParams {
    let stiffness = (2.0 * PI / RESPONSE).powi(2) * MASS;
    adw::SpringParams::new(DAMPING, MASS, stiffness)
}

/// Animate a scalar from `from`→`to` on the brand spring, handing each value to
/// `setter` (e.g. `widget.set_opacity`). Returns the running animation so the
/// caller can keep it alive / restart it; it begins playing immediately.
pub fn animate<W, F>(widget: &W, from: f64, to: f64, setter: F) -> adw::SpringAnimation
where
    W: IsA<gtk::Widget>,
    F: Fn(f64) + 'static,
{
    let target = adw::CallbackAnimationTarget::new(setter);
    let anim = adw::SpringAnimation::new(widget, from, to, brand_params(), target);
    anim.play();
    anim
}
