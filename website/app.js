/* FileID — website interactions (Prototype v3)
   - LavaLamp v3: warm wax glows rising behind the hero, lit from below
   - Extruded app icon with drag-to-spin
   - Scroll reveals */

(() => {
'use strict';

const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches || /[?&]static=1/.test(location.search);
if (/[?&]static=1/.test(location.search)) document.documentElement.classList.add('static');

/* LavaLamp v3 — six blurred wax blobs that rise and sway over a warm base. */
function initLavaLamp() {
  const canvas = document.getElementById('lavalamp');
  if (!canvas) return;
  const ctx = canvas.getContext('2d', { alpha: false });

  const blobs = [
    { x: -0.08, s: 560, rise: 64, sway: 19, d: -10, c: [255, 166, 36], o: 0.85 },
    { x: 0.16, s: 420, rise: 52, sway: 15, d: -33, c: [255, 112, 24], o: 0.8 },
    { x: 0.36, s: 620, rise: 72, sway: 21, d: -48, c: [255, 196, 40], o: 0.7 },
    { x: 0.56, s: 380, rise: 46, sway: 13, d: -18, c: [232, 72, 14], o: 0.75 },
    { x: 0.68, s: 520, rise: 60, sway: 17, d: -55, c: [255, 140, 28], o: 0.8 },
    { x: 0.84, s: 440, rise: 56, sway: 16, d: -27, c: [255, 186, 52], o: 0.7 }
  ];

  let w = 0, h = 0, dpr = 1;
  function resize() {
    dpr = Math.min(window.devicePixelRatio || 1, 1.5);
    const r = canvas.getBoundingClientRect();
    w = r.width; h = r.height;
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  resize();
  window.addEventListener('resize', resize);

  let running = true, visible = !document.hidden, lastT = performance.now(), t = 0;
  document.addEventListener('visibilitychange', () => { visible = !document.hidden; if (visible) { lastT = performance.now(); requestAnimationFrame(tick); } });
  const io = new IntersectionObserver((entries) => { entries.forEach(e => { running = e.isIntersecting; }); if (running) { lastT = performance.now(); requestAnimationFrame(tick); } }, { threshold: 0.01 });
  io.observe(canvas.parentElement);

  function base() {
    const g = ctx.createLinearGradient(0, 0, 0, h);
    g.addColorStop(0, '#0B0B0C'); g.addColorStop(0.42, '#0F0A08'); g.addColorStop(0.78, '#1B0D06'); g.addColorStop(1, '#2A1206');
    ctx.fillStyle = g; ctx.fillRect(0, 0, w, h);
    const glow = ctx.createRadialGradient(w / 2, h * 1.15, 0, w / 2, h * 1.15, Math.max(w, h) * 0.75);
    glow.addColorStop(0, 'rgba(255,128,32,.55)'); glow.addColorStop(0.38, 'rgba(214,74,14,.32)'); glow.addColorStop(0.68, 'rgba(120,34,8,.14)'); glow.addColorStop(1, 'rgba(0,0,0,0)');
    ctx.fillStyle = glow; ctx.fillRect(0, 0, w, h);
  }

  function frame() {
    base();
    ctx.save();
    ctx.globalCompositeOperation = 'screen';
    ctx.filter = 'blur(28px)';
    for (const b of blobs) {
      const tt = t + b.d;
      const rise = Math.sin((tt / b.rise) * Math.PI * 2) * 0.5 + 0.5;
      const sway = Math.sin((tt / b.sway) * Math.PI * 2);
      const r = b.s / 2;
      const cx = b.x * w + r + sway * 16;
      const cy = h + b.s * 0.1 - rise * h * 0.8;
      const g = ctx.createRadialGradient(cx, cy, 0, cx, cy, r * 1.1);
      const [cr, cg, cb] = b.c;
      g.addColorStop(0, `rgba(${cr},${cg},${cb},${0.9 * b.o})`);
      g.addColorStop(0.22, `rgba(${cr},${cg},${cb},${0.62 * b.o})`);
      g.addColorStop(0.48, `rgba(${cr},${cg},${cb},${0.32 * b.o})`);
      g.addColorStop(0.74, `rgba(${cr},${cg},${cb},${0.1 * b.o})`);
      g.addColorStop(1, `rgba(${cr},${cg},${cb},0)`);
      ctx.fillStyle = g;
      ctx.beginPath(); ctx.ellipse(cx, cy, r, r * 1.1, 0, 0, Math.PI * 2); ctx.fill();
    }
    ctx.restore();
  }

  t = 20; frame();
  if (reduceMotion) return;
  function tick(now) {
    if (!running || !visible) return;
    const dt = Math.min(0.05, (now - lastT) / 1000);
    lastT = now; t += dt;
    frame();
    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
}

/* Extruded app icon with drag-to-spin. */
function init3DLogo() {
  const root = document.getElementById('logo3d');
  const stage = document.getElementById('logoStage');
  const hint = document.getElementById('logoHint');
  if (!root || !stage) return;

  const LAYERS = 22, DEPTH = 44, half = DEPTH / 2;
  const frag = document.createDocumentFragment();
  for (let i = 0; i < LAYERS; i++) {
    const tt = i / (LAYERS - 1), isFace = i === 0 || i === LAYERS - 1;
    const el = document.createElement('div');
    el.className = 'l-layer';
    el.style.transform = `translateZ(${(-half + tt * DEPTH).toFixed(2)}px)`;
    el.style.filter = isFace ? 'none' : `brightness(${(0.42 + Math.abs(tt - 0.5) * 0.6).toFixed(3)}) saturate(.85)`;
    frag.appendChild(el);
  }
  root.appendChild(frag);

  let ry = -18, rx = -8, vry = reduceMotion ? 0 : 12, vrx = 0, dragging = false, lastX = 0, lastY = 0, lastT = 0, touched = false, touchedAt = 0;
  const apply = () => { rx = Math.max(-60, Math.min(60, rx)); root.style.transform = `rotateX(${rx}deg) rotateY(${ry}deg)`; };
  apply();

  stage.addEventListener('pointerdown', (e) => {
    dragging = true; touched = true; touchedAt = performance.now();
    lastX = e.clientX; lastY = e.clientY; lastT = performance.now(); vry = 0; vrx = 0;
    stage.setPointerCapture?.(e.pointerId); root.classList.add('no-trans'); hint?.classList.add('hidden');
  });
  window.addEventListener('pointermove', (e) => {
    if (!dragging) return;
    const now = performance.now(), dt = Math.max(8, now - lastT), dx = e.clientX - lastX, dy = e.clientY - lastY;
    ry += dx * 0.5; rx -= dy * 0.35; apply();
    vry = (dx * 0.5) / (dt / 1000); vrx = -(dy * 0.35) / (dt / 1000);
    lastX = e.clientX; lastY = e.clientY; lastT = now;
  });
  const end = () => { if (!dragging) return; dragging = false; vry = Math.max(-1400, Math.min(1400, vry)); vrx = Math.max(-700, Math.min(700, vrx)); };
  window.addEventListener('pointerup', end); window.addEventListener('pointercancel', end);

  let lastFrame = performance.now();
  function loop(now) {
    const dt = Math.min(0.06, (now - lastFrame) / 1000); lastFrame = now;
    if (!dragging) {
      const drag = Math.pow(0.96, dt * 60); vry *= drag; vrx *= drag; ry += vry * dt; rx += vrx * dt;
      const idle = touched && (performance.now() - touchedAt) > 2200;
      if (!touched || idle) {
        if (!reduceMotion) vry += (12 - vry) * Math.min(1, dt * 0.4);
        rx += (-8 - rx) * Math.min(1, dt * 0.9);
      }
      apply();
    }
    requestAnimationFrame(loop);
  }
  requestAnimationFrame(loop);
  setTimeout(() => { if (!touched) hint?.classList.add('hidden'); }, 4200);
}

/* Scroll reveals. */
function initReveals() {
  const targets = document.querySelectorAll('.reveal, .reveal-stagger');
  if (reduceMotion || !('IntersectionObserver' in window)) { targets.forEach(el => el.classList.add('in')); return; }
  const io = new IntersectionObserver((entries) => { entries.forEach(e => { if (e.isIntersecting) { e.target.classList.add('in'); io.unobserve(e.target); } }); }, { rootMargin: '0px 0px -10% 0px', threshold: 0.05 });
  const recheck = () => {
    const vh = window.innerHeight || document.documentElement.clientHeight;
    targets.forEach(el => { if (!el.classList.contains('in') && el.getBoundingClientRect().top < vh + 80) { el.classList.add('in'); io.unobserve(el); } });
  };
  targets.forEach(el => io.observe(el));
  recheck();
  window.addEventListener('scroll', recheck, { passive: true });
  window.addEventListener('hashchange', recheck);
  setTimeout(recheck, 600);
}

function boot() { initLavaLamp(); init3DLogo(); initReveals(); }
if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', boot); else boot();
})();
