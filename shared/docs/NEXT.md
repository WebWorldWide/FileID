# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

---

## 1. Verify Phase 0 Windows port commit on a Mac

The Phase 0 commit moves every macOS file into `platforms/apple/` and adds Windows scaffolding. None of the Swift code is modified, but `Package.swift`, `run.sh`, and `iterate.sh` paths need a real-Mac validation pass since I can't compile Swift on the user's Windows host.

**Acceptance:**
- `cd platforms/apple && bash run.sh` builds + bundles + opens FileID.app cleanly.
- `cd platforms/apple && swift test` is 28/28 GREEN.
- `cd platforms/apple && bash scripts/iterate.sh` is 11/11 GREEN.

If anything fails at this step, the most likely culprit is a hardcoded `cd "$PROJECT_DIR"` reference that I missed — `grep` for any remaining `app/`, `engine/`, `shared/` paths inside `platforms/apple/scripts/` to find them.

## 2. Apply the `startScan` IPC breaking change on the macOS side

The Rust engine implements the new payload `startScan(rootPath: String, rootDisplay: String?)` from day one. The macOS engine + app + iterate.sh still use the legacy `(rootBookmark: Data, rootPathDisplay: String)` payload. One coordinated commit:

- Edit `platforms/apple/shared/Sources/FileIDShared/IPCProtocol.swift` — change the case associated values.
- Edit `platforms/apple/engine/Sources/FileIDEngine/FileIDEngineMain.swift` — accept `rootPath` directly (drop the bookmark resolve branch). Mac is unsandboxed so this is a path-string `URL(fileURLWithPath:)` swap.
- Edit `platforms/apple/app/Sources/FileID/EngineClient.swift` — stop creating a security-scoped bookmark; send the path directly.
- Edit `platforms/apple/scripts/iterate.sh` line 128 — change the IPC frame to `{"startScan":{"rootPath":"$CORPUS"}}`.
- Verify `swift test` passes (round-trip test in `IPCProtocolTests.swift`).
- Run `bash scripts/iterate.sh` for end-to-end validation.

After this, both engines speak the same IPC.

## 3. Phase 1 — Library tab end-to-end on Windows

Per `~/.claude/plans/okay-so-this-is-dynamic-sparkle.md` Phase 1.

**Acceptance** (excerpt):
- WinUI 3 unpackaged app shell with Mica + Acrylic backdrop, dark mode forced, min size 1200×800.
- Theme port: GlassCard, GoldButton, BadgePill, SettingToggleRow, ThemedSegmentedControl in `FileID.Theme`.
- LavaLampBackground via Win2D, vsync-driven, paused when occluded — visually indistinguishable from macOS at 1080p.
- Library tab: folder pick → engine scan → progress + thumbnail grid.
- Engine pipeline: walkdir → kind detection → EXIF → phash → MobileCLIP scan-time embed → FTS5 OCR insert via `Windows.Media.Ocr`.
- Inline tag editing → `IPropertyStore` `System.Keywords` (round-trips via Explorer).
- Preview sheet: image (image-rs), video (Media Foundation thumbnail @ 25% duration), PDF (pdfium-render), audio (`symphonia` metadata).
- Cold scan ≥ 140 files/s on Ryzen 7 / RTX 3060-class with DirectML.

## 4. Lingering macOS work (deferred during the port)

Carried over from V9. Pick up after Phase 1 Windows ships, or interleave if scope allows.

- **Soak Restructure tab** on real ~50K library (Sankey, hover bus, drill-down, floating apply bar).
- **Engine perf sweep** — audit `ScanCoordinator`, `JobQueue`, `IPCSink` for strict-concurrency warnings; sustain ≥140 files/s on M1 Pro.
- **v1.0 ship checklist** (`shared/docs/SHIP.md`) — code signing + notarization, app icon, About panel, Sparkle channel.

## 5. Ideas parking lot

- Drag-and-drop a Restructure proposal row to override its destination.
- Per-cluster "merge into existing person" affordance in People.
- Smart Albums backed by saved CLIP queries.
- Export Restructure proposals as a JSON manifest for off-app review.
- Once Phase 4 lands, schedule a recurring agent to verify the privacy CI gate stays green on every release tag.
