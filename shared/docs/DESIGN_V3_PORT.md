# Prototype v3 — port spec (macOS → WinUI / GTK)

The macOS app is the reference. This file records its measured layout so the WinUI and GTK ports can match it 1:1 without reading Swift. Units are pt on macOS; 1 pt = 1 effective px on Windows and 1 px on GTK. `DESIGN_V3.md` holds the design contract; this file holds the numbers.

## Tokens

- Fonts: UI text Archivo; paths, filenames, numbers in fields and timecodes in the platform mono.
- Text: text1 `#F0F1F2`, text2 `#CFD3D8`, text3 `#A6ACB3`, label `#AEB4BB`. Under Increase Contrast / high contrast: text2 `#EBEDEF`, text3 `#D6D9DD`.
- Hairlines (white): 0.08 default, 0.06 soft, 0.14 strong. High contrast: 0.34 / 0.26 / 0.50.
- Panel: fill rgba(24,24,27,0.78), radius 10, 1 pt stroke at hairlineStrong × 0.8. Header padding 14 h / 10 v with a bottom hairline; header label Archivo 11 semibold uppercase, tracking 0.6. Footer padding 14 h / 8 v on white 0.02 with a top hairline.
- Pane fill white 0.015. Page inset 26 h, 20 top.
- Section label Archivo 11 semibold uppercase. Meta text Archivo 12 text3 (never below 11.5).
- Accents: gold `#FFCC00`, lavender `#B19BCE` (AI), cyan `#A0E2EA` (info/speech), pink `#F2A6C0` (done/kept), warning `#E0A340`, danger text `#FF6A5E`, danger fill `#E8433A`, paused rgba(185,190,196,0.5).

## Motion

- Tight spring 0.35 / 0.78 (response / damping): toggles, segments, selection, pills. Reduce Motion: instant.
- Standard spring 0.40 / 0.80: panes, panel insertion, expanding rows, aspect changes, sidebar. Reduce Motion: 0.15 s ease-out fade.
- Hover: 0.14 s ease-out.

## Focus

Every control takes keyboard focus. The ring is a 2 pt gold stroke at 0.9, 3 pt outside the control, corner radius = control radius + 3. The system ring is off so there is never a double ring. Text fields keep their hairlineStrong border and add the same ring.

Tab follows reading order inside focus regions. Shell: title bar → sidebar → page → chat inspector. Moments: results pane → detail. Takes: groups pane → actions → compare → filmstrip (← → Space Z, Return = primary) → goal. Tools: panels → preview. Restructure: step strip → content. Chat: conversation → sources. Space/Return activate; Esc cancels (chapter editor, chat inspector, sheet Done); Return is the default action (Save chapter, Export, drill-down Done); ⌘S saves a group; ⌘. stops a chat answer. Every icon-only control has a tooltip equal to its spoken label.

## Controls

Minimum hit target 28.

| Control | Spec |
|---|---|
| Primary button | 30 tall (24 small), gold fill, `#141414` Archivo 13 semibold, 14 h padding, radius 6. Hover `#FFD84D`, pressed `#CCA300` at 0.98 scale, disabled 0.4 opacity. |
| Secondary | 28, white 0.05 fill (0.09 hover, 0.14 pressed), 1 pt hairlineStrong stroke (white 0.24 hover, 0.60 high contrast), Archivo 13 medium text1, 12 h padding. |
| Ghost | text3 Archivo 12.5 medium, 6 h padding, min height 28, white 0.06 hover wash, radius 4. |
| Accent outline | 28, tint 0.10 fill (0.16 hover), 1 pt tint stroke at 0.6, Archivo 12 semibold, 10 h padding. |
| Danger | 30, `#E8433A` at 0.12 fill, `#FF6A5E` text, 1 pt stroke at 0.5; hover fill 0.85 with white text. |
| Icon button | 28 × 28, symbol 11.5 semibold text2, white 0.045 hover wash, radius 6. |
| Segmented bar | segment height 28, black 0.28 track, hairlineStrong stroke, radius 6; segments 12 h padding, Archivo 12.5 (semibold selected); selected pill gold, radius 4, inset 2, slides on the tight spring; Selected trait. |
| Toggle | 30 × 16 capsule (gold on / white 0.14 off), 12 pt knob, row min height 28, native toggle role. |
| Checkbox | 15 pt, radius 4, gold fill with a 9 pt heavy check, row min height 28, native toggle role. |
| Text field | 30 (28 in forms), black 0.3 fill, radius 6, Archivo 13.5 (mono 12.5 for numbers); 20 pt clear button when a search field has text. |
| Number field | mono 12.5, default 96 × 28, no digit grouping. |
| Rail row | min height 28, padding 10 h, outer inset 6 h, radius 8; selected gold 0.12 fill and gold 0.45 stroke, Selected trait. |
| Status pill | Archivo 10.5 semibold in its colour, 1 pt outline at 0.6, padding 6 h / 2 v, radius 4. |
| Job row | title Archivo 13 medium; state Archivo 12 semibold (running gold, paused text3, completed pink, failed/cancelled danger); percent mono 11.5; Pause secondary, Resume accent, Retry secondary, Cancel ghost (all 28); 4 pt grained bar in the state tone; padding 14 / 10. |
| Timecode | m:ss, h:mm:ss past an hour; spoken "N minutes N seconds". |

## Search & Moments

- 300 pt left pane (pane fill), vertical hairline, scroll area (padding 26 / 20, spacing 16).
- Left pane top (padding 14, spacing 10): search field "Search names, captions, chapters" with magnifier, Return searches; below it "N results" meta (80 pt indeterminate bar while searching) and "Best takes" secondary 28 (disabled under 2 unique files).
- Results: rail rows (spacing 2, list padding 8 v, row padding 7 v). Filename mono 12.5 medium, middle-truncated; meta Archivo 11.5 text3 ("m:ss · Chapter / Passage / Person / Sampled frame, unverified / Text in document / Text in image", lavender for sampled frames); snippet Archivo 12 text2, 2 lines. Spoken "name, meta", hint = text.
- No selection: dashed notice (radius 10, dash 5/4, white 0.16), serif 28 "Find a moment" and a subtitle.
- Player panel: header PLAYER with "m:ss / m:ss" meta 12 tabular. Filename mono 13 medium, folder mono 11 text3 head-truncated. Video 16:9, max height 340, black, radius 8, hairline stroke. Unplayable: "FileID can't play this format here…" row on black 0.3 radius 8 with Open secondary 28. Footer: Quick samples / Full moments segmented, Analyze primary 28, spacer, Play from start secondary 28 (with a player), Show in Finder secondary 28.
- Timeline: lanes Examined / Chapters / Speech, 6 apart; 70 pt label column (Archivo 11.5 medium text3), 10 gap; lane 16 tall, white 0.045 track, radius 4.
  - Examined: points as 1 pt lavender ticks (0.9); spans lavender 0.45 rects inset 3, radius 2.
  - Chapters: rects inset 2, radius 3, min width 3; gold 0.55 accepted, lavender 0.32 draft, paused grey stale; 1 pt stroke same colour at 0.85.
  - Speech: cyan 0.6, inset 4, radius 2, min width 2.
  - Playhead 1.5 pt gold in every lane, updated every 0.25 s; clicking a lane seeks to x / width × duration. Each lane is one accessibility element (value "N sampled frames" / "N chapters" / "N passages", or "Not analyzed yet" / "None").
  - Ruler 20 tall, ticks 1 × 5 hairlineStrong, labels mono 10.5 text3 (first leading, last trailing, rest centred). Step = first of 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200 s giving ≤ 8 ticks.
  - Data: Examined from `catalog_coverage` (sampled/verified, not speech); Speech from non-stale `catalog_passages` whose id starts with `speech:`.
- Chapters panel: header "CHAPTERS · N", Undo last edit ghost, + Add chapter secondary 28. Draft note (any non-user, non-stale chapter) lavender Archivo 12 on lavender 0.06, padding 14 / 8. Row padding leading 14, trailing 10, vertical 6, soft hairline: time/title button (radius 6, min 28; time mono 12 medium in a 56 pt column, gold when the playhead is inside; title Archivo 13 medium; summary Archivo 12 text3, 2 lines; spoken "Title, at X", hint "Plays from this chapter"); pill Draft lavender / Accepted pink / Stale warning; drafts add confidence "NN%" mono 11 text3 (34 pt) and Accept accent 28 (saves as a user chapter); Rename secondary 28; trash icon "Delete chapter". Current chapter: gold 0.07 wash and 3 pt gold left rail. Editor (padding 14 / 12, white 0.02): "NEW CHAPTER"/"EDIT CHAPTER", Title and Description fields 28, then Start + number field 96 × 28 + Playhead ghost, End the same, spacer, Cancel ghost (Esc), Add/Save chapter primary 28 (Return; disabled when the title is empty or end < start). Footer: "Chapters live in FileID's catalog. The video file is never changed."
- People panel: 7 pt dot (gold reviewed, lavender detected), name Archivo 13, time button secondary 28 mono 12 ("Play from X") or "Sampled image".
- Analysis queue panel: meta "Idle" or "N jobs", job rows.

## Chat

- Conversation column, vertical hairline, 300 pt Sources pane (pane fill).
- Header (26 h / 10 v): "LOOKING IN" label over chips Names / Tags / Captions / Chapters / Text in files (Archivo 11.5 medium text2 on white 0.07, radius 4, padding 7 h / 2 v; spoken as one sentence); trailing Clear ghost (disabled when empty).
- Messages (max width 760, spacing 20, padding 26 / 20): 12 pt left inset with a 2 pt rail in the role colour at 0.5; role label "You" gold or the model name lavender; body Archivo 13.5 text1, line spacing 2, selectable.
- Citation chips under the latest answer only, ≤ 8, flow layout spacing 6: "[n]" mono 11 semibold gold + name mono 11 text2 + "· meta" mono 11 text3; padding 8 h, min height 24, max width 280, radius 6; white 0.04 fill, hairlineStrong stroke; active chip gold 0.12 with gold 0.5 stroke; spoken "Source n: name, meta", hint "Opens the file".
- Busy: 60 pt indeterminate bar and lavender meta. Empty: serif 34 "Find the moment.", Archivo 13.5 text2, suggestion buttons 34 tall, radius 6, white 0.04, hairline, max width 460, trailing ↖ (fills and focuses the field).
- Composer (26 h / 12 v): multi-line field 1–5 lines, min 30, black 0.3, radius 6; Return sends; Send primary 30 or Stop secondary 30 (⌘.). Second row: toggle "Summarize with local AI", model chip (accent outline lavender 28, cpu icon, disabled when AI is off, spoken "Chat model: X"). Status line Archivo 12 text3.
- Sources pane: header "SOURCES" + count (padding 14 / 12), optional 16:9 player (radius 8, padding 12), rail rows ("[n]" mono 11.5 semibold gold in 30 pt, name mono 12 medium, meta 11.5, lavender for frames, snippet 12 text2 3 lines; context menu Show in Finder).
- Inspector variant 380 pt: padding 16, 3 chips, close icon (Esc); sources collapse into a 34 pt "SOURCES · N" row with a chevron rotating 90° (standard spring), list capped at 200 tall.
- Opening a source: media is checked for playability first; playable files play inline, everything else opens in the default app.

## Best Takes

Zero setup: the page asks the engine for `autoTakeGroups` on open and after every scan, and opens the first open group by itself. Manual groups with a goal (the older video flow) stay below.

- Groups pane 280 pt (pane fill, vertical hairline), one scroll:
  - Header row (14 leading, 8 trailing, 10 top): "POSSIBLE GROUPS · N" (open groups) in lavender, 40 pt indeterminate bar while loading, "Look again" icon button (↻, 28). Meta 11.5 text3, 14 h: "Bursts and repeated clips from your scans. Biggest and newest first." Empty after loading: meta 12 "None yet. Scan a folder with photo bursts or several tries of a clip."
  - Possible group rail rows (row padding 6 v, spacing 10): three 28 × 28 thumbnails of the first shots (radius 4, gap 2, hidden from VoiceOver); "N photos" / "N clips" Archivo 13 medium (text3 when skipped); date "Sat 19 Sep, 10:36 AM" Archivo 11.5 text3, plus " · Skipped". Spoken "N photos, date[, skipped]". Order: engine order (skipped last).
  - Soft hairline (8 v), "YOUR GROUPS · N" with the existing event rail rows; empty meta "Groups you make by hand, with a goal, show up here."
  - Soft hairline, "MAKE A GROUP BY HAND" (padding 14 h, spacing 10): search field "Search your videos" (Return searches and opens the manual editor), Find related secondary 28 + "+ New group" secondary 28, then Find related suggestion cards (lavender 0.08 fill, 0.3 stroke, radius 8).
  - After deleting a manual group: "Group deleted" footer with Undo accent 28.
- No group selected: dashed notice (serif 28) "No tries to compare yet" / "After a scan, Best Takes groups bursts and repeated clips here by itself. Scan a folder with photo bursts, or make a group by hand on the left." While the first load runs: 160 pt indeterminate bar + "Looking for bursts and repeated clips…" (13).
- Review (page inset 26 / 20, spacing 16), one possible group:
  - Header (spacing 12): serif 28 "8 photos" + date meta 13 text2 + "SKIPPED" status pill when skipped. Intro Archivo 13 text3: "Tries of the same shot, taken moments apart. FileID starred the one that looks best. Check it, then keep it and clear out the rest."
  - Action row (spacing 8): primary 30 "Keep the best, move the other N to Trash" (or "Keep K, move the other N to Trash" once the keep marks change; Return; disabled with a tooltip when nothing or everything is marked keep), "Keep all" secondary 28, "Skip group" ghost (disabled when already skipped), 60 pt bar while trashing.
  - Confirmation dialog: title "Move N photos to Trash?", message "Keeps NAME. The others (F files, including RAW copies) go to the Trash. You can undo right after, or put them back from the Trash.", destructive "Move to Trash", Cancel.
  - After the move: toast "Moved N photos to Trash" (+ " · K couldn't be moved") with Undo (10 s) → `restoreFromTrash(batchID)` → toast "Put back N files". Keep all: toast "Kept all N. This group won't be suggested again." with Undo (8 s, `decideTakeGroup` reopen). Skip: "Skipped. It's at the bottom of the list." with Undo. Each resolves to the next open group in the list.
  - Loading: 160 pt indeterminate bar + "Checking sharpness, faces and exposure in N photos…" (13). Failure: dashed notice "Couldn't score these photos" with the engine message.
  - Compare: two equal panes, gap 12. Left = BEST (gold label, star), right = THIS TRY (the focused try) or RUNNER-UP (rank 2) when the best is focused. Pane = panel radius 10, 2 pt gold stroke on the best, 1 pt hairlineStrong × 0.8 otherwise. Header 12 h / 9 v: "BEST · 3 of 8" section label, star, trailing "Keep" (pink check, Archivo 11.5 semibold) when marked. Image well black 0.35, 340 tall, aspect fit; Zoom crops to the largest face box × 2.6 (or the sharp-subject box × 1.2), centred, animated on the standard spring. Clips show a 36 pt gold play circle and play inline from one second before the most action. Body padding 12, spacing 8: filename mono 12 medium (middle-truncated) + "+ RAW" tag chip when the shot has a RAW twin + score 0–100 mono 12 semibold text2; 4 pt grained bar (gold on the best, neutral otherwise); reason chips (Archivo 11.5 medium, padding 7 / 2, radius 4, gap 6): positives gold on gold 0.12, goal reasons lavender on lavender 0.12, negatives text2 on white 0.07; none → meta 11.5 "Nothing stands out". Spoken: "Best, photo 3 of 8, NAME"; chips as one label.
  - Filmstrip: header "ALL N PHOTOS · K TO KEEP" + trailing ghost "Zoom to face" / "Show whole photo" (Selected trait; Z). Horizontal scroll, gap 8, padding 4. Tile 112 × 76, radius 8, thumbnail fill; index badge mono 10.5 semibold (gold fill with onGold text on the best, black 0.6 otherwise, radius 3) + gold star on the best; pink check-circle 15 pt top-right when kept; unkept tiles at 0.62 opacity; stroke 2 pt text1 when focused, 2 pt gold on the best, 1 pt hairlineStrong otherwise; focused tile scales 1.04 (tight spring). Click focuses, double-click toggles keep. The strip is one focus stop with the 2 pt gold ring (radius 11): ← → move focus and scroll it to centre, Space toggles keep (announces "Keeping NAME"), Z toggles zoom. Tile spoken "Photo 3 of 8, NAME, recommended, keep, score 91", hint = reasons, Selected trait when focused, actions Keep / Don't keep. Hint line meta 11.5: "← → move · Space keep · Z zoom · Return keeps the marked photos and moves the rest to Trash".
  - Goal panel (photos only): header "GOAL (OPTIONAL)" with a 60 pt bar while ranking; body 14 / 12: text field 28 "What should happen? e.g. Kyle gets a hit, everyone smiling" (Return ranks) + accent lavender 28 "✦ Rank with AI" (disabled when empty or running). Result row: 3 pt rail (lavender when ranked, text3 otherwise) + summary Archivo 12.5 text2. Without a result: meta 12 "Without a goal, FileID picks on sharpness, faces, eyes and exposure. Add a goal to find the try where it happens." A ranked result moves BEST to the model's pick, keeps it marked if the keep marks were untouched, and adds "Shows the goal" / "Closest to the goal" chips.
- Engine contract (both engines): `autoTakeGroups` (limit ≤ 100) → `takeGroups`; `scoreTakes` (every file of the group, ≤ 800) → `takeQualities` in capture order with `rank`, `recommended`, ≤ 3 `reasons`, normalized `faces` / `subject` boxes; `decideTakeGroup` keepAll / skip / reopen; `rankTakesWithGoal` (shot primaries best-first, the first six go on one contact sheet) → `takeGoalRanking` (status ranked / unclear / unavailable); `trashFiles` → `bulkActionResult` action "trashFiles:<batch>" → `restoreFromTrash(batchID)`.
- Manual groups (selected from YOUR GROUPS or New group): unchanged group panel, goal job, recommendation row and take cards below:
  - Group panel: NEW GROUP / GROUP with Undo edit ghost and Delete ghost in danger; 44 pt label column Archivo 12.5 medium text2; Name and Goal fields 28; "CLIPS · x of y chosen" with Choose all/none ghost; checkbox rows mono 12; footer Save group primary 28 (⌘S), Score takes secondary 28, Refresh scores ghost. Recommendation row: 3 pt left rail in the tone (gold ranked/preferred, lavender tie, text3 insufficient). Card grid adaptive, min 260, max 420, gap 12.
  - Take card: panel radius 10, 2 pt gold stroke when recommended. Header 12 h / 9 v: "TAKE n", star, 7 pt dot (gold reviewed, lavender draft, text3 unscored). 16:9 thumbnail with a 36 pt gold play circle. Body padding 12, spacing 8: filename mono 12 medium; outcome Archivo 13 semibold; why Archivo 12 text3 3 lines; Sharpness row (64 pt label, 4 pt bar, value mono 11). Footer 10 h / 8 v: Met / Missed / Unclear, spacer, Prefer accent with star, Undo review icon.

## File Tools

- Left scroll (26 / 20) with panels 01 and 02, vertical hairline, 380 pt preview pane.
- 01 · Recipe: header segmented Photo / Video / Chapters; search field + Find secondary 30; checklist rows (checkbox, filename mono 12, parent mono 11 text3, soft hairlines); footer "x of y selected", Select all/none ghost.
- 02 · Output: 92 pt label column (Archivo 12.5 medium text2), gap 12. Format segmented (PNG/JPEG/TIFF, MP4 · H.264, WebVTT/JSON). Photo: Longest side number field 90 × 28, "px, up to 8192". Video: Resolution 720p/1080p/4K; Frame aspect tiles 96 wide with 30 pt frames, radius 8, selected gold border and gold 0.1 fill; Framing segmented only when the frame isn't Original. Notes Archivo 12 text3 indented 104. Toggles Stabilize shaky video, Enlarge smaller media to this size. Footer "SAVE TO" + path mono 12 head-truncated (or "No folder chosen"), Choose… secondary 28 ("Choose output folder").
- 03 · Preview: header (14 / 12) with 60 pt indeterminate bar while busy; frame black 0.35 at the chosen aspect (photo = image aspect; video 16:9 / 9:16 / 1:1 / 4:5), max height 300, radius 8, 1.5 pt gold stroke, aspect animates on the standard spring; summary Archivo 13 medium ("N files · PNG · ≤ 4096 px") and capability detail Archivo 12 text3; "OUTPUTS · N" rows (name mono 12, state Archivo 11.5 semibold: Planned text3, Running gold, Completed pink, Failed danger, Undone paused; message 11.5). Footer (14): Preview secondary 30, Export primary 30 (Return), spacer, Undo accent 30 or Cancel ghost while exporting; then Show last export ghost and a note.

## Restructure

- Step strip pinned above the scroll (26 h, 16 top, 12 bottom, full-width hairline below), panel-styled radius 10, three equal cells split by vertical hairlines; cell padding 14 h / 12 v, spacing 8.
  - Badge 20 pt: done pink fill with 9 pt heavy `#141414` check; current 1.5 pt gold ring with mono 11 semibold gold number; upcoming hairlineStrong ring, text3 number. Title Archivo 13.5 semibold (text3 upcoming). Detail Archivo 12 text3, 2 lines. Actions row min 28. Current cell gold 0.07 wash and 3 pt gold left rail; standard spring. Spoken "Step n of 3, Title, done/current/not started", hint = detail.
  - Preview: current while planning or without a plan, done once a plan exists or files moved. Plan primary 28 / Plan again ghost, 160 pt indeterminate bar while planning; detail "N moves planned · S sources → D destinations".
  - Review: upcoming without a plan, current when nothing is selected, done when something is selected or files moved. Review all moves ghost (opens drill-down); detail "x of y moves selected" (large plans: "only the N confident moves apply").
  - Move: current while moving or with a selected plan, done after a move. "Move N files" primary 28 → confirmation "Move N files?" (destructive Move files, Cancel); while moving a 120 pt bar and Cancel danger 28 → "Stopping…"; after: Undo move accent 28 with ↺.
- Content (26 h, 20 top, spacing 16): intro Archivo 13.5 text2, folder mono 11 text3 head-truncated.
- Metric row panel, three cells: 7 pt tint dot + label, value mono 20 semibold (tint on hover), hint Archivo 11.5; keep pink, tidy warning, reorganize gold; hover tint 0.07.
- Notices: 3 pt tone rail on tone 0.05 over the panel; title Archivo 13.5 semibold, body 12.5 text2; actions accent 28 in the tone (Deep Analyze notice adds Not now ghost).
- Map panel: "FOLDER MAP" or "NOW → AFTER", "S sources → D destinations" meta, Map/Tree segmented; body padding 14 with a hint line; Sankey nodes are buttons (focus radius 8, "From X"/"To X"); tree filter Show: All / Moves / New folders.
- Recommendations: header meta "Skip a group to leave it out of the move"; rows padding 14 / 12: 30 pt icon tile (tint 0.16, radius 6, 13 pt glyph), headline Archivo 13.5 semibold, body 12.5 text2, Review files / Hide files secondary 28 with chevron (Expanded/Collapsed), Skip these ghost or Include accent; trailing count mono 20 semibold in the tint and "N folders" 11.5; skipped rows 55% opacity; hover tint 0.06 + 3 pt rail; expanded list on white 0.02 with a soft hairline.
- Bucket section: icon 13 gold, name mono 12.5 medium, "N files moving in" 11.5 text3, Select all / Skip all ghost; rows indented 26 with a 1 pt hairlineStrong guide at x = 10.
- Proposal row (button, min 28, radius 6): checkbox, 11 pt icon, filename mono 12 (text3 when skipped), "from X" mono 11 text3, reason Archivo 11.5 text2, confidence badge; Space/click toggles; spoken "Move F from S to B" with Selected trait. Inline lists show 50 rows then "+ N more"; "See all N files" ghost past 30.
- Staying put: 38 pt collapsible header (chevron 90°, section label + meta); rows 7 pt pink dot, folder mono 12, count 11.5; hover highlights the source in the map.
- Drill-down sheet: header (26 h, 20 top, 14 bottom) crumb "RESTRUCTURE / REVIEW", serif 28 title, meta, Done primary 30 (Return); hairline; list (26 / 16, spacing 12), unlimited rows.
