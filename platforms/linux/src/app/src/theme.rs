// Design system — the FileID brand palette + glass surfaces, ported 1:1 from
// the macOS reference (`platforms/apple/.../Theme/Theme.swift`) and the Windows
// `FileID.Theme` GlassCard. A single GTK `CssProvider` installed at startup
// carries the palette + the reusable classes the UI leans on.
//
// The highest-leverage move (vs. the old scaffold that looked like stock
// Adwaita): we recolor **libadwaita's own named colors** — `accent_bg_color` →
// gold, view/window backgrounds → transparent so the LavaLamp reads through —
// so every stock widget (buttons, switches, checks, progress bars, selected
// rows, focus rings) brands itself without touching each call site. On top of
// that we style the widget *types* (button/entry/progressbar/switch/rows/…) and
// add the sidebar nav + glass-card + pill + tile classes.
//
// Gold #FFCC00, orange #FF6600 (LavaLamp), lavender #B19BCE, cyan #A0E2EA,
// pink #F2A6C0 — the signature identity shared across all three platforms.

/// Brand colours as RGB triples in the 0..1 range Cairo wants. Used by the
/// LavaLamp painter so the animated background and the CSS stay in lockstep.
/// These mirror the macOS/Windows LavaLamp recipe: gold + orange + dark blobs
/// on a near-black base. (lavender/cyan/pink are brand colours too, but only
/// appear as CSS tokens — the Cairo LavaLamp uses just gold/orange/dark.)
pub mod rgb {
    pub const GOLD: (f64, f64, f64) = (1.0, 0.8, 0.0); // #FFCC00
    pub const ORANGE: (f64, f64, f64) = (1.0, 0.4, 0.0); // #FF6600
    /// Near-black base the macOS/Windows LavaLamp fills behind the blobs (#141414).
    pub const BASE: (f64, f64, f64) = (0.078, 0.078, 0.078);
    /// The large dark ellipse that mottles the centre darker (#0D0D0D).
    pub const DARK: (f64, f64, f64) = (0.051, 0.051, 0.051);
}

const CSS: &str = r#"
/* ── Brand palette ────────────────────────────────────────────────────────── */
@define-color fileid_gold     #FFCC00;
@define-color fileid_gold_dim #CCA300;
@define-color fileid_orange   #FF6600;
@define-color fileid_lavender #B19BCE;
@define-color fileid_cyan     #A0E2EA;
@define-color fileid_pink     #F2A6C0;
@define-color fileid_base     #0B0B0C;
/* Prototype v3 tokens (shared/docs/DESIGN_V3.md) */
@define-color fileid_bg0      #070708;
@define-color fileid_text1    #F0F1F2;
@define-color fileid_text2    #CFD3D8;
@define-color fileid_text3    #A6ACB3;
@define-color fileid_label    #AEB4BB;
@define-color fileid_danger   #FF6A5E;
@define-color fileid_warning  #E0A340;

/* ── Recolor libadwaita so every stock widget inherits the brand ──────────── */
/* Gold is the one primary/accent colour (macOS/Windows discipline). */
@define-color accent_bg_color  #FFCC00;
@define-color accent_fg_color  #000000;
@define-color accent_color     #F4C430;   /* readable gold for text/icon accents on dark */

@define-color window_bg_color    #0B0B0C;
@define-color window_fg_color    #F0F1F2;
@define-color view_bg_color      transparent;   /* let the LavaLamp read through content */
@define-color view_fg_color      #FFFFFF;
@define-color headerbar_bg_color transparent;
@define-color headerbar_fg_color #FFFFFF;
@define-color card_bg_color      alpha(#18181B, 0.78);
@define-color card_fg_color      #F0F1F2;
@define-color popover_bg_color   #161618;
@define-color dialog_bg_color    #161618;
@define-color sidebar_bg_color   alpha(#0B0B0C, 0.5);

/* ── Typography ───────────────────────────────────────────────────────────── */
/* Set the font + ramp EXPLICITLY so the app looks identical on GNOME / KDE /
   COSMIC and never inherits a poor system UI font (the old build inherited
   "Open Sans 11", which read cheap). Inter is the premium, SF-Pro-like analog;
   we ship a fallback chain so it degrades gracefully where Inter is absent.
   Font properties inherit in GTK CSS, so setting them on `window` cascades. */
window {
    font-family: "Archivo", "Inter", "Roboto", "Noto Sans", "Cantarell", sans-serif;
    font-size: 13.5px;
}
label, button, entry, headerbar, .nav-row, row, popover, gridview, listview {
    font-family: "Archivo", "Inter", "Roboto", "Noto Sans", "Cantarell", sans-serif;
}
/* Prototype v3: serif page titles, Archivo section labels, mono for paths and shortcuts. */
.title-1 { font-family: "Instrument Serif", "New York", "Georgia", serif; font-size: 34px; font-weight: 400; letter-spacing: 0; }
.title-2 { font-family: "Instrument Serif", "New York", "Georgia", serif; font-size: 24px; font-weight: 400; letter-spacing: 0; }
.title-3 { font-size: 14px; font-weight: 600; letter-spacing: 0; }
.heading { font-weight: 600; letter-spacing: 0; }
.caption { font-size: 11px; }
.monospace, .meta, .kbd { font-family: "JetBrains Mono", "Roboto Mono", "DejaVu Sans Mono", monospace; }
.section-label { font-size: 11px; font-weight: 600; letter-spacing: 0.6px; color: @fileid_label; }
.meta { font-size: 11.5px; color: @fileid_text3; }
/* Corner scale xs 4 / s 6 / m 8 / l 10: controls s, cards and rows m, panels and floating cards l. */
button, entry, .pill, .chip { border-radius: 6px; }
.card, .glass-card, .padded-card, .fileid-glass, popover > contents { border-radius: 10px; }
check { border-radius: 4px; }
radio, switch, switch > slider, progressbar > trough, progressbar > trough > progress { border-radius: 99px; }

/* ── Backgrounds: keep content transparent so the signature LavaLamp shows ── */
.fileid-tab, viewstack, stack,
scrolledwindow, scrolledwindow > viewport, viewport,
flowbox, flowboxchild, gridview, gridview > child, list {
    background-color: transparent;
}
window { background-color: @fileid_base; }
.dim-label { color: alpha(#FFFFFF, 0.55); }

/* The muted "material" layer over the LavaLamp — lighter than the old 0.55 so
   the drifting gold/orange blobs actually read (macOS frosts, doesn't black-out). */
.fileid-scrim { background-color: alpha(#0A0A0C, 0.18); }

/* ── Glass card: dark frosted fill + hairline highlight + soft depth ──────── */
/* GTK has no backdrop blur; approximate the Windows AcrylicBrush (black @0.5 +
   white @0.06 + 1px white @0.08) with a dark translucent fill + highlight. */
.glass-card, .padded-card {
    background-color: alpha(#18181B, 0.78);
    border: 1px solid alpha(#FFFFFF, 0.11);
    border-radius: 10px;
    padding: 14px;
    box-shadow: none;
}
.fileid-glass {
    background-color: alpha(#18181B, 0.78);
    border: 1px solid alpha(#FFFFFF, 0.11);
    border-radius: 10px;
}

/* ── Title bar (Prototype v3): 48 px over bg1, bottom hairline ───────────── */
headerbar, .fileid-headerbar {
    background: transparent;
    box-shadow: none;
    border: none;
    min-height: 40px;
}
headerbar.fileid-titlebar {
    min-height: 48px;
    padding: 0 8px 0 6px;
    background-color: alpha(#0B0B0C, 0.62);
    border-bottom: 1px solid alpha(#FFFFFF, 0.08);
}
headerbar windowcontrols button { background: transparent; border: none; box-shadow: none; }
headerbar windowcontrols button:hover { background-color: alpha(#FFFFFF, 0.10); }

/* ── Sidebar: area switcher, 32 px rows, quiet badges, scan card ─────────── */
.fileid-sidebar {
    background-color: alpha(#0B0B0C, 0.55);
    border-right: 1px solid alpha(#FFFFFF, 0.08);
    padding: 0;
}
.nav-row {
    background: transparent;
    border: none;
    border-radius: 8px;
    padding: 0 10px;
    margin: 0 8px;
    min-height: 32px;
    color: @fileid_text1;
    font-size: 13.5px;
    font-weight: 400;
    box-shadow: none;
    transition: background-color 140ms ease-out, color 140ms ease-out;
}
.nav-row:hover { background-color: alpha(#FFFFFF, 0.05); border-color: transparent; }
.nav-row.active { background-color: alpha(@fileid_gold, 0.12); color: @fileid_gold; font-weight: 600; }
.nav-row image { color: @fileid_text2; }
.nav-row.active image { color: @fileid_gold; }
.nav-badge { font-size: 11.5px; font-weight: 500; font-feature-settings: "tnum"; }

/* Settings / page section headings — readable uppercase divider. */
.settings-heading {
    color: alpha(#FFFFFF, 0.62);
    font-size: 10pt;
    font-weight: 800;
    letter-spacing: 1px;
}

/* ── Buttons: brand the stock greys ──────────────────────────────────────── */
button {
    background-color: alpha(#FFFFFF, 0.07);
    color: #FFFFFF;
    border: 1px solid alpha(#FFFFFF, 0.10);
    border-radius: 6px;
    padding: 6px 14px;
    min-height: 20px;
    box-shadow: none;
    transition: background-color 140ms ease, border-color 140ms ease, box-shadow 140ms ease;
}
button:hover { background-color: alpha(#FFFFFF, 0.13); border-color: alpha(#FFFFFF, 0.18); }
button:active, button:checked { background-color: alpha(#FFFFFF, 0.18); }
button:disabled { color: alpha(#FFFFFF, 0.35); opacity: 0.6; }
button.flat { background: transparent; border-color: transparent; box-shadow: none; }
button.flat:hover { background-color: alpha(#FFFFFF, 0.10); border-color: transparent; }
button.image-button { padding: 6px; min-width: 20px; }

/* Primary CTA — gold fill, black label (Start scan / Apply / Pick folder). */
.gold-button, button.suggested-action {
    background: @fileid_gold;
    color: #000000;
    font-weight: 700;
    border: none;
    padding: 7px 16px;
    box-shadow: 0 2px 10px alpha(@fileid_gold, 0.22);
}
.gold-button:hover, button.suggested-action:hover {
    background: shade(@fileid_gold, 1.06);
    box-shadow: 0 3px 14px alpha(@fileid_gold, 0.32);
}
.gold-button:active, button.suggested-action:active { background: shade(@fileid_gold, 0.94); }
.gold-button:disabled { background: alpha(@fileid_gold, 0.30); color: alpha(#000000, 0.5); box-shadow: none; }

button.destructive-action {
    background-color: alpha(#E5A5A5, 0.14);
    color: #F2B8B8;
    border: 1px solid alpha(#E5A5A5, 0.40);
}
button.destructive-action:hover { background-color: alpha(#E5A5A5, 0.22); }

/* ── Progress: gold fill (fixes the .gold-accent-on-ProgressBar no-op) ────── */
progressbar > trough { background-color: alpha(#FFFFFF, 0.08); border-radius: 99px; min-height: 6px; }
progressbar > trough > progress { background-color: @fileid_gold; border-radius: 99px; min-height: 6px; }

/* ── Switch / check / radio — gold ───────────────────────────────────────── */
switch { background-color: alpha(#FFFFFF, 0.14); border-radius: 99px; border: none; }
switch:checked { background-color: @fileid_gold; }
switch > slider { background-color: #FFFFFF; border-radius: 99px; }
check, radio {
    background-color: alpha(#FFFFFF, 0.06);
    border: 1px solid alpha(#FFFFFF, 0.22);
}
check { border-radius: 4px; }
check:checked, radio:checked {
    background-color: @fileid_gold;
    border-color: @fileid_gold;
    color: #000000;
}

/* ── Entry / search field — translucent pill ─────────────────────────────── */
entry, .fileid-search {
    background-color: alpha(#FFFFFF, 0.06);
    border: 1px solid alpha(#FFFFFF, 0.10);
    border-radius: 6px;
    color: #FFFFFF;
    padding: 4px 10px;
    box-shadow: none;
}
entry:focus-within, .fileid-search:focus-within { border-color: alpha(@fileid_gold, 0.55); }
entry image { color: alpha(#FFFFFF, 0.55); }

/* ── Adwaita rows / boxed lists → glassy, not opaque GNOME ────────────────── */
.boxed-list { background: transparent; border: none; box-shadow: none; }
.boxed-list > row {
    background-color: alpha(#FFFFFF, 0.05);
    border: 1px solid alpha(#FFFFFF, 0.07);
    border-radius: 8px;
    margin: 3px 0;
}
.boxed-list > row:hover { background-color: alpha(#FFFFFF, 0.09); }
row:selected { background-color: alpha(@fileid_gold, 0.18); color: #FFFFFF; }

/* ── Empty-state pages: transparent so the LavaLamp shows ─────────────────── */
statuspage { background: transparent; }

/* ── Scrollbars: slim + subtle ───────────────────────────────────────────── */
scrollbar { background: transparent; border: none; }
scrollbar > range > trough { background: transparent; }
scrollbar > range > trough > slider {
    background-color: alpha(#FFFFFF, 0.20);
    border-radius: 99px;
    min-width: 6px;
    min-height: 6px;
}
scrollbar > range > trough > slider:hover { background-color: alpha(#FFFFFF, 0.35); }

separator { background-color: alpha(#FFFFFF, 0.08); min-width: 1px; min-height: 1px; }
spinner { color: @fileid_gold; }

/* ── Gold segmented filter pills (All / Images / Videos / …) ──────────────── */
.pill {
    background-color: alpha(#FFFFFF, 0.06);
    border: 1px solid transparent;
    border-radius: 6px;
    padding: 4px 15px;
    color: alpha(#FFFFFF, 0.78);
    font-weight: 500;
    box-shadow: none;
    transition: background-color 130ms ease, color 130ms ease;
}
.pill:hover { background-color: alpha(#FFFFFF, 0.12); }
.pill-active {
    background-color: @fileid_gold;
    color: #000000;
    font-weight: 700;
    border-color: transparent;
}

/* ── Accents ─────────────────────────────────────────────────────────────── */
.gold-accent     { color: @fileid_gold; }
.lavender-accent { color: @fileid_lavender; }
.cyan-accent     { color: @fileid_cyan; }
.pink-accent     { color: @fileid_pink; }

/* ── File tile ───────────────────────────────────────────────────────────── */
.file-tile {
    background-color: alpha(#FFFFFF, 0.05);
    border: 1px solid alpha(#FFFFFF, 0.08);
    border-radius: 8px;
    padding: 8px;
    box-shadow: 0 2px 8px alpha(#000000, 0.30);
}
.file-tile:hover { border-color: alpha(#FFFFFF, 0.20); background-color: alpha(#FFFFFF, 0.08); }
.file-tile-selected { border: 2px solid @fileid_gold; }

.tile-thumb { background-color: alpha(#000000, 0.35); border-radius: 6px; }

.kind-badge {
    background-color: @fileid_gold;
    color: #000000;
    border-radius: 4px;
    padding: 1px 7px;
    font-size: 8pt;
    font-weight: 700;
}

.tile-caption { color: alpha(#FFFFFF, 0.55); font-size: 9pt; }

.edit-name-hint {
    color: @fileid_gold;
    font-size: 10pt;
    font-weight: 700;
    margin-top: 2px;
}

.people-flow-banner {
    padding: 10px 12px;
    border-color: alpha(@fileid_gold, 0.35);
}

/* Centered ▶ badge over video tile keyframes. */
.video-play-badge {
    color: #FFFFFF;
    background-color: alpha(#000000, 0.45);
    border-radius: 99px;
    padding: 8px;
}

/* ── Status tones: gold acts, lavender = a model runs, cyan = info, pink = done ── */
.tone-gold { color: @fileid_gold; }
.tone-ai { color: @fileid_lavender; }
.tone-info { color: @fileid_cyan; }
.tone-done { color: @fileid_pink; }
.tone-danger { color: @fileid_danger; }
.tone-warning { color: @fileid_warning; }
.tone-muted, .tone-paused { color: @fileid_text3; }
.tone-text2 { color: @fileid_text2; }
progressbar.tone-gold > trough > progress { background-image: linear-gradient(to right, #FF8A1A, #FFB000, #FFCC00, #FFE9A3); }
progressbar.tone-ai > trough > progress { background-image: linear-gradient(to right, #7462A6, #B19BCE, #DED2F2); }
progressbar.tone-done > trough > progress { background-image: linear-gradient(to right, #FFB000, #FFCC00, #F2A6C0); }
progressbar.tone-paused > trough > progress { background-image: linear-gradient(to right, alpha(#B9BEC4, 0.28), alpha(#B9BEC4, 0.55)); }
progressbar.tone-info > trough > progress { background-image: linear-gradient(to right, alpha(#A0E2EA, 0.2), alpha(#A0E2EA, 0.55)); }
progressbar.tone-muted > trough > progress, progressbar.tone-text2 > trough > progress { background-image: linear-gradient(to right, alpha(#D6DADE, 0.35), alpha(#D6DADE, 0.85)); }
progressbar.tone-danger > trough > progress { background-image: none; background-color: @fileid_danger; }
progressbar.grain-bar > trough { background-color: alpha(#FFFFFF, 0.07); }
progressbar.mini > trough, progressbar.mini > trough > progress { min-height: 4px; }
.tone-dot { min-width: 6px; min-height: 6px; border-radius: 99px; }
.tone-dot.tone-gold { background-color: @fileid_gold; }
.tone-dot.tone-ai { background-color: @fileid_lavender; }
.tone-dot.tone-info { background-color: @fileid_cyan; }
.tone-dot.tone-done { background-color: @fileid_pink; }
.tone-dot.tone-muted, .tone-dot.tone-paused { background-color: @fileid_text3; }

/* ── Title bar controls ──────────────────────────────────────────────────── */
.tb-control, menubutton.tb-control > button {
    min-height: 30px;
    padding: 0 10px;
    background-color: alpha(#000000, 0.28);
    border: 1px solid alpha(#FFFFFF, 0.16);
    border-radius: 6px;
    box-shadow: none;
    color: @fileid_text1;
}
.tb-control:hover, menubutton.tb-control > button:hover { background-color: alpha(#000000, 0.36); border-color: alpha(#FFFFFF, 0.24); }
menubutton.tb-control > button:checked, .tb-control.open { background-color: alpha(#000000, 0.28); border-color: @fileid_gold; }
.tb-icon { min-width: 30px; padding: 0; color: @fileid_text2; background-color: alpha(#000000, 0.32); }
.tb-icon.active { background-color: alpha(@fileid_gold, 0.14); border-color: @fileid_gold; color: @fileid_gold; }
button.tb-plain { background: transparent; border-color: transparent; min-width: 26px; min-height: 26px; padding: 0; color: @fileid_text2; }
button.tb-plain:hover { background-color: alpha(#FFFFFF, 0.06); border-color: transparent; }
.wordmark { font-family: "Instrument Serif", "New York", "Georgia", serif; font-size: 18px; font-weight: 400; color: @fileid_text1; }
.tb-folder { font-size: 13.5px; font-weight: 600; color: @fileid_text1; }
.tb-folder.empty { color: @fileid_text3; }
.tb-caret { font-family: "JetBrains Mono", "Roboto Mono", "DejaVu Sans Mono", monospace; font-size: 11px; color: @fileid_text3; }
.vrule { background-color: alpha(#FFFFFF, 0.18); min-width: 1px; min-height: 14px; }
.tb-activity-label { font-size: 12.5px; font-weight: 500; color: @fileid_text1; }
.tb-activity-pct { font-size: 12.5px; font-weight: 600; font-feature-settings: "tnum"; }
.tb-search {
    min-height: 30px;
    padding: 0 6px 0 10px;
    background-color: alpha(#000000, 0.32);
    border: 1px solid alpha(#FFFFFF, 0.16);
    border-radius: 6px;
    font-size: 13.5px;
    color: @fileid_text1;
}
.tb-search:focus-within { border-color: @fileid_gold; }
.kbd { font-size: 11px; color: @fileid_text3; border: 1px solid alpha(#FFFFFF, 0.16); border-radius: 4px; padding: 1px 5px; }

/* ── Popovers (folder switcher, search, activity) ────────────────────────── */
popover.fileid-popover > contents {
    background-color: #161618;
    border: 1px solid alpha(#FFFFFF, 0.14);
    border-radius: 10px;
    padding: 0;
}
.pop-head { padding: 10px 12px 6px 12px; }
.pop-row { padding: 9px 12px; border-top: 1px solid alpha(#FFFFFF, 0.06); }
.pop-actions { padding: 10px; background-color: alpha(#FFFFFF, 0.02); border-top: 1px solid alpha(#FFFFFF, 0.14); }
.pop-footer { padding: 8px 10px; border-top: 1px solid alpha(#FFFFFF, 0.06); }
.pop-title { font-size: 14px; font-weight: 600; color: @fileid_text1; }
.pop-meta { font-size: 12px; color: @fileid_text3; }
button.result-row {
    background: transparent;
    border: none;
    border-radius: 0;
    min-height: 32px;
    padding: 0 12px;
    box-shadow: none;
    font-size: 14px;
}
button.result-row:hover, button.result-row.first { background-color: alpha(@fileid_gold, 0.08); }
button.ghost { background: transparent; border-color: transparent; color: @fileid_text2; padding: 0 8px; }
button.ghost:hover { background-color: alpha(#FFFFFF, 0.06); border-color: transparent; }
button.ghost.danger { color: @fileid_danger; }
button.secondary { min-height: 28px; padding: 0 10px; background-color: alpha(#FFFFFF, 0.05); border: 1px solid alpha(#FFFFFF, 0.14); font-size: 13px; font-weight: 500; }
button.secondary:hover { background-color: alpha(#FFFFFF, 0.09); border-color: alpha(#FFFFFF, 0.24); }
button.small { min-height: 24px; padding: 0 8px; font-size: 12px; }
button.gold-button.compact { min-height: 28px; padding: 0 12px; font-size: 13px; box-shadow: none; color: #141414; }

/* ── Sidebar area switcher + scan card ───────────────────────────────────── */
.area-switcher {
    background-color: alpha(#000000, 0.28);
    border: 1px solid alpha(#FFFFFF, 0.14);
    border-radius: 8px;
    padding: 3px;
}
.area-switcher button {
    background: transparent;
    border: none;
    box-shadow: none;
    min-height: 26px;
    padding: 0;
    font-size: 12.5px;
    font-weight: 500;
    color: @fileid_text2;
    transition: color 140ms ease-out;
}
.area-switcher button:hover { background: transparent; color: @fileid_text1; }
.area-switcher button.on { color: #141414; font-weight: 600; }
.scan-card {
    background-color: alpha(#FFFFFF, 0.035);
    border: 1px solid alpha(#FFFFFF, 0.08);
    border-radius: 10px;
    padding: 12px;
}
.scan-title { font-size: 13px; font-weight: 600; color: @fileid_text1; }
.scan-pct { font-size: 12.5px; font-weight: 600; font-feature-settings: "tnum"; }
.scan-step { font-size: 12px; color: @fileid_text2; }
.scan-sub { font-size: 12px; color: @fileid_text3; }
.step-seg { min-height: 3px; border-radius: 99px; background-color: alpha(#FFFFFF, 0.12); }
.step-seg.past { background-color: alpha(#CFD3D8, 0.7); }
.step-seg.current.tone-gold { background-color: @fileid_gold; }
.step-seg.current.tone-ai { background-color: @fileid_lavender; }
.step-seg.current.tone-done { background-color: @fileid_pink; }
.step-seg.current.tone-danger { background-color: @fileid_danger; }
.step-seg.current.tone-muted, .step-seg.current.tone-paused, .step-seg.current.tone-text2 { background-color: @fileid_text2; }
.scan-stats { padding: 6px 0; border-top: 1px solid alpha(#FFFFFF, 0.06); border-bottom: 1px solid alpha(#FFFFFF, 0.06); }
.stat-key { font-size: 12px; color: @fileid_text3; }
.stat-value { font-size: 12px; font-weight: 500; color: @fileid_text1; font-feature-settings: "tnum"; }
.stat-trailing { font-size: 11.5px; color: @fileid_text3; font-feature-settings: "tnum"; }
.engine-note {
    background-color: alpha(@fileid_warning, 0.08);
    border: 1px solid alpha(@fileid_warning, 0.35);
    border-radius: 8px;
    padding: 8px;
}
.engine-note label { font-size: 11.5px; color: @fileid_text2; }

/* ── Page area: crumb + serif title header over a frosted page ───────────── */
.fileid-page { background-color: alpha(#0F0F11, 0.8); }
.page-header { padding: 18px 26px 14px 26px; border-bottom: 1px solid alpha(#FFFFFF, 0.08); }
.page-meta { font-size: 12px; color: @fileid_text2; }

/* ── Library Selected pane: key/value rows and tag chips ─────────────────── */
.selected-pane { padding: 4px 14px 14px 14px; margin-left: 12px; border-left: 1px solid alpha(#FFFFFF, 0.08); }
.kv-row { padding-bottom: 5px; border-bottom: 1px solid alpha(#FFFFFF, 0.06); }
.kv-key { font-size: 11px; font-weight: 600; letter-spacing: 0.6px; color: @fileid_label; }
.kv-value { font-size: 13px; color: @fileid_text1; }
.chip {
    font-family: "JetBrains Mono", "Roboto Mono", "DejaVu Sans Mono", monospace;
    font-size: 11px;
    color: @fileid_text2;
    padding: 1px 6px;
    border: 1px solid alpha(#FFFFFF, 0.14);
}
.numeric { font-feature-settings: "tnum"; }
.drop-zone {
    margin: 14px;
    border: 2px dashed @fileid_gold;
    border-radius: 6px;
    background-color: alpha(@fileid_gold, 0.08);
}
"#;

/// Install the brand CSS into the default display and force dark mode.
///
/// Called once from `connect_startup`. Force-dark matches macOS + Windows
/// (the app is dark-only by design).
pub fn install() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    let high_contrast = gtk::CssProvider::new();
    high_contrast.load_from_string(HIGH_CONTRAST_CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    let style = adw::StyleManager::default();
    style.set_color_scheme(adw::ColorScheme::ForceDark);
    apply_high_contrast(&high_contrast, style.is_high_contrast());
    style.connect_high_contrast_notify(move |style| apply_high_contrast(&high_contrast, style.is_high_contrast()));
}

fn apply_high_contrast(provider: &gtk::CssProvider, on: bool) {
    let Some(display) = gtk::gdk::Display::default() else { return };
    gtk::style_context_remove_provider_for_display(&display, provider);
    if on {
        gtk::style_context_add_provider_for_display(&display, provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1);
    }
}

/// The system high-contrast setting, which also stills the lava lamp.
pub fn high_contrast() -> bool {
    adw::StyleManager::default().is_high_contrast()
}

/// macOS `Theme.adaptive(normal:increased:)` under Increase Contrast: hairlines
/// 0.08/0.06/0.14 → 0.34/0.26/0.5, text3 → #D6D9DD, text2 → #EBEDEF, and a solid
/// base behind the UI instead of the lava (Reduce Transparency).
const HIGH_CONTRAST_CSS: &str = r#"
.dim-label, .tile-caption { color: alpha(#FFFFFF, 0.85); }
.meta, .pop-meta, .scan-sub, .stat-key, .stat-trailing, .tone-muted, .tone-paused, .section-label, .kv-key { color: #D6D9DD; }
.page-meta, .scan-step, .tone-text2, .nav-row image, .chip { color: #EBEDEF; }
.glass-card, .padded-card, .fileid-glass, .file-tile { border-color: alpha(#FFFFFF, 0.34); }
.fileid-sidebar { border-right-color: alpha(#FFFFFF, 0.34); }
.selected-pane { border-left-color: alpha(#FFFFFF, 0.34); }
headerbar.fileid-titlebar, .page-header { border-bottom-color: alpha(#FFFFFF, 0.34); }
.kv-row, .pop-row, .pop-footer, .scan-stats, .boxed-list > row { border-color: alpha(#FFFFFF, 0.26); }
.tb-control, menubutton.tb-control > button, .area-switcher, .tb-search, .chip, .scan-card, button.secondary, entry, popover.fileid-popover > contents { border-color: alpha(#FFFFFF, 0.5); }
separator { background-color: alpha(#FFFFFF, 0.34); }
.fileid-scrim { background-color: #0B0B0C; }
"#;
