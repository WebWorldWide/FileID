# Visual language — palette, motion, materials

The macOS app is the canonical visual reference. Windows (WinUI 3) and future-Linux ports are 1:1 visual reproductions. This file is the language-neutral source of truth for the visual tokens; per-platform Theme files (`Theme.swift`, `Theme.xaml`, future Linux `theme.rs` or stylesheet) reference these values.

## Palette

| Role | Hex | RGB (0–1) | When to use |
|---|---|---|---|
| **Gold** (primary accent) | `#FFCC00` | `1.00, 0.80, 0.00` | Primary CTAs (Apply, Start Scan, Pick a folder). Tab selection. Value-prop moments. |
| Gold dim | `#CCA300` | `0.80, 0.64, 0.00` | Disabled gold buttons; secondary gold surfaces. |
| **Lavender / "ai"** | `#B19BCE` | `0.69, 0.61, 0.81` | An on-device AI is doing work *right now* (Deep Analyze running, face clustering, ArcFace embed, "Verify with AI"). |
| **Cyan / "info"** | `#A0E2EA` | `0.63, 0.89, 0.92` | Secondary status / informational banners ("3 files selected", "Last scan completed in 4 m"). |
| **Pink / "delight"** | `#F2A6C0` | `0.95, 0.65, 0.75` | Success / completion moments (renamed, grouped, moved). Briefly. Never as a permanent fixture. |

Apply gold sparingly — it is the primary color, not a wallpaper. The iridescent palette (lavender / cyan / pink) is for *moments*, not surfaces.

## Surface tokens

| Token | Color | Opacity | Notes |
|---|---|---|---|
| `surface.base`   | `#000000` | 0.30 | Behind glass cards on a fully-blurred background. |
| `surface.card`   | `#FFFFFF` | 0.06 | Tinted overlay on the inside of a glass card. |
| `surface.border` | `#FFFFFF` | 0.08 | 1px stroke on glass cards + segmented controls. |

## Spacing scale

`xs = 4`, `s = 8`, `m = 16`, `l = 24`, `xl = 40` — all in points (macOS) or DIPs (Windows). Stick to the scale.

## Corner radii

`s = 8`, `m = 12`, `l = 16`. GlassCard uses `m`. Buttons + pills use Capsule (= half height).

## Materials

| macOS | Windows | Role |
|---|---|---|
| `.ultraThinMaterial` | WinUI 3 `<AcrylicBrush TintColor="#000000" TintOpacity="0.5" FallbackColor="#1A1A1A"/>` driven via Composition `DesktopAcrylicController` | GlassCard surface |
| `NSVisualEffectView .underWindowBackground` | `MicaController` (Win11) on `Window.SystemBackdrop`, falling back to a flat `#0E0E0E` on Win10 22H2 | Window backdrop behind LavaLamp |
| `.ultraThinMaterial` over LavaLamp | `<AcrylicBrush TintColor="#000000" TintOpacity="0.35"/>` over LavaLamp | Composited overlay so foreground content stays legible |

## LavaLampBackground

The user's favorite. Three blurred ellipses drifting under a translucent overlay.

- Background: solid `#141414` (RGB 0.08, 0.08, 0.08)
- Three ellipses with sinusoidal position modulation:

| Ellipse | Diameter (px) | Color | Opacity | x-rate (rad/s) | y-rate (rad/s) |
|---|---|---|---|---|---|
| 1 | 800 | gold `#FFCC00` | 0.40 | 0.20 | 0.23 |
| 2 | 600 | red-orange `#FF6600` | 0.30 | 0.15 | 0.18 |
| 3 | 1000 | dark `#0D0D0D` | (`white = 0.05`, opaque) | 0.10 | 0.12 |

Position formula (per ellipse `i`):
```
cx_i = w/2 + sin(t * x_rate_i) * w * x_amp_i
cy_i = h/2 + cos(t * y_rate_i) * h * y_amp_i
```
where `x_amp = [0.3, 0.4, 0.2]` and `y_amp = [0.3, 0.4, 0.2]` for the three ellipses.

- Gaussian blur radius: **120 px**
- Composited via offscreen buffer (`drawingGroup` on macOS, `RenderTargetBitmap` on Windows) so the parent doesn't re-rasterize the blur every frame
- Vsync-driven (TimelineView on macOS, `CompositionTarget.Rendering` on Windows)
- **Pause when occluded / minimized.** Saves CPU/GPU when invisible.

Above the LavaLamp: a `surface.base` (#000 @ 30%) rectangle with `colorScheme = .dark` to mute the brightness so foreground content stays legible.

## Motion primitives

| Primitive | Duration | Easing | Notes |
|---|---|---|---|
| `ShimmerView` (loading-tile gold→lavender sweep) | 1.6 s | linear, infinite | Disabled under reduced-motion |
| `CompletionRipple` (gold ring on success) | 0.9 s | easeOut | Scale 0.4 → 2.6, opacity 0.85 → 0. Disabled under reduced-motion |
| `IridescentBorder` (rotating sweep gradient) | 14 s | linear, infinite | Stops gold → delight → ai → info → gold. Static gold under reduced-motion |
| Tile entrance (Library grid) | 0.25 s | spring, response 0.35, dampingFraction 0.78 | `.opacity * .scale(0.96)` |
| Person card entrance (People grid) | 0.35 s | spring, response 0.35, dampingFraction 0.78 | `.scale(0.92) + .opacity` |
| Restructure expand/collapse | 0.40 s | spring, response 0.40, dampingFraction 0.80 | |
| Restructure apply bar entrance | 0.40 s | spring, response 0.40, dampingFraction 0.80 | `.move(edge: .bottom) + .opacity` |
| Tab auto-switch | 0.25 s | easeInOut | |

### Spring math

When the platform doesn't have a native spring API matching SwiftUI's `(response, dampingFraction)`, solve the underdamped second-order ODE:

```
ω_n = 2π / response
ω_d = ω_n * sqrt(1 - ζ²)              where ζ = dampingFraction
y(t) = 1 - e^(-ζ ω_n t) * (cos(ω_d t) + (ζ / sqrt(1 - ζ²)) * sin(ω_d t))
```

WinUI 3 has `SpringScalarNaturalMotionAnimation` which takes `Period = response` and `DampingRatio = dampingFraction` directly — no math port needed.

## Reduced motion

Every motion primitive checks the OS reduce-motion flag:
- macOS: `@Environment(\.accessibilityReduceMotion)`
- Windows: `Microsoft.UI.Xaml.UIElement.AreAnimationsAllowed` / `UISettings.AnimationsEnabled`

When reduce-motion is on:
- Springs collapse to a 0.15 s linear opacity fade
- ShimmerView disables (static surface)
- CompletionRipple disables (no pulse, just a brief gold flash that snaps in/out)
- IridescentBorder freezes at gold
- LavaLampBackground keeps animating but at half rate (still feels alive, much less motion)

## Typography

Sized in points (macOS) / DIPs (Windows), using the OS default UI font.

- Captions / labels: 10–12 pt
- Body: 13 pt
- Headers: 17 pt semibold
- Hero (preview-sheet-title-style): 24 pt bold

Numerics (file counts, percentages) use `tabular nums` so columns align in lists.

## Buttons

- **GoldButton** (`Theme.GoldButton` / Windows `<GoldButton/>`): primary CTA. Black text on gold pill. Borderless prominent.
- **Pill buttons** (segmented control, toggle picker): gold pill on selected, white-8% surface on inactive.
- **Subtle text buttons**: `secondary` foreground, capsule hit area, no fill.

## Iconography

SF Symbols on macOS. Segoe Fluent Icons on Windows (or shipped SVG fallbacks for icons that lack a Segoe equivalent — TBD list as Phase 1 lands).
