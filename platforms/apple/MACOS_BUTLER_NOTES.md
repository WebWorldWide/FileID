# macOS butler-restructure mirror — build + wire on a Mac

Mirrors the Windows butler redesign (`shared/docs/RESTRUCTURE.md`, Windows commits
`124ab1c` + `f135c41`) into the Swift engine. **Written in the Windows dev env, so
it is unverified — it needs `swift build` + `swift test` on a Mac.** The pure
algorithm is a line-by-line port of the Rust that passes its unit tests, but
nothing here has been compiled by Swift.

## What landed (engine — done, unverified)

- **`engine/Sources/FileIDEngine/Pipeline/RestructureSemantic.swift`** (new) —
  faithful Swift port of `restructure_semantic.rs`: signal fusion (CLIP 0.70 /
  tags 0.22 / time 0.08), density clustering via the existing
  `IdentityClustering` (no new deps), learn-your-style folder prototypes,
  3-band confidence (auto/review/ask) from folder-match strength + top-1−top-2
  margin + cluster cohesion, and c-TF-IDF distinctive-term group naming.
- **`Restructure.swift`** — `proposeAll` now loads CLIP embeddings + content
  tags, runs `RestructureSemantic.classify` for images, and falls back to the
  rule cascade for the rest. `RestructureProposal` gained `confidence` +
  `reason`; the rule cascade stamps both per branch (named person → auto, …,
  misc → ask), mirroring `restructure.rs`.
- **`IPCProtocol.swift`** — `RestructureMove` gained `confidence` + `reason`
  (kept `tier`), matching the extended `shared/ipc-schema/ipc.schema.json`.
- **`Tests/EngineTests/RestructureSemanticTests.swift`** (new) — parity tests
  mirroring the Rust ones (distinctive naming, auto-file on tight folder match,
  two-group separation). Run `swift test` to confirm the port.

## What still needs a Mac (app-side wiring + UI)

The macOS Restructure UI computes its plan **app-side** in `RestructureEngine.compute`
(`app/Sources/FileID/Views/RestructureView.swift`) via `FolderClassifier`, not from
the engine's `proposeAll`. So the new confidence/reason won't reach the UI until one
of these is wired (decide on the Mac):

1. **Surface the engine plan**: have the app call `Restructure.proposeAll` (now
   butler-aware) for the plan, or port `RestructureSemantic.classify` into
   `RestructureEngine.compute` so the app-side path produces confidence + reason.
2. **Reason display** — `RestructureView.swift` `proposalRow` (~line 701): render
   `proposal.reason` as a subtitle under the filename (mirror the Windows
   drill-down).
3. **Confidence in the outcome model** — macOS groups moves as Keep / Tidy /
   Reorganize (`RestructureHoverContext.swift`), not auto/review/ask. Map the band
   onto that model (e.g. surface an "auto-file N · review N · hold N" line in
   `RestructureApplyBar.swift`, and hold the `ask` band out of the default apply
   set) rather than bolting on a second taxonomy.
4. **Sankey palette** — `SankeyFlowView.swift` (~lines 603–636) uses gold + orange
   + secondary. For parity with Windows, switch destination-bucket fills to the
   Okabe-Ito CVD-safe palette (brand hues stay chrome-only):
   `#E69F00 #56B4E9 #009E73 #F0E442 #0072B2 #D55E00 #CC79A7`. The long-tail
   `+ N more` rollup already avoids silent truncation — keep it.

## Verify on the Mac

`cd platforms/apple && DEVELOPER_DIR=… swift build && swift test` (Engine + Shared
suites). Then `bash run.sh`, open Restructure, confirm: groups get specific names
(not "Photos"), confidence bands populate, reasons read naturally, and a library
still round-trips with the Windows engine (both write `confidence`/`reason` as
optional IPC fields, so an older peer ignores them).
