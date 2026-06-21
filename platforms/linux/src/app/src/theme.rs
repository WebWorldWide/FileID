// Design system — the FileID brand palette + glass surfaces, ported 1:1
// from the macOS reference (`platforms/apple/.../Theme/Theme.swift`) and the
// Windows `FileID.Theme` GlassCard. A single GTK `CssProvider` installed at
// startup carries the palette as `@define-color` tokens plus the reusable
// classes the rest of the UI leans on:
//
//   .glass-card      ← GlassCard / .ultraThinMaterial analog
//   .fileid-scrim    ← the muted material layer over the LavaLamp
//   .pill / .pill-active ← the gold segmented filter pills
//   .gold-accent / .gold-button ← primary CTA colour (Start Scan, Apply)
//
// Gold #FFCC00, lavender #B19BCE, cyan #A0E2EA, pink #F2A6C0 — the signature
// identity shared across all three platforms. Never reinterpret these.

use adw::prelude::*;

/// Brand colours as RGB triples in the 0..1 range Cairo wants. Used by the
/// LavaLamp painter so the animated background and the CSS stay in lockstep.
pub mod rgb {
    pub const GOLD: (f64, f64, f64) = (1.0, 0.8, 0.0); // #FFCC00
    pub const LAVENDER: (f64, f64, f64) = (0.694, 0.608, 0.808); // #B19BCE
    pub const CYAN: (f64, f64, f64) = (0.627, 0.886, 0.918); // #A0E2EA
    pub const PINK: (f64, f64, f64) = (0.949, 0.651, 0.753); // #F2A6C0
    /// Near-black base the macOS LavaLamp fills behind the blobs (`white: 0.08`).
    pub const BASE: (f64, f64, f64) = (0.04, 0.04, 0.05);
}

const CSS: &str = r#"
@define-color fileid_gold     #FFCC00;
@define-color fileid_gold_dim #CCA300;
@define-color fileid_lavender #B19BCE;
@define-color fileid_cyan     #A0E2EA;
@define-color fileid_pink     #F2A6C0;

/* GlassCard / ultraThinMaterial: faint white fill + hairline border. */
.glass-card {
    background-color: alpha(#FFFFFF, 0.06);
    border: 1px solid alpha(#FFFFFF, 0.08);
    border-radius: 12px;
}

/* The muted "material" layer that sits between the LavaLamp and the UI,
   mirroring macOS's Color(white:0.08) + .ultraThinMaterial overlay. */
.fileid-scrim {
    background-color: alpha(#0A0A0C, 0.55);
}

/* Tab page roots stay transparent so the scrim + LavaLamp read through. */
.fileid-tab { background-color: transparent; }

/* Header bar floats over the LavaLamp like the macOS toolbar. */
.fileid-headerbar { background-color: transparent; box-shadow: none; }
headerbar.fileid-headerbar { background: transparent; }

/* Search field — translucent capsule, mirrors the macOS search pill. */
.fileid-search {
    background-color: alpha(#FFFFFF, 0.06);
    border: 1px solid alpha(#FFFFFF, 0.08);
    border-radius: 18px;
    padding: 2px 8px;
}

/* Gold segmented filter pills (All / Images / Videos / …). */
.pill {
    background-color: alpha(#FFFFFF, 0.06);
    border-radius: 14px;
    padding: 2px 12px;
    color: alpha(#FFFFFF, 0.75);
    font-weight: 500;
}
.pill:hover { background-color: alpha(#FFFFFF, 0.12); }
.pill-active {
    background-color: @fileid_gold;
    color: #000000;
    font-weight: 700;
}

/* Primary CTA — gold fill, black label (Start Scan, Apply, Pick folder). */
.gold-button {
    background: @fileid_gold;
    color: #000000;
    font-weight: 600;
    border: none;
    box-shadow: none;
}
.gold-button:hover { background: shade(@fileid_gold, 1.08); }
.gold-button:disabled { background: alpha(@fileid_gold, 0.35); color: alpha(#000000, 0.5); }

.gold-accent  { color: @fileid_gold; }
.lavender-accent { color: @fileid_lavender; }

/* File tile — opaque carrier so thumbnails crisp-render over the LavaLamp. */
.file-tile {
    background-color: alpha(#FFFFFF, 0.04);
    border: 1px solid alpha(#FFFFFF, 0.08);
    border-radius: 10px;
    padding: 6px;
}
.file-tile:hover { border-color: alpha(#FFFFFF, 0.18); }
.file-tile-selected { border: 2px solid @fileid_gold; }

.tile-thumb {
    background-color: alpha(#000000, 0.25);
    border-radius: 8px;
}

.kind-badge {
    background-color: @fileid_gold;
    color: #000000;
    border-radius: 9px;
    padding: 1px 6px;
    font-size: 8pt;
    font-weight: 700;
}

.tile-caption { color: alpha(#FFFFFF, 0.55); font-size: 9pt; }
"#;

/// Install the brand CSS into the default display and force dark mode.
///
/// Called once from `connect_startup`. Force-dark matches macOS + Windows
/// (the app is dark-only by design); a light override can come later via
/// Settings.
pub fn install() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark);
}
