// LavaLampBackground — Cairo port of the macOS/Windows signature background
// (`platforms/apple/.../Theme/LavaLampBackground.swift`,
// `platforms/windows/.../Motion/LavaLampBackground.cs`). A `gtk::DrawingArea`
// paints a near-black #141414 base, then three slowly-drifting radial-gradient
// blobs in the canonical recipe — a warm GOLD glow, an ORANGE #FF6600 glow, and
// a large DARK ellipse that mottles the centre darker — redrawn every frame via
// a frame-clock tick callback. The macOS/Windows versions blur their blobs;
// Cairo has no cheap blur, so we reproduce the soft "lava" feel with large
// radial gradients whose alpha falls to zero at the rim — visually equivalent.
//
// Efficiency: tick callbacks only fire while the widget is mapped, so an
// occluded / unmapped window stops animating for free. Three gradient fills per
// frame is trivial GPU/CPU work.

use gtk::prelude::*;
use std::time::Instant;

/// (colour, speedX, speedY, driftX, driftY, radiusFrac, peakAlpha) for one blob.
type Blob = ((f64, f64, f64), f64, f64, f64, f64, f64, f64);

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

        // Each blob: (colour, speedX, speedY, driftX, driftY, radiusFrac, peakAlpha).
        // Speeds/drifts/alphas match the macOS + Windows recipe 1:1 (gold 0.40 /
        // orange 0.30 / dark 0.55), so the motion language + warmth match.
        let blobs: [Blob; 3] = [
            (crate::theme::rgb::GOLD, 0.20, 0.23, 0.30, 0.30, 0.46, 0.44),
            (
                crate::theme::rgb::ORANGE,
                0.15,
                0.18,
                0.40,
                0.40,
                0.38,
                0.34,
            ),
            (crate::theme::rgb::DARK, 0.10, 0.12, 0.20, 0.20, 0.50, 0.35),
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

    // Drive off the frame clock (only ticks while mapped), capped at ~30 fps:
    // the blobs drift too slowly for 60+ fps to be visible, and the full-window
    // Cairo repaint is real CPU on software renderers (WSLg/llvmpipe measured
    // >1.5 cores at display rate — half of that is pure waste).
    let last_draw = std::cell::Cell::new(0i64);
    area.add_tick_callback(move |area, clock| {
        let now = clock.frame_time();
        if now - last_draw.get() >= 33_000 {
            last_draw.set(now);
            area.queue_draw();
        }
        glib::ControlFlow::Continue
    });

    area
}
