/* FileID landing — LavaLamp canvas background + scroll reveal.
   Vanilla JS, no dependencies. Mirrors the app's drifting-blob motion:
   near-black base + soft radial-gradient blobs (gold/lavender/cyan/pink)
   that drift on sin/cos paths, composited additively. */

(function () {
  "use strict";

  const reduceMotion =
    window.matchMedia &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;

  /* -------- LavaLamp background -------- */
  const canvas = document.getElementById("lavalamp");
  if (canvas && canvas.getContext) {
    const ctx = canvas.getContext("2d", { alpha: false });

    // Each blob: base color, radius factor, and two drift frequencies/phases.
    // Frequencies echo the macOS LavaLampBackground (≈0.1–0.23).
    const blobs = [
      { color: [255, 204, 0], r: 0.55, fx: 0.020, fy: 0.023, ax: 0.30, ay: 0.30, px: 0.0, py: 1.1, a: 0.42 },
      { color: [242, 166, 192], r: 0.42, fx: 0.015, fy: 0.018, ax: 0.40, ay: 0.36, px: 2.2, py: 0.4, a: 0.34 },
      { color: [160, 226, 234], r: 0.46, fx: 0.018, fy: 0.013, ax: 0.34, ay: 0.40, px: 4.0, py: 3.1, a: 0.30 },
      { color: [177, 155, 206], r: 0.50, fx: 0.011, fy: 0.016, ax: 0.26, ay: 0.30, px: 1.0, py: 5.2, a: 0.34 }
    ];

    let w = 0, h = 0, dpr = 1, running = true, rafId = null;

    function resize() {
      dpr = Math.min(window.devicePixelRatio || 1, 1.6);
      w = window.innerWidth;
      h = window.innerHeight;
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      canvas.style.width = w + "px";
      canvas.style.height = h + "px";
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    }

    function drawBlob(b, t) {
      const cx = w * 0.5 + Math.sin(t * b.fx + b.px) * w * b.ax;
      const cy = h * 0.5 + Math.cos(t * b.fy + b.py) * h * b.ay;
      const rad = Math.max(w, h) * b.r;
      const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, rad);
      const [r, gr, bl] = b.color;
      g.addColorStop(0, "rgba(" + r + "," + gr + "," + bl + "," + b.a + ")");
      g.addColorStop(0.55, "rgba(" + r + "," + gr + "," + bl + "," + b.a * 0.28 + ")");
      g.addColorStop(1, "rgba(" + r + "," + gr + "," + bl + ",0)");
      ctx.fillStyle = g;
      ctx.fillRect(0, 0, w, h);
    }

    function render(t) {
      // near-black base (matches Color(white: 0.08) in the app)
      ctx.fillStyle = "#0b0b10";
      ctx.fillRect(0, 0, w, h);
      ctx.globalCompositeOperation = "lighter";
      for (let i = 0; i < blobs.length; i++) drawBlob(blobs[i], t);
      ctx.globalCompositeOperation = "source-over";
    }

    function frame(now) {
      if (!running) return;
      render(now * 0.06); // scale ms → gentle drift
      rafId = requestAnimationFrame(frame);
    }

    resize();
    window.addEventListener("resize", resize, { passive: true });

    if (reduceMotion) {
      render(1200); // single static, pleasant frame
    } else {
      rafId = requestAnimationFrame(frame);
      // Pause when the tab is hidden — no wasted cycles.
      document.addEventListener("visibilitychange", function () {
        if (document.hidden) {
          running = false;
          if (rafId) cancelAnimationFrame(rafId);
        } else if (!running) {
          running = true;
          rafId = requestAnimationFrame(frame);
        }
      });
    }
  }

  /* -------- Scroll reveal -------- */
  const revealEls = document.querySelectorAll(".reveal");
  if (reduceMotion || !("IntersectionObserver" in window)) {
    revealEls.forEach((el) => el.classList.add("in"));
  } else {
    const io = new IntersectionObserver(
      function (entries) {
        entries.forEach(function (entry) {
          if (entry.isIntersecting) {
            entry.target.classList.add("in");
            io.unobserve(entry.target);
          }
        });
      },
      { rootMargin: "0px 0px -10% 0px", threshold: 0.08 }
    );
    revealEls.forEach((el) => io.observe(el));
  }
})();
