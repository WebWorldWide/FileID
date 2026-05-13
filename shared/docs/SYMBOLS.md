# SF Symbols → Segoe Fluent mapping

Cross-platform icon parity. macOS uses SF Symbols (`Image(systemName:)`),
Windows uses Segoe Fluent Icons (`<FontIcon Glyph="&#xEXXX;"/>`). The two
glyph sets don't have 1:1 visual equivalents — some pairs read identical,
others diverge. This doc enumerates every site + the chosen mapping, with
notes when the Windows glyph reads visually different from the SF original.

## Process

1. Find every `<FontIcon Glyph="&#xEXXX;"/>` site in `platforms/windows/src/FileID.App/**/*.xaml`.
2. Cross-reference with the matching `Image(systemName: "name")` in `platforms/apple/app/Sources/FileID/Views/**/*.swift`.
3. Compare visually:
   - macOS: open the SF Symbols app → search by name → look at Default style.
   - Windows: open the Segoe Fluent Icons chart at https://learn.microsoft.com/en-us/windows/apps/design/style/segoe-fluent-icons-font (search by hex).
4. If the Windows glyph reads "obviously different" (different metaphor, different stroke weight, different fill style), pick a closer one from the Segoe chart and propose the swap below.

## Core mappings (high-confidence)

These mappings have been verified to read similarly on both platforms.

| Site / role | SF Symbol | Segoe Fluent | Hex | Notes |
|---|---|---|---|---|
| Folder picker | `folder` / `folder.fill` | Folder | E8B7 | Match |
| Search | `magnifyingglass` | Search | E721 | Match |
| Tag | `tag` / `tag.fill` | TagGroup | E8EC | Slightly different stroke; reads OK |
| OEM / processing | `gearshape` | OEM | E895 | Match |
| Checkmark | `checkmark` / `checkmark.circle.fill` | CheckMark / AcceptMedium | E73E / E8FB | Match |
| Download | `square.and.arrow.down` / `arrow.down.circle.fill` | Download | E896 | Match |
| Warning | `exclamationmark.triangle.fill` | Warning | EA39 | Match |
| Person/people | `person.crop.circle` / `person.2.fill` | People | E716 / E125 | Match |
| Sparkles / AI | `sparkles` | (no direct match) | E945 | Windows uses "Lightbulb" hex; reads as "tip / smart" — acceptable analog |
| Right-chevron | `chevron.right` | ChevronRight | E76C | Match |
| Left-chevron | `chevron.left` | ChevronLeft | E76B | Match |
| Hamburger / menu | `line.3.horizontal` | GlobalNavButton | E700 | Match |
| Reveal in file manager | `arrow.up.right.square` / `arrow.forward.square` | OpenInNewWindow | E8A7 / E838 | Acceptable |
| Open file | `arrow.up.right.circle` | OpenFile | E8E5 | Match |
| Copy path | `doc.on.clipboard` | Copy | E8C8 | Match |
| Trash | `trash` / `trash.fill` | Delete | E74D | Match |
| Faces detected | `face.smiling` / `person.crop.rectangle` | ContactInfo / Contact | E779 / E8D4 | Match |
| Text / OCR | `doc.text` / `text.viewfinder` | Document / TextDocument | E8A5 / E8E9 | Match |
| Cog / settings | `gear` / `gearshape.fill` | Setting | E713 | Match |
| Library / photos | `photo` / `photo.stack` | Photo | E91B | Match |
| Restructure / folder hierarchy | `folder.badge.gearshape` / `folder.badge` | FolderHorizontal | F12B | Match |
| Audio | `speaker.wave.2` / `music.note` | MusicNote | EA37 | Match |
| Video | `play.rectangle` / `video` | Video | E714 | Match |
| Cleanup / dedupe | `square.stack.3d.up` / `rectangle.stack` | StackArrowForward | E97A | Imperfect; reads OK |
| Info | `info.circle` | Info | E946 | Match |
| Pause | `pause.fill` | Pause | E769 | Match |
| Resume / play | `play.fill` | Play | E768 | Match |
| Cancel / xmark | `xmark.circle.fill` | Cancel | E711 | Match |
| Spinner (animated) | `arrow.triangle.2.circlepath` | Sync / Refresh | E895 / E72C | Sync (E895) reads better |

## Sites needing verification (pixel-level)

The following sites use glyphs that have been chosen pragmatically but
haven't been visually verified against the SF Symbol on real hardware.
User to walk both apps side-by-side and confirm or replace.

| File | Line | Current glyph | SF symbol name (macOS file) |
|---|---|---|---|
| `Views/Library/FilePreviewSheet.xaml` | 53 | E8B7 (Photo placeholder) | tbd — compare to `LibraryView.swift` placeholder |
| `Views/Cleanup/CleanupView.xaml` | (8 sites) | various | walk against `CleanupView.swift` |
| `Views/OnboardingSplash.xaml` | (8 sites) | various | match macOS `Detail.swift` empty-state icons |
| `Views/People/PeopleView.xaml` | (7 sites) | various | walk against `PeopleView.swift` |
| `Views/Library/LibraryView.xaml` | (11 sites) | various | tile badges (faces/text) + toolbar icons |

## Out-of-scope (Apple-only)

These macOS SF Symbols have no concept on Windows (no equivalent feature):

- `command` / `option` / `shift` modifier glyphs — Windows uses Ctrl/Alt/Shift text labels, not modifier glyphs.
- `apple.logo` — N/A.
- Liquid-glass `sf.glassmorph` symbols (macOS 26+ only).

## Adding new icons

When adding a new FontIcon site in WinUI:
1. Find the SF Symbol the macOS view uses for the same role.
2. Search Segoe Fluent for a glyph with similar metaphor + stroke weight.
3. Update the table above.
4. If you can't find a close match, use a text label instead (Windows convention permits text-only buttons for non-canonical icons).

## Source

- macOS SF Symbols: `platforms/apple/app/Sources/FileID/Views/**/*.swift` (search for `Image(systemName:`)
- Windows Segoe Fluent sites: `platforms/windows/src/FileID.App/**/*.xaml` (search for `Glyph=`)
- Segoe Fluent reference chart: https://learn.microsoft.com/en-us/windows/apps/design/style/segoe-fluent-icons-font
