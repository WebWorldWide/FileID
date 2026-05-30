# SF Symbols → Segoe Fluent mapping

Cross-platform icon parity. macOS draws icons with SF Symbols
(`Image(systemName:)`); the WinUI 3 app draws them with Segoe Fluent Icons
(`<FontIcon Glyph="&#xEXXX;"/>`). The two glyph sets have no 1:1 visual
mapping — some pairs read identically, others diverge in metaphor, stroke
weight, or fill. This doc records the chosen Segoe glyph for each role and
flags the sites that still need a side-by-side visual check.

Scope: the WinUI app icons under
`platforms/windows/src/FileID.App/Views/**/*.xaml` versus the macOS reference
under `platforms/apple/app/Sources/FileID/Views/**/*.swift`. Some Windows
glyphs are bound at runtime (`Glyph="{x:Bind …}"`) rather than literal hex
(model-status icons in `WelcomeSheet.xaml`, kind badges in `LibraryView.xaml`);
those live in code-behind, not in the table below.

## Process

1. Find a `Glyph=` site in the WinUI views.
2. Find the matching `Image(systemName: "name")` for the same role in the
   macOS views.
3. Compare:
   - macOS: SF Symbols app → search by name → Default style.
   - Windows: Segoe Fluent chart
     (https://learn.microsoft.com/en-us/windows/apps/design/style/segoe-fluent-icons-font)
     → search by hex.
4. If the Windows glyph reads obviously different, pick a closer Segoe glyph
   and update the table. If nothing is close, fall back to a text label
   (Windows convention permits text-only buttons).

## Core mappings

Roles whose Segoe glyph appears as a literal hex (or code-behind constant) in
the current app and reads acceptably against the SF Symbol. Every hex below is
verified present in the source.

| Role | SF Symbol (macOS) | Segoe Fluent | Hex | Notes |
|---|---|---|---|---|
| Folder / folder picker | `folder.fill` | Folder | E8B7 | Match |
| Search | `magnifyingglass` | Search | E721 | Match |
| Tag | `tag.fill` | TagGroup | E8EC | Slightly heavier stroke; reads OK |
| Rename / wand | `wand.and.rays` | Rename | E8AC | Match |
| Checkmark | `checkmark` / `checkmark.circle.fill` | CheckMark | E73E | Match |
| Download | `arrow.down.circle` | Download | E896 | Code-behind constant (`WelcomeSheet`), `EmptyStateView` action |
| Warning | `exclamationmark.triangle.fill` | Warning | E7BA | Match |
| People / person | `person.crop.circle` | People / Contact | E716 / E77B | Match |
| Sparkles / AI | `sparkles` / `sparkle.magnifyingglass` | Lightbulb | E945 | No direct Segoe match; "tip/smart" analog, on `AiBrush`. Also stands in for `brain.head.profile` (semantic search) |
| Right chevron | `chevron.right` | ChevronRight | E76C | Match |
| Left chevron | `chevron.left` | ChevronLeft | E76B | Match |
| Reveal in file manager | `arrow.up.right.square` | OpenInNewWindow | E838 | Acceptable |
| Open file | `arrow.up.right` | OpenFile | E8E5 | Match |
| Copy path | `doc.on.clipboard` | Copy | E8C8 | Match |
| Trash | `trash.fill` | Delete | E74D | Match |
| Faces badge | `person.crop.circle.fill` | Contact | E8D4 | Tile/preview face badge |
| Text / OCR badge | `text.viewfinder` | TextDocument | E8E9 | Tile/preview OCR badge |
| Library / photos | `photo` | Photo | E91B | Match |
| Restructure / branch | `arrow.triangle.branch` | (Restructure header) | E77B | Match |
| Symlink / shortcut apply | `link` | Link | E71B | Match |
| Move / convert | `arrow.right` | Forward | E8AB | Match |
| Resume / play | `play.fill` | Play | E768 | Match |
| Cancel / clear | `xmark.circle.fill` | Cancel / Clear | E711 / E894 | Match |
| Sync / refresh | `arrow.triangle.2.circlepath` | Refresh | E72C | Match |
| Phase / processing | `hourglass` / phase icon | (gear) | E895 | Generic static "working" glyph; use E72C (Refresh) for a rotating spinner |

All FontIcon glyphs are written as numeric XML escapes (`&#xE8B7;`) rather
than raw Unicode, so the source bytes stay ASCII and survive the CI charset
check.

## Sites needing verification (pixel-level)

Chosen pragmatically but not yet walked against the SF Symbol on real
hardware. Walk both apps side-by-side and confirm or replace. Counts are
literal `Glyph=` occurrences in the current file (some are bound, not hex).

| File | Glyph sites | Cross-reference (macOS) |
|---|---|---|
| `Views/Library/LibraryView.xaml` | 16 | `LibraryView.swift` — toolbar, tile kind/faces/text badges, empty states |
| `Views/Cleanup/CleanupView.xaml` | 10 | `CleanupView.swift` — toolbar + dedupe group rows |
| `Views/Library/FilePreviewSheet.xaml` | 9 | `LibraryView.swift` detail — nav, actions, photo placeholder (line 187), badges |
| `Views/People/PeopleView.xaml` | 7 | `PeopleView.swift` — toolbar, suggested-merge, empty states |
| `Views/OnboardingSplash.xaml` | 7 | `Detail.swift` / `EmptyStateView.swift` feature-row icons |
| `Views/Sidebar/SidebarProcessingControl.xaml` | 6 | `Sidebar/SidebarProcessingControl.swift` — phase / pause / play |
| `Views/DeepAnalyze/DeepAnalyzeView.xaml` | 5 | `DeepAnalyzeViews.swift` |
| `Views/Restructure/RestructureView.xaml` | 4 | `RestructureView.swift` + `Restructure/*.swift` (apply bar) |
| `Views/Sidebar/SidebarFolderHeader.xaml` | 3 | `Sidebar.swift` header |

The Restructure surface is the live area: macOS uses a Win2D/SwiftUI Sankey
(`Restructure/SankeyFlowView.swift`) plus `arrow.triangle.branch`,
`rectangle.3.offgrid`, and `checkmark.seal.fill`. As the Win2D
`SankeyFlowControl` lands on Windows, sync its node/flow glyphs here.

## Out-of-scope (Apple-only)

No Windows concept, so no mapping:

- `command` / `option` / `shift` modifier glyphs — Windows uses Ctrl/Alt/Shift
  text labels.
- `apple.logo`.
- Liquid-glass / glassmorph symbols (macOS 26+ only).

## Adding a new icon

1. Find the SF Symbol the macOS view uses for the same role.
2. Pick a Segoe glyph with a similar metaphor and stroke weight.
3. Add the row above (or update the site count).
4. If no close match exists, use a text label.

## Source

- macOS SF Symbols: `platforms/apple/app/Sources/FileID/Views/**/*.swift`
  (`Image(systemName:`).
- Windows Segoe Fluent: `platforms/windows/src/FileID.App/Views/**/*.xaml`
  (`Glyph=`).
- Segoe Fluent chart:
  https://learn.microsoft.com/en-us/windows/apps/design/style/segoe-fluent-icons-font
