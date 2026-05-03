# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

---

## 1. User-side Phase 1 verification on real Windows hardware

Phase 1 (V11) ships a feature-rich UI shell but I haven't run any of it. The primary blocker is verifying the build is clean on a real Windows host.

**Acceptance:**
- Open `platforms/windows/FileID.sln` in Visual Studio 2022 17.11+. NuGet restore succeeds.
- `dotnet build platforms/windows/FileID.sln -c Debug` succeeds with zero errors. (Warnings expected; we treat warnings as errors in CI but local-build warnings can leak through XAML codegen.)
- `dotnet run --project platforms/windows/src/FileID.App` launches the app:
  - Mica/Acrylic chrome with a dark title bar.
  - LavaLamp animating behind a sidebar + detail layout.
  - First-launch Welcome sheet appears (since no models installed yet); Skip dismisses cleanly.
  - Sidebar shows folder picker placeholder + the 6 disabled tabs + an "Engine starting…" pill at the bottom.
  - The engine pill flips to "Engine ready" within 1–2 seconds (because the Rust engine emits `ready` on stdin/stdout). If it stays "Starting…" or goes "Crashed", check `%LOCALAPPDATA%\FileID\logs\app.log`.
- `dotnet test platforms/windows/Tests/FileID.IpcSchema.Tests` is GREEN.
- Side-by-side LavaLamp video review at 1080p against macOS reference: the three-ellipse drift + 120 px Gaussian + 35 % darken overlay should look indistinguishable. Frame-by-frame ideally, but a 30 s recording is sufficient for sign-off.
- Hit Ctrl+O. Folder picker opens. Pick a folder. Sidebar header switches to "<parent>/leaf" with leaf in gold, Change/Clear/Wipe actions appear, tabs become enabled.
- Hit Ctrl+R after picking a folder. Sidebar processing control flips to in-flight state with progress bar (which will sit at 0 because the engine returns `not_implemented` for startScan in Phase 0 — that's expected; Phase 2 wires the real scan).
- Ctrl+Shift+S toggles the sidebar.
- Alt+1..6 jumps tabs.
- Drag a folder onto the window. Gold-bordered overlay appears. Drop accepts the folder.
- Reduce-motion verification: Settings → Accessibility → Visual effects → Animation effects OFF. LavaLamp halves rate; Shimmer freezes; CompletionRipple becomes inert; IridescentBorder freezes gold.
- Accessibility Insights audit ≥ 0 critical issues. Tab key reaches every interactive element.

**Likely first-run hiccups (in priority order):**
1. WinAppSDK 1.6 runtime not installed: surface error MessageBox at launch. Install via `winget install Microsoft.WindowsAppRuntime.1.6` and relaunch.
2. NuGet restore fails: probably the `nuget.config` carve-out — re-run `dotnet restore platforms/windows/FileID.sln`.
3. XAML compilation errors I missed: most likely candidates are the IridescentBorder template (Win2D namespace), the DetailHostView swap pattern, and the templated control attached property registrations. If a build error references one of those, paste it and I'll fix.
4. Engine doesn't spawn: the C# app expects `FileIDEngine.exe` either alongside `FileID.exe` or under `engine/target/{x86_64,aarch64}-pc-windows-msvc/release/`. Run `pwsh platforms/windows/build/build.ps1` first to produce the engine binary.

## 2. Apply the `startScan` IPC breaking change on the macOS side

Carried over from V10. The Rust engine implements the new payload from day one. The macOS engine + app + iterate.sh still use the legacy `(rootBookmark: Data, rootPathDisplay: String)` payload. One coordinated commit on a Mac:

- Edit `platforms/apple/shared/Sources/FileIDShared/IPCProtocol.swift` — change the case associated values to `(rootPath: String, rootDisplay: String?)`.
- Edit `platforms/apple/engine/Sources/FileIDEngine/FileIDEngineMain.swift` — accept `rootPath` directly (drop the bookmark resolve branch).
- Edit `platforms/apple/app/Sources/FileID/EngineClient.swift` — stop creating a security-scoped bookmark; send the path directly.
- Edit `platforms/apple/scripts/iterate.sh` line 128 — change the IPC frame to `{"startScan":{"rootPath":"$CORPUS"}}`.
- Verify `swift test` passes. Run `bash scripts/iterate.sh`.

After this, both engines speak the same IPC. Nothing else cross-platform-breaking is queued.

## 3. Phase 2 — Library tab end-to-end on Windows

Per `platforms/windows/PHASES.md` Phase 2. Big chunk: scan pipeline (walkdir, EXIF, phash, MobileCLIP scan-time embed, OCR via Windows.Media.Ocr, SCRFD+ArcFace face detect+embed, DBWriter), Library tab UI (search, multi-select, file preview sheet, tag editor, bulk actions). 4–5 weeks of work.

Don't start until item 1 above passes.

## 4. Lingering macOS work (deferred during the port)

Carried over from V9/V10. Pick up after Phase 1 Windows ships, or interleave if scope allows.

- **Soak Restructure tab** on real ~50K library (Sankey, hover bus, drill-down, floating apply bar).
- **Engine perf sweep** — audit `ScanCoordinator`, `JobQueue`, `IPCSink` for strict-concurrency warnings; sustain ≥140 files/s on M1 Pro.
- **v1.0 ship checklist** (`shared/docs/SHIP.md`) — code signing + notarization, app icon, About panel, Sparkle channel.

## 5. Ideas parking lot

- Drag-and-drop a Restructure proposal row to override its destination.
- Per-cluster "merge into existing person" affordance in People.
- Smart Albums backed by saved CLIP queries.
- Export Restructure proposals as a JSON manifest for off-app review.
- Once Phase 4 lands, schedule a recurring agent to verify the privacy CI gate stays green on every release tag.
