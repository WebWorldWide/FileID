# FileID Prototype v3 — cross-platform design contract

Source of truth: the claude.ai/design project `FileID Prototype v3.dc.html` (three areas). macOS implements it first (`platforms/apple`); Windows (WinUI 3) and Linux (GTK4/libadwaita) port it 1:1 from this contract. Native primitives only.

## Geometry

- **Subtle continuous corners** (revised 2026-10-15 at the owner's request, after the cosmic-studio reference): radius xs 4 / s 6 / m 8 / l 10 with continuous curvature. Controls, chips and inline fills use s; cards, tiles and selection rings use m; floating cards (scan card, popovers) use l. Progress bars and the toggle track are capsules; radios are circles. A stroke always uses the same radius as the clip it outlines. Hairlines, 1–3 px rules and hit-test shapes stay square.
- **1 px hairlines** split everything: `rgba(255,255,255,.08)` default, `.06` soft (list rows), `.14` strong (panel/control borders), `.18` control border.
- Sidebar, page and side panes sit flush, separated by hairlines. Panels float on `rgba(24,24,27,.78)` with a strong hairline.
- Spacing: page inset 26 px horizontal / 20 px top; panel padding 14 px horizontal / 10 px vertical; grid gaps 12 px; tiles `minmax(150px,1fr)`, people tiles 1:1, library tiles 4:3.

## Color

| Token | Value | Use |
| --- | --- | --- |
| bg0 / bg1 | `#070708` / `#0B0B0C` | window, title bar base |
| text1 / text2 / text3 | `#F0F1F2` / `#CFD3D8` / `#A6ACB3` | primary / secondary / meta |
| label | `#AEB4BB` | mono section labels |
| gold | `#FFCC00` (hover `#FFD84D`, text on gold `#141414`) | the only action/selection accent |
| ai (lavender) | `#B19BCE` | a model is running; unverified model evidence |
| info (cyan) | `#A0E2EA` | information, speech, idle |
| done (pink) | `#F2A6C0` | finished work |
| danger / warning | `#FF6A5E` / `#E0A340` | destructive, offline drive |
| paused | `rgba(185,190,196,.5)` | paused bars |

Selection wash: `rgba(255,204,0,.07)` background + 3 px gold rail on the left of a selected row.

## Type

- **Instrument Serif** (SIL OFL, bundled `Resources/Fonts`): page titles 34 px, window wordmark 18–20 px, hero/empty titles 28–56 px, people initials 44 px. Regular weight only.
- **Archivo** (SIL OFL, bundled, variable `wdth,wght`): all UI text. 13–14.5 px, weights 500/600.
- **Mono** (revised 2026-10-15: UI text, labels, counts and section headers use Archivo; mono is kept for paths, filenames, shortcuts and code-like values) (platform mono: SF Mono on macOS, Cascadia Mono on Windows, the system monospace on GNOME — JetBrains Mono in the prototype): section labels 11 px / 600 / letter-spacing .14em / uppercase; meta 11–11.5 px; control labels 11.5–12 px; metric values 20 px / 600.

## Shell

```
┌ Title bar (48 px macOS / 42 px Windows) ─────────────────────────────────────┐
│ [traffic lights] [FileID | <Folder> ▾]   [Browse | Analyze | Act]   [Search or ask ⌘K] [■ ACTIVITY ▁▁ 62%] [⚙] │
├ Sidebar 232 px ─┬ Page ───────────────────────────────────────────────────────┤
│ AREA  3 pages   │ AREA / PAGE  (mono crumb)                        page meta   │
│ ▌Library  ⌘1    │ Page title (serif 34)                                        │
│  meta line      │ ───────────────────────────────────────────────────────────── │
│  People   ⌘2    │ page content (panels / split panes)                          │
│  …              │                                                              │
│ ─────────────── │                                                              │
│ pipeline block  │                                                              │
└─────────────────┴──────────────────────────────────────────────────────────────┘
```

- **Areas**: Browse = Library, People, Search & Moments · Analyze = Deep Analyze, Chat, Best Takes · Act = Cleanup, Restructure, File Tools. Settings is reached from the gear; its crumb is `FileID / Settings`. ⌘1–⌘9 (Ctrl on Windows/Linux) jump to pages in that order; ⌘, opens Settings; ⌘K focuses search; Esc closes popovers.
- **Folder switcher** popover: current folder with path and file count, Add folder…, Rescan, Clear folder, Wipe library + rescan (destructive, confirmed).
- **Search or ask**: one field. Results group into Ask Chat (lavender), Library (query handoff), Pages (name · area · shortcut). Enter picks the first row.
- **Activity** button: label + mini grained bar + percent; colour gold while scanning/exporting/moving, lavender while a model runs, cyan idle. Its popover lists each job (name, sub, state, bar, Pause/Resume or Cancel, Open <page>).
- **Sidebar** (revised 2026-10-15 for readability): one list grouped Browse / Analyze / Act under small uppercase headers; 32 px rows with icon, 13.5 px label and a trailing badge only when it says something (file count, Setup, live percent, Plan); the selected row is a radius-m gold tint. Shortcuts live in the tooltip. The footer scan card (radius l) shows title + percent, a 6 px capsule bar, a five-segment step indicator with "Step N of 5 · Stage", one sub line, collapsible counters (found, tagged, rate, memory, failures) and the scan controls (Scan / Pause / Resume / Cancel / Rescan).
- **Toast** bottom-centre: pink dot, mono text, optional gold outline Undo/Open button, auto-dismiss 5–7 s.

## Components

- **Panel**: panel fill + strong hairline; header row = mono section label + trailing controls, bottom hairline; footer row on `rgba(255,255,255,.02)` with top hairline.
- **Metric cell**: label (mono), value (mono 20/600), unit (mono 11.5 text3); cells separated by vertical hairlines.
- **Key/value row**: 80 px uppercase mono key + 13 px value, soft hairline below.
- **Segmented bar**: hairline-bordered row of 24–28 px buttons, selected = gold fill with `#141414` text.
- **Buttons**: primary gold 30 px (28/32 variants), secondary hairline on `rgba(255,255,255,.04)`, ghost text-only, accent-outline (gold 60% border, 10% fill) for Accept/Undo, danger red outline.
- **Toggle**: 30×16 capsule track, 12 px round knob, gold when on. **Radio**: 12 px circle with 6 px gold dot. **Check**: 14 px, radius 4, gold fill with ✓.
- **Chips**: tag chips mono 11 with strong hairline; kind badges mono 10/600 uppercase on `rgba(0,0,0,.55)`; status pills (ACCEPTED/KEPT) outlined.
- **Progress bar**: square track `rgba(255,255,255,.07)`, fill = gradient drawn across the full track and masked to the fraction (gold `#FF8A1A→#FFB000→#FFCC00→#FFE9A3`, lavender `#7462A6→#B19BCE→#DED2F2`, done `#FFB000→#FFCC00→#F2A6C0`, paused grey), fine grain overlay at ≈0.4 opacity, width animates 0.45 s.
- **Grain**: a 140 px tile of smoothed multi-octave noise around mid-grey (prototype: SVG fractalNoise, baseFrequency 0.95, 3 octaves) at 2× pixel density; ≈0.14 over the window, ≈0.4 over bars, overlay blend. Never per-pixel static.

## Background (LavaLamp v3)

Vertical base `#0B0B0C → #0F0A08 (42%) → #1B0D06 (78%) → #2A1206`, a warm radial glow rising from the bottom (`rgba(255,128,32,.55)` centre), six blurred wax blobs (28 px blur, screen blend) that rise 80% of the height over 46–72 s and sway 13–21 s, a top shade `rgba(11,11,12,.7 → 0)`, and a lavender radial glow that fades in over 2.4 s while a model is running. Modes: Full, Calm (1.7× slower, 55% wax), Off (static glow at 40%). Reduce Motion pauses the animation.

## Pages (content contracts)

- **Library**: filter row (filter field 320 px, kind segmented bar, "N of M shown", Select); 4:3 tiles with KIND badge, duration, OFFLINE badge, moments count (lavender), name 13/500, mono size · date; selected tile has a 2 px gold ring; 300 px Selected pane (16:10 preview, name, key/value rows Kind/Captured/Size/Length/People, tag chips, Moments/Export/Ask Chat actions).
- **People**: Confirm-faces queue panel (initials tile, "Is this **Name**?", file · match, Yes/No/Skip); 1:1 person tiles with initials fallback (serif 44), unnamed tiles dashed with text3 name.
- **Search & Moments**: 300 px results pane (rail rows: name, mono meta, text), player card with Examined / Chapters / Speech tracks and ruler, Chapters panel (time, title, Draft/Accepted/Edited pill, confidence, Accept/Rename/Delete), Analysis queue panel.
- **Deep Analyze**: run panel (This run · model, state, bar, 5 metric cells, LAST · file + Pause/Resume), Proposed names panel (from → to, why, Accept/Keep name), Model radio list, Files scope segmented bar, Propose-names toggle, Log.
- **Chat**: Looking-in chips, messages with mono role labels (You gold, model lavender), citation chips `[n] name · time`, textarea + Send/Stop, model toggle; 300 px Sources pane.
- **Best Takes**: 260 px Groups pane with Possible-group suggestion (lavender wash), Goal field, recommendation row with tone rail, take cards (TAKE n, outcome, why, source colour, Sharpness bar, Met/Missed/Unclear, ☆ Prefer).
- **Cleanup**: metric row (Groups, Extra copies, Space, Kept) + Trash all extra copies; group panels (kind label, reason, Trash n) with KEEP/TRASH tiles (keeper 2 px gold ring, others 60% opacity).
- **Restructure**: three step cells (Preview · Review · Move; macOS has no shortcut mode), Sort by + Pattern + Use Deep Analyze names, metric row, Now/After panels (stacked colour bars, tree with colour dots), Moves sample (From → To) with status/bar and Make shortcuts / Move files / Remove shortcuts / Undo move.
- **File Tools**: 01 · Recipe (Photo/Video/Chapters, file checklist), 02 · Output (aspect tiles 30 px frames, Resolution, format bars), 380 px 03 · Preview pane (gold-framed preview, outputs with bars, Preview/Export/Undo).
- **Settings**: Appearance (Lava lamp Full/Calm/Off), Processing radios, Models (INSTALLED · size in pink), Index (Stored, Size, Delete index… danger).
- **Welcome**: icon + serif title + mono subtitle, `1 · Install models` rows (name, job, Required/Optional, status, bar), `2 · Choose a folder`, Later / Install all · size / Scan <folder>.

## Status

- macOS: every page is on v3 — shell, sidebar, title bar, pipeline block, design system (focus rings, 28 pt targets, Reduce Motion), lava, grain, fonts, Library (Selected pane), People, Cleanup, Settings, Welcome, Search & Moments (timeline tracks), Chat (Sources pane), Best Takes, File Tools, Restructure (step cells). Measured layout for the ports: [DESIGN_V3_PORT.md](DESIGN_V3_PORT.md).
- GTK: shell landed (title bar, area sidebar, scan card, page headers, corner scale, bundled Archivo/Instrument Serif, high contrast). Pages next, from DESIGN_V3_PORT.md.
- WinUI: shell landed (48 px title bar, area sidebar, scan card, page headers, radius scale; workbenches are pages). CI-verified only. Pages next. Keep `LavaLampBackground` ports (Win2D / GTK snapshot) on the v3 recipe.
