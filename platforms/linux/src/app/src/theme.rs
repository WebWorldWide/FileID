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
@define-color fileid_base     #0E0E12;

/* ── Recolor libadwaita so every stock widget inherits the brand ──────────── */
/* Gold is the one primary/accent colour (macOS/Windows discipline). */
@define-color accent_bg_color  #FFCC00;
@define-color accent_fg_color  #000000;
@define-color accent_color     #F4C430;   /* readable gold for text/icon accents on dark */

@define-color window_bg_color    #0E0E12;
@define-color window_fg_color    #FFFFFF;
@define-color view_bg_color      transparent;   /* let the LavaLamp read through content */
@define-color view_fg_color      #FFFFFF;
@define-color headerbar_bg_color transparent;
@define-color headerbar_fg_color #FFFFFF;
@define-color card_bg_color      alpha(#FFFFFF, 0.06);
@define-color card_fg_color      #FFFFFF;
@define-color popover_bg_color   #17171C;
@define-color dialog_bg_color    #131318;
@define-color sidebar_bg_color   alpha(#000000, 0.28);

/* ── Typography ───────────────────────────────────────────────────────────── */
/* Set the font + ramp EXPLICITLY so the app looks identical on GNOME / KDE /
   COSMIC and never inherits a poor system UI font (the old build inherited
   "Open Sans 11", which read cheap). Inter is the premium, SF-Pro-like analog;
   we ship a fallback chain so it degrades gracefully where Inter is absent.
   Font properties inherit in GTK CSS, so setting them on `window` cascades. */
window {
    font-family: "Inter", "Inter Display", "Roboto", "Noto Sans", "Cantarell", sans-serif;
    font-size: 13px;
}
label, button, entry, headerbar, .nav-row, row, popover, gridview, listview {
    font-family: "Inter", "Inter Display", "Roboto", "Noto Sans", "Cantarell", sans-serif;
}
/* Page titles / section headings — bold, tight tracking like SF Pro Display. */
.title-1 { font-size: 21pt; font-weight: 800; letter-spacing: -0.6px; }
.title-2 { font-size: 15pt; font-weight: 700; letter-spacing: -0.4px; }
.title-3 { font-size: 12pt; font-weight: 700; letter-spacing: -0.2px; }
.heading { font-weight: 700; letter-spacing: -0.1px; }
.caption { font-size: 9pt; }
.monospace { font-family: "JetBrains Mono", "Roboto Mono", "DejaVu Sans Mono", monospace; }

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
    background-color: alpha(#16161B, 0.64);
    border: 1px solid alpha(#FFFFFF, 0.10);
    border-radius: 12px;
    padding: 16px;
    box-shadow: 0 4px 18px alpha(#000000, 0.38);
}

/* ── Header bar floats over the LavaLamp like the macOS unified toolbar ───── */
headerbar, .fileid-headerbar {
    background: transparent;
    box-shadow: none;
    border: none;
    min-height: 40px;
}
headerbar windowcontrols button { background: transparent; border: none; box-shadow: none; }
headerbar windowcontrols button:hover { background-color: alpha(#FFFFFF, 0.10); }

/* ── Left sidebar nav (the macOS/Windows structure, not GNOME top tabs) ───── */
.fileid-sidebar {
    background-color: alpha(#000000, 0.30);
    border-right: 1px solid alpha(#FFFFFF, 0.08);
    padding: 10px 10px 14px 10px;
}
.sidebar-heading {
    color: alpha(#FFFFFF, 0.42);
    font-size: 9pt;
    font-weight: 800;
    letter-spacing: 1.2px;
    margin: 14px 10px 4px 10px;
}
.nav-row {
    background: transparent;
    border: 1px solid transparent;
    border-radius: 8px;
    padding: 8px 12px;
    color: alpha(#FFFFFF, 0.82);
    font-weight: 500;
    box-shadow: none;
    min-height: 0;
    transition: background-color 130ms ease, border-color 130ms ease, color 130ms ease;
}
.nav-row:hover { background-color: alpha(#FFFFFF, 0.06); }
.nav-row.active {
    background-color: alpha(@fileid_gold, 0.18);
    border-color: alpha(@fileid_gold, 0.55);
    color: @fileid_gold;
    font-weight: 700;
}
.nav-row image { color: alpha(#FFFFFF, 0.75); }
.nav-row.active image { color: @fileid_gold; }

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
    border-radius: 9px;
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
check { border-radius: 6px; }
check:checked, radio:checked {
    background-color: @fileid_gold;
    border-color: @fileid_gold;
    color: #000000;
}

/* ── Entry / search field — translucent pill ─────────────────────────────── */
entry, .fileid-search {
    background-color: alpha(#FFFFFF, 0.06);
    border: 1px solid alpha(#FFFFFF, 0.10);
    border-radius: 18px;
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
    border-radius: 10px;
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
    border-radius: 99px;
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
    border-radius: 12px;
    padding: 8px;
    box-shadow: 0 2px 8px alpha(#000000, 0.30);
}
.file-tile:hover { border-color: alpha(#FFFFFF, 0.20); background-color: alpha(#FFFFFF, 0.08); }
.file-tile-selected { border: 2px solid @fileid_gold; }

.tile-thumb { background-color: alpha(#000000, 0.35); border-radius: 8px; }

.kind-badge {
    background-color: @fileid_gold;
    color: #000000;
    border-radius: 99px;
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
"#;

/// Install the brand CSS into the default display and force dark mode.
///
/// Called once from `connect_startup`. Force-dark matches macOS + Windows
/// (the app is dark-only by design).
pub fn install() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
}
