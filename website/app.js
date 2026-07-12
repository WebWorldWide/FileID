/* ──────────────────────────────────────────────────────────────
   FileID — landing page interactions
   - LavaLamp canvas (three drifting ellipses, per VL doc)
   - 3D extruded logo (multi-layer Z stack) with drag-to-spin
   - Sparkles + idle auto-rotation
   - Live GitHub releases fetch with graceful fallback
   ────────────────────────────────────────────────────────────── */

(() => {
'use strict';

const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;

/* ───────────────────────────────────────────────
   1) LAVALAMP — three drifting blurred ellipses
   Per shared/docs/VISUAL-LANGUAGE.md
   ─────────────────────────────────────────────── */
function initLavaLamp() {
  const canvas = document.getElementById('lavalamp');
  if (!canvas) return;
  const ctx = canvas.getContext('2d', { alpha: true });

  // Per the VL doc — keep these honest to the spec
  const blobs = [
    { d: 800,  color: '#FFCC00', alpha: 0.40, xr: 0.20, yr: 0.23, xa: 0.30, ya: 0.30 },
    { d: 600,  color: '#FF6600', alpha: 0.30, xr: 0.15, yr: 0.18, xa: 0.40, ya: 0.40 },
    { d: 1000, color: '#0D0D0D', alpha: 1.00, xr: 0.10, yr: 0.12, xa: 0.20, ya: 0.20 },
  ];

  let w = 0, h = 0, dpr = 1;
  function resize() {
    dpr = Math.min(window.devicePixelRatio || 1, 1.5);  // cap DPR — blob is huge & blurred, no need for 3x
    const r = canvas.getBoundingClientRect();
    w = r.width; h = r.height;
    canvas.width = Math.round(w * dpr);
    canvas.height = Math.round(h * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }
  resize();
  window.addEventListener('resize', resize);

  let lastT = performance.now();
  let timeSec = 0;
  let running = true;
  let visible = !document.hidden;

  document.addEventListener('visibilitychange', () => {
    visible = !document.hidden;
    if (visible) { lastT = performance.now(); requestAnimationFrame(tick); }
  });

  // Pause when hero is offscreen
  const heroEl = canvas.parentElement;
  const io = new IntersectionObserver((entries) => {
    entries.forEach(e => { running = e.isIntersecting; });
    if (running) { lastT = performance.now(); requestAnimationFrame(tick); }
  }, { threshold: 0.01 });
  io.observe(heroEl);

  const rate = reduceMotion ? 0.5 : 1;  // half rate under reduced motion (per VL doc)

  function tick(now) {
    if (!running || !visible) return;
    const dt = Math.min(0.05, (now - lastT) / 1000);
    lastT = now;
    timeSec += dt * rate;

    // Background: solid #141414
    ctx.globalCompositeOperation = 'source-over';
    ctx.fillStyle = '#141414';
    ctx.fillRect(0, 0, w, h);

    // Three blurred ellipses — CSS filter blur(110px) on canvas does the blur for free
    for (const b of blobs) {
      const cx = w / 2 + Math.sin(timeSec * b.xr) * w * b.xa;
      const cy = h / 2 + Math.cos(timeSec * b.yr) * h * b.ya;
      const rx = b.d / 2;
      const ry = b.d / 2;

      ctx.globalAlpha = b.alpha;
      ctx.fillStyle = b.color;
      ctx.beginPath();
      ctx.ellipse(cx, cy, rx, ry, 0, 0, Math.PI * 2);
      ctx.fill();
    }
    ctx.globalAlpha = 1;

    requestAnimationFrame(tick);
  }
  requestAnimationFrame(tick);
}

/* ───────────────────────────────────────────────
   2) 3D LOGO — extruded icon with drag-to-spin
   ─────────────────────────────────────────────── */
function init3DLogo() {
  const root = document.getElementById('logo3d');
  const stage = document.getElementById('logoStage');
  const hint = document.getElementById('logoHint');
  if (!root || !stage) return;

  // Build extruded layers — same icon at incrementing Z offsets.
  // Front + back are bright, middle layers are progressively darkened
  // to give a "metallic edge" look during rotation.
  const LAYERS = 26;
  const DEPTH  = 56;  // total px depth (front to back)
  const halfDepth = DEPTH / 2;
  const frag = document.createDocumentFragment();
  for (let i = 0; i < LAYERS; i++) {
    const t = i / (LAYERS - 1);            // 0 .. 1
    const z = -halfDepth + t * DEPTH;
    // Brightness curve: bright at the two faces, darker in the middle
    const edgeProx = Math.abs(t - 0.5) * 2;          // 1 at faces, 0 in middle
    const isFace = (i === 0 || i === LAYERS - 1);
    const brightness = isFace ? 1 : 0.42 + edgeProx * 0.30;
    const contrast   = isFace ? 1 : 1.08;
    const saturate   = isFace ? 1 : 0.85;

    const el = document.createElement('div');
    el.className = 'l-layer';
    el.style.transform = `translateZ(${z.toFixed(2)}px)`;
    el.style.filter = `brightness(${brightness.toFixed(3)}) contrast(${contrast}) saturate(${saturate})`;
    // Edge layers don't need full opacity — slight stacking gives a richer edge
    if (!isFace) el.style.opacity = '0.96';
    frag.appendChild(el);
  }
  root.appendChild(frag);

  // Spin state
  let ry = -18;                 // current Y rotation (deg)
  let rx = -8;                  // current X rotation (deg)
  let vry = reduceMotion ? 0 : 14;  // angular velocity (deg/sec) — gentle auto-spin
  let vrx = 0;
  let dragging = false;
  let lastX = 0, lastY = 0, lastT = 0;
  let userTouched = false;
  let touchedAt = 0;

  function apply() {
    // Clamp x so it never flips, allow y to roam freely
    if (rx >  60) rx =  60;
    if (rx < -60) rx = -60;
    root.style.setProperty('--rx', rx.toFixed(2) + 'deg');
    root.style.setProperty('--ry', ry.toFixed(2) + 'deg');
    root.style.transform = `rotateX(${rx}deg) rotateY(${ry}deg)`;
  }
  apply();

  // Pointer events (covers mouse + touch + pen)
  stage.addEventListener('pointerdown', (e) => {
    dragging = true;
    userTouched = true;
    touchedAt = performance.now();
    lastX = e.clientX;
    lastY = e.clientY;
    lastT = performance.now();
    vry = 0; vrx = 0;
    stage.setPointerCapture?.(e.pointerId);
    root.classList.add('no-trans');
    if (hint) hint.classList.add('hidden');
  });

  window.addEventListener('pointermove', (e) => {
    if (!dragging) return;
    const now = performance.now();
    const dt  = Math.max(8, now - lastT);  // floor at 8ms to avoid divide-by-zero spikes
    const dx  = e.clientX - lastX;
    const dy  = e.clientY - lastY;

    const sensX = 0.5;   // deg per px horizontally (Y rotation)
    const sensY = 0.35;  // deg per px vertically   (X rotation)

    ry += dx * sensX;
    rx -= dy * sensY;
    apply();

    // Track velocity for flick-release
    vry = (dx * sensX) / (dt / 1000);
    vrx = -(dy * sensY) / (dt / 1000);

    lastX = e.clientX;
    lastY = e.clientY;
    lastT = now;
  });

  function endDrag() {
    if (!dragging) return;
    dragging = false;
    // Cap the launch velocity so a wild flick doesn't blur for a year
    vry = Math.max(-1400, Math.min(1400, vry));
    vrx = Math.max(-700,  Math.min(700,  vrx));
  }
  window.addEventListener('pointerup',     endDrag);
  window.addEventListener('pointercancel', endDrag);
  window.addEventListener('pointerleave',  endDrag);

  // Animation loop — applies velocity decay + gentle pull back to a base tilt when idle
  let lastFrame = performance.now();
  function loop(now) {
    const dt = Math.min(0.06, (now - lastFrame) / 1000);
    lastFrame = now;

    if (!dragging) {
      // Velocity decay (air resistance)
      const drag = Math.pow(0.96, dt * 60);
      vry *= drag;
      vrx *= drag;

      ry += vry * dt;
      rx += vrx * dt;

      // Idle: after user hasn't touched in 2s and velocity is low, kick auto-spin
      const idle = userTouched && (performance.now() - touchedAt) > 2200;
      if (!userTouched || idle) {
        if (!reduceMotion) {
          // Pull toward gentle auto-spin
          const targetVry = 14;
          vry += (targetVry - vry) * Math.min(1, dt * 0.4);
        }
        // Gentle pull X back to base tilt
        const targetRx = -8;
        rx += (targetRx - rx) * Math.min(1, dt * 0.9);
      }

      apply();
    }
    requestAnimationFrame(loop);
  }
  requestAnimationFrame(loop);

  // Hide hint after first 4s if not touched
  setTimeout(() => { if (!userTouched && hint) hint.classList.add('hidden'); }, 4200);

  // Light parallax from cursor on the whole stage (only when not dragged + not touched)
  document.addEventListener('mousemove', (e) => {
    if (dragging || userTouched) return;
    const r = stage.getBoundingClientRect();
    const cx = r.left + r.width  / 2;
    const cy = r.top  + r.height / 2;
    const nx = (e.clientX - cx) / window.innerWidth;
    const ny = (e.clientY - cy) / window.innerHeight;
    // very subtle nudge
    const targetRy = -18 + nx * 14;
    const targetRx = -8  - ny * 8;
    ry += (targetRy - ry) * 0.04;
    rx += (targetRx - rx) * 0.04;
  });

  /* Sparkles — randomly placed crosses around the logo */
  const sparkleHost = document.getElementById('logoSparkles');
  if (sparkleHost && !reduceMotion) {
    const count = 6;
    for (let i = 0; i < count; i++) {
      const s = document.createElement('span');
      s.className = 'sparkle';
      const angle = (i / count) * Math.PI * 2 + Math.random() * 0.6;
      const dist  = 130 + Math.random() * 70;
      const x = 50 + Math.cos(angle) * dist / 4;
      const y = 50 + Math.sin(angle) * dist / 4;
      s.style.left = x + '%';
      s.style.top  = y + '%';
      const size = 6 + Math.random() * 8;
      s.style.width = s.style.height = size + 'px';
      s.style.animationDuration = (1.8 + Math.random() * 2.4) + 's';
      s.style.animationDelay = (Math.random() * 4) + 's';
      sparkleHost.appendChild(s);
    }
  }
}

/* ───────────────────────────────────────────────
   3) LIVE GITHUB RELEASES — graceful fallback
   ─────────────────────────────────────────────── */
async function initReleases() {
  const status = document.getElementById('releaseStatus');
  if (!status) return;

  // Asset name → CSS selector → download target patterns we'll look for
  const patterns = {
    'setup-exe':  /^FileIDSetup.*\.exe$/i,
    'x64-msi':    /(FileID|FileID-)x64.*\.msi$/i,
    'arm64-msi':  /(FileID|FileID-)arm64.*\.msi$/i,
    'dmg':        /\.dmg$/i,
  };

  try {
    const res = await fetch('https://api.github.com/repos/WebWorldWide/FileID/releases?per_page=5', {
      headers: { 'Accept': 'application/vnd.github+json' },
    });
    if (!res.ok) throw new Error('http ' + res.status);
    const releases = await res.json();
    if (!Array.isArray(releases) || releases.length === 0) {
      // No releases yet — show the polite "build from source" notice
      showNoReleases(status);
      return;
    }

    const latest = releases.find(r => !r.draft && !r.prerelease) || releases[0];
    const assets = latest.assets || [];
    if (assets.length === 0) {
      showNoReleases(status);
      return;
    }

    // Match assets → buttons
    let matched = 0;
    document.querySelectorAll('[data-asset]').forEach(el => {
      const key = el.getAttribute('data-asset');
      const pat = patterns[key];
      if (!pat) return;
      const a = assets.find(x => pat.test(x.name));
      if (a) {
        el.setAttribute('href', a.browser_download_url);
        // Update the secondary label with size if it's the block primary
        const sec = el.querySelector('.btn-sec');
        if (sec && a.size) {
          const mb = (a.size / 1024 / 1024).toFixed(1);
          const original = sec.textContent;
          sec.textContent = `${original} · ${mb} MB`;
        }
        matched++;
      }
    });

    const tag = latest.tag_name || latest.name || 'latest';
    const date = latest.published_at ? new Date(latest.published_at).toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' }) : '';
    if (matched > 0) {
      status.hidden = false;
      status.innerHTML = `
        <strong>${escapeHtml(tag)}</strong> — released ${escapeHtml(date)}.
        <a href="${latest.html_url}" target="_blank" rel="noopener">View release notes ↗</a>
      `;
    } else {
      showNoReleases(status);
    }
  } catch (err) {
    // Network blocked, rate-limit, no releases — fall through silently to the static markup
    showNoReleases(status);
  }
}

function showNoReleases(status) {
  status.hidden = false;
  status.innerHTML = `
    <strong>No release builds shipping yet.</strong>
    Compile your own using <code>./build.sh</code> below, or
    <a href="https://github.com/WebWorldWide/FileID/releases" target="_blank" rel="noopener">watch the releases page</a>
    for the first signed installer.
  `;
}

function escapeHtml(s) {
  return String(s).replace(/[&<>"']/g, c => ({ '&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;' }[c]));
}

/* ───────────────────────────────────────────────
   4) SCROLL REVEAL — IntersectionObserver
   ─────────────────────────────────────────────── */
function initReveals() {
  if (reduceMotion) {
    document.querySelectorAll('.reveal, .reveal-stagger').forEach(el => el.classList.add('in'));
    return;
  }
  const targets = document.querySelectorAll('.reveal, .reveal-stagger');
  if (!('IntersectionObserver' in window) || targets.length === 0) {
    targets.forEach(el => el.classList.add('in'));
    return;
  }
  const io = new IntersectionObserver((entries) => {
    entries.forEach(e => {
      if (e.isIntersecting) {
        e.target.classList.add('in');
        io.unobserve(e.target);
      }
    });
  }, { rootMargin: '0px 0px -10% 0px', threshold: 0.05 });

  targets.forEach(el => io.observe(el));
}

/* ───────────────────────────────────────────────
   5) CARD TILT — cursor parallax on feature cards
   ─────────────────────────────────────────────── */
function initCardTilt() {
  if (reduceMotion) return;
  // Only on devices that support hover (avoid touch jitter)
  if (!window.matchMedia('(hover: hover)').matches) return;

  const cards = document.querySelectorAll('.feature');
  cards.forEach(card => {
    let raf = 0;

    card.addEventListener('pointermove', (e) => {
      const r = card.getBoundingClientRect();
      const px = (e.clientX - r.left) / r.width;   // 0..1
      const py = (e.clientY - r.top)  / r.height;  // 0..1
      const ry = (px - 0.5) *  6;   // deg
      const rx = (py - 0.5) * -5;   // deg
      cancelAnimationFrame(raf);
      raf = requestAnimationFrame(() => {
        card.classList.add('tilt');
        card.style.setProperty('--tx', rx.toFixed(2) + 'deg');
        card.style.setProperty('--ty', ry.toFixed(2) + 'deg');
      });
    });

    card.addEventListener('pointerleave', () => {
      cancelAnimationFrame(raf);
      card.classList.remove('tilt');
      card.style.setProperty('--tx', '0deg');
      card.style.setProperty('--ty', '0deg');
    });
  });
}

/* ───────────────────────────────────────────────
   Boot
   ─────────────────────────────────────────────── */
function boot() {
  initLavaLamp();
  init3DLogo();
  initReleases();
  initReveals();
  initCardTilt();
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', boot);
} else {
  boot();
}

})();
