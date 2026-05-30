# Visual language — palette, motion, materials

The macOS app is the canonical visual reference. Windows (WinUI 3) and the
deferred Linux port are 1:1 visual reproductions, built on native primitives
(no web tech). This file is the platform-neutral source of truth for the visual
tokens; per-platform theme files reference these values:

- macOS — `platforms/apple/.../Theme/Theme.swift`
- Windows — `platforms/windows/.../FileID.Theme/Theme.xaml`

When a platform theme file and this doc disagree, `Theme.swift` wins.

## Palette

| Role | Hex | RGB (0–1) | When to use |
|---|---|---|---|
| **Gold** (primary accent) | `#FFCC00` | `1.00, 0.80, 0.00` | Primary CTAs (Apply, Start Scan, Pick a folder). Tab selection. Value-prop moments. |
| Gold dim | `#CCA300` | `0.80, 0.64, 0.00` | Disabled gold buttons; secondary gold surfaces. |
| **Lavender / "ai"** | `#B19BCE` | `0.69, 0.61, 0.81` | An on-device AI is doing work *right now* (Deep Analyze running, face clustering / SFace embed, "Verify with AI"). |
| **Cyan / "info"** | `#A0E2EA` | `0.63, 0.89, 0.92` | Secondary status / informational banners ("3 files selected", "Last scan completed in 4 m"). |
| **Pink / "delight"** | `#F2A6C0` | `0.95, 0.65, 0.75` | Success / completion moments (renamed, grouped, moved). Briefly. Never as a permanent fixture. |

Apply gold sparingly — it is the primary color, not a wallpaper. The iridescent
palette (lavender / cyan / pink) is for *moments*, not surfaces.

Note: `#FFCC00`, not `#FFD700`. The SwiftUI literal is `Color(red: 1.0, green: 0.8, blue: 0.0)`.

### Secondary tokens

| Token | Color | Notes |
|---|---|---|
| Destructive | `#E5A5A5` text on a muted dark-red fill | Clear-folder, trash, wipe. |
| Tag chip | `#FFCD3C` foreground @ 0.85 on `#FFCD3C` @ 0.10 | Auto/AI tag chips. |
| Tag chip (kind) | `#FFFFFF` on `#808080` @ 0.30 | Neutral file-type label leading each card's chip row. |

## Surface tokens

| Token | Color | Opacity | Notes |
|---|---|---|---|
| `surface.base`   | `#000000` | 0.30 | Mutes a fully-blurred backdrop so foreground content stays legible. |
| `surface.card`   | `#FFFFFF` | 0.06 | Tinted overlay on the inside of a glass card. |
| `surface.border` | `#FFFFFF` | 0.08 | 1px stroke on glass cards + segmented controls. |

Gold-tinted selection states layer on top: selected background `gold @ 0.18`,
selected stroke `gold @ 0.55`, inactive fill `white @ 0.08`.

## Spacing scale

`xs = 4`, `s = 8`, `m = 16`, `l = 24`, `xl = 40` — points (macOS) or DIPs
(Windows). Stick to the scale.

## Corner radii

`s = 8`, `m = 12`, `l = 16`. GlassCard uses `m`. Buttons + pills use a capsule
(= half height).

## Materials

The cards are real glass — DWM/AppKit-rendered translucency, never a software
approximation. A `surface.base` mute + `surface.card` tint + 1px `surface.border`
on top is what gives them depth.

| Surface | macOS | Windows |
|---|---|---|
| Window backdrop (behind LavaLamp) | `NSVisualEffectView .underWindowBackground` | `MicaController` (`MicaKind.Base`, dark) on the window's system backdrop; falls back to `DesktopAcrylicController` where Mica is unsupported (Win10 22H2) |
| GlassCard surface | `.ultraThinMaterial` | XAML `AcrylicBrush` (`TintColor #000000`, `TintOpacity 0.5`, `TintLuminosityOpacity 0.65`, `FallbackColor #1A1A1A`) + `surface.card` tint + `surface.border` stroke |

## LavaLampBackground

The user's favorite — the visual signature of FileID. Three soft-edged ellipses
drifting under a translucent overlay, on a solid `#141414` (`white 0.08`) base.

| Ellipse | Diameter (px) | Color | Center opacity | x-rate (rad/s) | y-rate (rad/s) | amplitude |
|---|---|---|---|---|---|---|
| 1 | 800 | gold `#FFCC00` | 0.40 | 0.20 | 0.23 | 0.30 |
| 2 | 600 | orange `#FF6600` | 0.30 | 0.15 | 0.18 | 0.40 |
| 3 | 1000 | dark `#0D0D0D` | 0.55 | 0.10 | 0.12 | 0.20 |

Position formula (per ellipse `i`, amplitude shared on both axes):
```
cx_i = w/2 + sin(t * x_rate_i) * w * amp_i
cy_i = h/2 + cos(t * y_rate_i) * h * amp_i
```

The soft falloff is rendered differently per platform but reads identically:

- **macOS** — `Canvas` fills sharp-edged ellipses (opacities 0.4 / 0.3 / opaque
  `white 0.05`) under a 120px Gaussian blur, composited to its own offscreen via
  `drawingGroup` so the parent doesn't re-rasterize the blur each frame.
  Vsync-driven by `TimelineView(.animation)` (drives ProMotion at 120 Hz).
- **Windows** — three `SpriteVisual`s on `Microsoft.UI.Composition`, each with a
  `CompositionRadialGradientBrush` falloff from `color @ center opacity` to
  `color @ 0` (a true hardware soft edge, no Gaussian-blur effect). Motion is a
  GPU-continuous `ExpressionAnimation`: two linear phase oscillators per visual
  drive `Sin(xPhase)` / `Cos(yPhase)`, evaluated every vsync by the compositor.
  Win2D's `CanvasAnimatedControl` is deliberately avoided — it fast-fails in
  CoreMessagingXP on Win11 build 26200+.

Above the LavaLamp sits a `surface.base` (`#000` @ 30%) rectangle to mute the
brightness. Both platforms **pause when occluded / minimized** (macOS `paused`
flag; Windows `XamlRoot.IsHostVisible`).

## Motion primitives

Durations are centralized in each theme file so retiming is global. Values below
are platform-neutral.

| Primitive | Duration | Easing | Notes |
|---|---|---|---|
| `ShimmerView` (loading-tile gold→lavender diagonal sweep) | 1.6 s | linear, infinite | Frozen under reduced-motion |
| `CompletionRipple` (gold ring on success) | 0.9 s | easeOut | Scale 0.4 → 2.6, opacity 0.85 → 0. Fires on any trigger change; skipped under reduced-motion |
| `IridescentBorder` (rotating sweep gradient) | 14 s | linear, infinite | Stops gold → delight → ai → info → gold. Static gold under reduced-motion |
| Tile entrance (Library grid) | 0.35 s | spring, response 0.35, dampingFraction 0.78 | `.opacity * .scale(0.96)` |
| Person card entrance (People grid) | 0.35 s | spring, response 0.35, dampingFraction 0.78 | `.scale(0.92) + .opacity` |
| Restructure expand/collapse | 0.40 s | spring, response 0.40, dampingFraction 0.80 | |
| Restructure apply-bar entrance | 0.40 s | spring, response 0.40, dampingFraction 0.80 | `.move(edge: .bottom) + .opacity` |
| Sankey entrance | 0.55 s | spring | Primary reorg visualization (see below) |
| Tab swap (detail host) | 0.22 s | sine easeInOut | Opacity crossfade out→in |
| Sidebar toggle | 0.20 s | — | |
| Stat-tile hover | 0.18 s | easeInOut | |

### Spring tokens

Two springs cover almost everything: **standard** (response 0.40, dampingFraction
0.80) and **tight** (response 0.35, dampingFraction 0.78). `response` is the
period of one oscillation; `dampingFraction ∈ [0,1]`, where 1.0 is critically
damped (no overshoot).

Both platforms use a native spring solver — SwiftUI `.spring(response:dampingFraction:)`
and WinUI 3 `SpringScalarNaturalMotionAnimation` (`Period = response`,
`DampingRatio = dampingFraction`). No math port. If a future platform lacks a
native spring, solve the underdamped second-order ODE directly:

```
ω_n = 2π / response
ω_d = ω_n * sqrt(1 - ζ²)              where ζ = dampingFraction
y(t) = 1 - e^(-ζ ω_n t) * (cos(ω_d t) + (ζ / sqrt(1 - ζ²)) * sin(ω_d t))
```

## Reduced motion

Every motion primitive checks the OS reduce-motion flag:
- macOS: `@Environment(\.accessibilityReduceMotion)`
- Windows: `UISettings.AnimationsEnabled` (+ `AnimationsEnabledChanged`), surfaced
  process-wide via the `ReducedMotion` singleton

When reduce-motion is on:
- Springs snap to their final state (no animated transition)
- ShimmerView freezes (static surface)
- CompletionRipple skips the pulse (the completion still happens, just undecorated)
- IridescentBorder freezes at gold
- LavaLampBackground keeps drifting at half rate (still alive, much less motion)

## Typography

Sized in points (macOS) / DIPs (Windows), using the OS default UI font.

- Badge: 10 (semibold)
- Caption / label: 11
- Body: 13
- Title: 17 (semibold)
- Hero (preview-sheet title): 24 (bold)

Numerics (file counts, percentages) use tabular figures so columns align in lists.

## Buttons

- **GoldButton**: primary CTA. Black semibold text on a gold pill; borderless,
  prominent. Hover/pressed darken via a black overlay (0.08 / 0.16); disabled
  drops to 0.45 opacity.
- **Pill buttons** (segmented control, toggle picker): gold pill when selected,
  `white @ 0.08` surface when inactive.
- **Subtle text buttons**: secondary foreground, capsule hit area, no fill.

## Reorg visualization (Sankey)

The Sankey diagram is the primary view for Restructure: source-folder → category
flows, with ribbon thickness proportional to file count and barycentre node
ordering to minimize crossings. It links source folders to the engine's
classifier categories from a `RestructurePlan`. Hover highlights the path and
shows count/destination; click invokes drill-down.

Current implementation is pure-XAML (`Path` + cubic-bezier `BezierSegment`,
GPU-composited at vsync). A Win2D upgrade (destination-color links, Okabe-Ito
CVD-safe palette, end-to-end hover highlight, animated drill-down) is planned —
see `RESTRUCTURE.md` (Phase 4). It is paired with a before/after tree as the
per-folder confirmation view.

## Iconography

SF Symbols on macOS; Segoe Fluent Icons on Windows, with shipped SVG fallbacks
for the few glyphs Segoe lacks. Revisit the fallback list only when a new view
introduces a glyph Segoe Fluent doesn't cover.
