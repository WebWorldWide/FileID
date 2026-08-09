# Phase 1 — Acceptance checklist

Phase 1 of the Windows port is code-complete as of V11 (2026-05-02). This file is the test script for verifying it on real Windows hardware. Run through the steps; check off each one. Anything that fails is a Phase 1 bug, not Phase 2 work.

> The macOS app is the spec. When in doubt, compare the Windows build at the same step on a Mac. Anything visibly different from macOS is reported here, not handwaved.

## 0. Prerequisites

- [ ] Windows 10 22H2 OR Windows 11 (any build)
- [ ] Visual Studio 2022 17.11+ with Windows App SDK 1.6 workload — OR — `winget install Microsoft.WindowsAppRuntime.1.6` and the .NET 8 SDK
- [ ] Rust 1.78+ (`rustup default 1.78`) for the engine
- [ ] PowerShell 7+ (`pwsh`)

## 1. Build

- [ ] `pwsh platforms/windows/build/build.ps1` — Rust engine builds clean (LTO release, x64).
- [ ] `dotnet restore platforms/windows/FileID.sln` — NuGet restore succeeds.
- [ ] `dotnet build platforms/windows/FileID.sln -c Debug` — zero errors. Warnings should be zero too (TreatWarningsAsErrors); if any leak through, capture and report.
- [ ] `dotnet test platforms/windows/Tests/FileID.IpcSchema.Tests` — every IPC round-trip test green. Failures here mean the wire format diverges from Swift's Codable output and need to be fixed before any further work.

## 2. First launch

- [ ] `dotnet run --project platforms/windows/src/FileID.App` (or run from Visual Studio, F5).
- [ ] Window opens with dark Mica backdrop. Title bar caption buttons (min/max/close) are dark-themed.
- [ ] LavaLamp animates behind the layout: three blurred ellipses (gold + orange-red + dark) drifting via sin/cos position.
- [ ] First-launch Welcome sheet appears (because no models installed). Three rows: CLIP / ArcFace / Deep Analyze. Privacy banner ("No analytics. No telemetry. No remote logging.") at the bottom.
- [ ] Click **Skip for now**. Sheet dismisses cleanly.
- [ ] Sidebar shows: gold "Pick a folder" CTA + 6 disabled tabs ("Pick a folder above to enable tabs" implied) + "Engine starting…" pill at bottom.
- [ ] Within 2 seconds, the engine pill flips to "Engine ready". Hover for a tooltip showing version + PID + worker count + RAM. (If it stays Starting, check `%LOCALAPPDATA%\FileID\logs\app.log` — most likely cause: engine binary missing.)

## 3. LavaLamp side-by-side

The user's favorite. We made specific commitments to match macOS exactly.

- [ ] Open the macOS FileID build at the same window size. Run them side-by-side at 1080p.
- [ ] Three ellipse positions appear at roughly the same locations relative to window center. (They won't be perfectly synchronized — they each start from t=0 at app launch, not absolute time.)
- [ ] Drift speed is the same. Time multipliers (0.20, 0.23, 0.15, 0.18, 0.10, 0.12) match Swift's `LavaLampBackground.swift:14-22`.
- [ ] Blur radius reads as identical. The 120 px Gaussian on Win2D `CommandList → GaussianBlurEffect` should produce the same softness as SwiftUI's `ctx.addFilter(.blur(radius: 120))`.
- [ ] No flicker. No jitter. No banding.
- [ ] Reduce-motion: Settings → Accessibility → Visual effects → Animation effects OFF → LavaLamp halves rate (still visibly moving, just gentler). On macOS reduce-motion pauses entirely; we do half-rate to avoid a frozen-looking surface.

## 4. Folder picker + sidebar

- [ ] Press Ctrl+O. Folder picker opens.
- [ ] Pick a folder you own. Sidebar header switches to "<parent>/leaf", with leaf in gold.
- [ ] Three actions appear: Change folder…, Clear folder, Wipe library + rescan.
- [ ] All 6 tabs are now enabled.
- [ ] Click **Wipe library + rescan**. Confirmation dialog appears with explicit "this can't be undone". Cancel.
- [ ] Click **Clear folder**. Folder header reverts to picker CTA. Tabs disable.
- [ ] Drag a folder onto the window. Gold-bordered "Drop folder to scan" overlay appears. Drop. Folder gets picked.
- [ ] Try dragging a single file (not a folder). On drop, FileID surfaces "FileID needs a folder" alert.

## 5. Tabs + keyboard shortcuts

- [ ] Click each of the 6 tabs. Detail pane swaps to a placeholder describing what that tab will do (Phase 2+).
- [ ] Selected tab gets gold @ 18% background + gold @ 55% stroke. Other tabs are transparent.
- [ ] Alt+1..6 jumps tabs (Library / People / Cleanup / Deep Analyze / Restructure / Settings).
- [ ] Ctrl+Shift+S toggles the sidebar visible/hidden. State persists across launches.
- [ ] Ctrl+F doesn't crash (no search field in Phase 1; Ctrl+F raises an event Phase 2 will hook).

## 6. Processing control + engine pill

- [ ] With a folder picked, click **Start Scan** (or Ctrl+R). Engine logs the IPC. Phase 1 stub returns `not_implemented`, so the sidebar surfaces an error in the log; the IDLE state stays. **This is expected**: real scan plumbing lands in Phase 2.
- [ ] Engine pill: hover shows real values (version 0.1.0, current PID, 14ish workers depending on your CPU, the machine's RAM in GB).
- [ ] Kill the engine via Task Manager. Within ~1 s the C# app sees the EOF on stdout. Within 1–4–16 s (3-strike backoff window) the engine respawns and the pill returns to Ready. After the third strike inside 60 s, the pill goes Crashed and stays.

## 7. State persistence

- [ ] Pick a folder, switch to a non-default tab (e.g. People), hide the sidebar. Close the window.
- [ ] Relaunch. Folder is restored, tab is People (folder-picked allows it), sidebar stays hidden until Ctrl+Shift+S brings it back.
- [ ] Confirm `%LOCALAPPDATA%\FileID\app-settings.json` exists with the expected keys (lastFolderPath, sidebarVisible, activeTab).

## 8. Privacy verification

- [ ] Open Wireshark or Fiddler. Launch FileID. Skip the Welcome sheet. Idle for 30 seconds. Hit Ctrl+O, pick a folder, look at every tab.
- [ ] Wireshark capture should be empty. No DNS resolution, no TCP connect, no TLS handshake. **This is the privacy guarantee in action.**
- [ ] Click **Install all** in the Welcome sheet. Network traffic appears: HTTPS GETs to huggingface.co (CLIP, ArcFace) and the Qwen model repo. **No traffic to anywhere else** — no analytics, no Sentry, no telemetry endpoint.
- [ ] After all models installed, idle for 30 s with the app open. Network goes quiet again.

## 9. Accessibility

- [ ] Tab key reaches every interactive element in the sidebar + main content. Focus rings are visible.
- [ ] Screen reader (Narrator: Win+Ctrl+Enter) reads the tab labels with the right roles ("button, Library, switch to the Library tab").
- [ ] Run **Accessibility Insights for Windows** (free Microsoft tool) on the FileID window. Report should be zero Critical issues. Phase 1 is allowed to have Moderate issues we'll fix in Phase 8 polish.
- [ ] Tooltip on the engine status pill is reachable via long-hover or keyboard focus.

## 10. Acceptance sign-off

- [ ] Side-by-side LavaLamp video archived (drop in `shared/docs/assets/phase1-lavalamp-comparison.mp4` if recorded — gitignored, just for our own reference).
- [ ] Any deltas vs macOS captured as Phase 1 followups in NEXT.md (visual or behavioral). Anything labeled "ship as-is" gets a note in DECISIONS.md so future-you knows it was conscious.
- [ ] All 9 sections above tick clean.
- [ ] Phase 2 starts when this file is fully signed.

---

## Common gotchas seen during similar WinUI 3 ports

If you hit one of these, it's not a Phase 1 bug — it's a known-tricky pattern:

- **WinAppSDK runtime not found at launch** → install `Microsoft.WindowsAppRuntime.1.6` via winget. Or, when shipping, the WiX MSI installs the runtime alongside us.
- **Folder picker silently does nothing** → the InitializeWithWindow call needs the right HWND. We grab it via `WinRT.Interop.WindowNative.GetWindowHandle(MainWindow)`. If a third-party tweak changes how MainWindow is constructed, this can drop.
- **Mica looks flat / not translucent** → Win10 22H2 falls back to AcrylicBrush; Mica is Win11-only. Both work; just visually different. Verify by hitting the Win+Tab task switcher and confirming Mica blends with the desktop wallpaper on Win11.
- **LavaLamp draws behind a black box** → Win2D wasn't restored. Re-`dotnet restore`.
- **Title bar light when window inactive** → `DwmSetWindowAttribute(DWMWA_USE_IMMERSIVE_DARK_MODE)` should pin it dark. If it's still flashing light at frame zero, Win10 builds older than 1809 are the cause; we don't support those.
- **Engine pill stays "Starting…"** → `%LOCALAPPDATA%\FileID\logs\app.log` will say either "engine binary missing" (run `build.ps1`) or "could not parse command frame" (Rust-side IPC encoding bug). Both have clear log lines.
