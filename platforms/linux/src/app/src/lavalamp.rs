// LavaLampBackground — Cairo port of the macOS signature background
// (`platforms/apple/.../Theme/LavaLampBackground.swift`). A `gtk::DrawingArea`
// paints a near-black base, then four slowly-drifting radial-gradient blobs in
// the brand palette (gold / lavender / cyan / pink), redrawn every frame via a
// frame-clock tick callback. The macOS version blurs its Canvas; Cairo has no
// cheap blur, so we reproduce the soft "lava" feel with large radial gradients
// whose alpha falls to zero at the rim — visually equivalent at this scale.
//
// Efficiency: tick callbacks only fire while the widget is mapped, so an
// occluded / unmapped window stops animating for free. Four gradient fills per
// frame is trivial GPU/CPU work.

use gtk::prelude::*;
use std::time::Instant;

/// Build the animated LavaLamp drawing area. The returned widget is meant to be
/// the bottom layer of a `gtk::Overlay`; it never takes input.
pub fn build() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_hexpand(true);
    area.set_vexpand(true);
    // Background only — let all pointer/keyboard input fall through to the UI
    // layered above it in the Overlay.
    area.set_can_target(false);
    area.set_can_focus(false);

    let start = Instant::now();

    area.set_draw_func(move |_area, cr, width, height| {
        let w = width as f64;
        let h = height as f64;
        let t = start.elapsed().as_secs_f64();

        // Opaque near-black base (macOS Color(white: 0.08)).
        let (br, bg, bb) = crate::theme::rgb::BASE;
        cr.set_source_rgb(br, bg, bb);
        let _ = cr.paint();

        // Each blob: (palette colour, drift speeds, drift extents, radius, peak alpha).
        // Phases/speeds echo the macOS sin/cos drift so the motion language matches.
        let blobs: [((f64, f64, f64), f64, f64, f64, f64, f64, f64); 4] = [
            (crate::theme::rgb::GOLD, 0.20, 0.23, 0.30, 0.30, 0.46, 0.42),
            (crate::theme::rgb::LAVENDER, 0.15, 0.18, 0.40, 0.40, 0.50, 0.34),
            (crate::theme::rgb::CYAN, 0.10, 0.12, 0.20, 0.20, 0.40, 0.30),
            (crate::theme::rgb::PINK, 0.13, 0.17, 0.35, 0.28, 0.44, 0.28),
        ];

        for (i, &((cr_, cg, cb), sx, sy, ex, ey, radf, alpha)) in blobs.iter().enumerate() {
            // Offset each blob's phase so they don't pulse in unison.
            let phase = i as f64 * 1.7;
            let cx = w * 0.5 + (t * sx + phase).sin() * w * ex;
            let cy = h * 0.5 + (t * sy + phase).cos() * h * ey;
            let radius = (w.max(h)) * radf;

            let grad = gtk::cairo::RadialGradient::new(cx, cy, 0.0, cx, cy, radius);
            grad.add_color_stop_rgba(0.0, cr_, cg, cb, alpha);
            grad.add_color_stop_rgba(0.55, cr_, cg, cb, alpha * 0.35);
            grad.add_color_stop_rgba(1.0, cr_, cg, cb, 0.0);
            // `let _ =` is deliberate: it compiles whether `set_source` returns
            // `()` or `Result` across cairo-rs versions.
            let _ = cr.set_source(&grad);
            cr.rectangle(0.0, 0.0, w, h);
            let _ = cr.fill();
        }
    });

    // Drive ~display-refresh via the frame clock; only ticks while mapped.
    area.add_tick_callback(|area, _clock| {
        area.queue_draw();
        glib::ControlFlow::Continue
    });

    area
}
