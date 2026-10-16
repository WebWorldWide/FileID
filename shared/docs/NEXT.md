# NEXT — resume here

Short, current priorities only. History lives in `STATE.md` (newest first) and git; release gates live in `SHIP.md`; the feature ledger lives in `NEXT_VERSION.md`; model choices live in `MODELS.md`.

## Where things stand (2026-10-15)

- `main` is the only branch. Unmerged checkpoint work is recoverable from `archive/2026-10-13/*` tags; the pre-merge Mac completion checkpoint (including its dropped portable face-refresh Rust slice) is commit `03c50bcd`.
- The Mac completion pass (best takes, video tools, moments/cast, reviewed naming, People refresh, MLX model browser, right-side chat) is folded into `main` together with the restored shared engine (#248) and the GTK theme fix (#249); IPC is 1.16.0.
- The shared Rust engine (`platforms/windows/src/engine`) serves Windows, Linux, the CLI and the TUI. macOS has its own Swift engine and remains the behavioral reference.
- Test hosts: the Linux VM (`adlon-ci-linux`), the Windows 11 VM (`ADLON-CI-WIN`, also a self-hosted CI runner) and the owner's Mac (headless `swift build`/`swift test` over SSH only; the owner does interactive macOS testing and package signing locally).

## In flight

0. **Finish the August-rollback restore** (engine landed first): restore the GTK app (paged restructure plans, engine client, settings) and the WinUI app (47 conflicted files) area by area, each as its own PR verified on its host. The macOS engine now implements `reassignFace`; the remaining portable-only commands (`findMergeSuggestions`, `embedTextQuery`, `embedImageQuery`, `restoreFromTrash`, `revertMerge`, `generateVideoThumbnail`) are served in-app on macOS and stay `not_implemented_yet` in its engine.
1. **Redesign** (FileID Prototype v3, contract in `DESIGN_V3.md`): macOS shell, design system (now rounded, Archivo text), simplified sidebar, fonts, lava, Library/People/Cleanup/Settings/Welcome landed. Finish the macOS page layouts (Moments tracks, Takes/Tools/Chat panels, Restructure step cells), then port the shell to WinUI and GTK (tokens are in; the title bar, area sidebar, pipeline block and page headers are not). Keep the grain fine (2× multi-octave) and polish each page fully.
2. **Benchmarks on real hardware**: run the model catalog benchmark on the RTX 2060 PC and the Mac; exercise the WinUI Models workbench interactively; macOS MLX benchmarking is implemented (model browser → Speed on this Mac) but has not measured a real model yet because none is installed; with owner approval install Qwen3-VL 4B and run it. Then measure the shortlisted gap models in MODELS.md.
3. **Portable People/naming parity**: re-derive the dropped portable face-refresh/crop-publication/naming-context slice (commit `03c50bcd`) on top of the restored shared engine, per the MAC_FINISH.md handoff matrix.

## Live to-do (owner requests, 2026-10-15; tick as work lands)

Performance loop (goal from the owner: large multiples on speed, memory, CPU and size; every change measured on the Adlon library or a model run):
- [x] RAW scan decode uses the embedded full-size preview: 606 → 35–55 ms per Sony ARW on Adlon (11–17×).
- [x] Model download no longer refused when memory is briefly busy (memory is checked before loading, not before fetching).
- [x] SmolVLM 256M/500M removed from the Mac catalog: the pinned MLX runtime aborts the engine on them (384 vs 512 px tiles).
- [x] Forced 448 px image size skipped for fixed-tile architectures (Idefics3, SmolVLM).
- [x] Measure end-to-end scan files/s on Adlon: 4.1 → 40–42 files/s on 105 RAWs (decode preview + GPU CLIP), memory 1,090 → ~660 MB.
- [x] CLIP on the GPU via MLX, weights read from the installed ONNX, batched (190 ms → 3.2 ms per image).
- [x] Per-image overlap: CLIP (GPU) and the file hash (disk) run while Vision uses the Neural Engine; Vision is warmed once before workers start. Per-file 129 → ~115 ms.
- [x] RAW previews up to a third larger than the target are used without resampling (decode 33 → 21 ms per Sony ARW); engine CPU for 382 RAWs 34.0 → 30.6 s.
- [ ] Reach ≥140 files/s. Now 55–56 files/s overall and ~63 steady on 382 warm RAWs (was 28 uncached / ~41 warm this morning, 4 before the session) with other apps loading the Mac. Remaining: the scan's CPU admission allows 8 files in flight (cores − 2), JPEG decode/resample for model inputs, and Vision's process-wide throughput (~160 images/s measured in isolation).
- [ ] Hash only once per image (content hash currently reads the file a second time after decode).
- [x] Compress AI memory: MLX buffer cache capped (Qwen3-VL 4B peak 4.33 → 3.92 GB, no speed loss); benchmark now reports real process footprint. KV quantization measured as negligible at current prompt sizes (see DECISIONS.md).
- [ ] Compress AI memory further: free vision-encoder activations between passes; route tag/rename passes to a small model (Qwen3.5 0.8B peaks at 1.1 GB) when quality holds.
- [x] Idle models are parked after 5 min (weights freed, model stays selected, reloads on next use): 3.1 GB → ~107 MB while idle, verified on Adlon files with Qwen3-VL 4B.
- [ ] Chat: stream first token faster (prompt cache reuse across turns, smaller retrieval context).
- [x] Video moments: next window's frames extract while the model runs; revision check in-process (one helper spawn and one frame decode fewer per window).
- [ ] Video analysis: keep one long-lived frame helper per job instead of a process per window; batch frames per model call; measure on Adlon clips.
- [ ] Scan: overlap I/O with ML (read-ahead on USB/NAS); move RAM++ to the GPU the same way as CLIP once installed and measured.
- [ ] Size: audit bundle size (MLX metallib, ONNX Runtime, fonts) and strip unused architectures/resources.
- [ ] Audit third-party packages for hot-path overhead (GRDB writes, swift-transformers tokenizer, ORT session setup) and replace or rewrite where measurably faster.
- [x] Run `benchmarkModel` on Qwen3-VL 4B and Qwen3.5 0.8B (results in STATE.md and MODELS.md).
- [x] Qwen3-VL adapter forwards per-token generation (every Qwen3-VL generation aborted the engine before).
- [x] Free-memory admission uses the kernel availability level (refused a 0.8B model with 50% free).

- [ ] Test isolation: `queuedCancelHonoredWithTerminalComplete` shares the `DeepAnalyze.shared` cancel flag and failed once in three local parallel runs (CI runs `--no-parallel`); give the runner an injectable analyzer.

- [x] In-app playback (macOS): videos and audio play inside the preview sheet with AVKit controls; Chat results, Moments and Best Takes play in their own players instead of opening QuickTime.
- [x] In-app playback on Linux: the preview dialog plays video/audio with GtkVideo; Search & Moments hits play in a FileID dialog from the matched moment instead of launching another app.
- [x] In-app playback on Windows: the preview sheet already used MediaPlayerElement; Chat/Moments evidence now plays every video and audio hit in place (from the matched moment when there is one) instead of opening the default app. CI-verified only.

UI loop (Prototype v3 simplicity plus the old feature set, pixel-checked against Apple's Human Interface Guidelines):
- [x] Rounded geometry, Archivo text, simplified sidebar, compact scan card.
- [x] Library Selected pane (single click), full preview on double-click, accessibility actions on tiles.
- [x] Finder-style file sizes everywhere.
- [x] HIG: icon-only buttons have spoken names (VoiceOver read "Gear Shape", "Split View Horizontally Left", "Conversation"); text and hairlines strengthen under Increase Contrast; the animated background becomes solid under Reduce Transparency; Reduce Motion already honored.
- [x] HIG (Library): the grid takes keyboard focus on click; arrow keys move the selection by tile and by row, Return/Space open the preview, Escape clears; selection scrolls into view and pages in more files. Built and unit-tested; not yet exercised by keyboard on screen.
- [ ] HIG: keyboard focus order and visible focus rings on the other pages with Full Keyboard Access; text-size setting for the content area.
- [ ] Moments tracks, Takes/Tools/Chat panels, Restructure step cells in v3 layout.
- [x] Model browser "Speed on this Mac" checked on screen with a real in-app run (Qwen3-VL 4B: 4.3 s load, 165/26 tok/s, 5.1 s per image, 3.8 GB); metrics use the full width, the selected row stays in view, measured memory replaces the estimate.
- [x] Title bar: window buttons vertically centred in the 48 pt bar (re-applied on resize/full screen), search centred, folder left, activity/chat/settings right; Browse/Analyze/Act moved to the top of the sidebar and filters its pages.
- [x] Preview sheet: sharp ImageIO decode (Quick Look queued it behind grid thumbnails and showed 640 px), neutral backdrop, toolbar with arrows/Finder/Open/info toggle (⌘I)/close, info panel with details, people, description + smart name, tags, Finder tags, technical.
- [x] Scrolling (Library): main-thread load while scrolling 75% → 28% — thumbnails decode to bitmaps off the main thread (NSImage(data:) decoded on first draw), no per-tile shimmer timeline, gradient layer or appear transitions; native view churn 53% → ~1% of main-thread work.
- [ ] Scrolling: remaining cost is SwiftUI layout and per-tile hover tracking (~11%); consider an NSCollectionView-backed grid for 10k+ files, and apply the same fixes to People, Cleanup and Moments lists.
- [x] Library shows every file: the grid paged at a hard 200; it now loads 300 at a time as you scroll (stable `scanned_at, id` order), verified to reach 3,447 of 3,447.
- [x] App reads use a pool of 6 read-only connections (was one serial queue shared by grid, search, tags, people and counters), each with a 32 MB cache and memory-mapped I/O.
- [x] Scroll memory: off-screen tiles drop their bitmap, thumbnails decode at 448 px (was 528), cache 256 MB: 1,542 → 843 MB after flinging through 3,447 files.
- [x] Library grid on NSCollectionView with cell reuse, Core Animation text layers and one grid-wide hover tracker: flinging through 3,447 files (warm) main thread 86% → 53%, app memory 1,622 → 318 MB.
- [x] People and Cleanup: face crops run off the main thread; file tiles release their bitmap when scrolled away (both grids were already lazy and inherit off-main thumbnail decoding). Not measured on screen: this library has no grouped faces or duplicates.
- [x] ReadStore uses the six-connection read pool only for WAL databases (the engine's always is); other journal modes get one connection. Fixes an intermittent "database is locked" in the app tests (1 in ~4 runs → 0 in 10).
- [x] Shared engine RAW decode: `pipeline/raw_preview.rs` extracts the largest viewable embedded JPEG from TIFF-based RAW (ARW/NEF/CR2/ORF/PEF/DNG/…) and RAF, skipping lossless-JPEG sensor data, and applies the RAW orientation. Real Adlon ARW: full 7008×4672 preview in 72–92 ms (Linux could not decode these at all; Windows demosaiced via WIC).
- [x] Linux Library: pages in 600 at a time as the grid nears its end (was a hard 1,000 cap), reloads in one splice instead of one signal per row, thumbnails decode on a 2–6 thread pool and skip tiles already rebound (was one OS thread per tile).
- [x] Windows Library: pages in 300 at a time from the ScrollViewer position (was a hard 200), appends in one collection Reset, and live-scan refreshes keep every loaded page; ReadStore paging test added. Compiled and tested by CI only (no .NET SDK on the Mac); interactive check on the Windows host still needed.
- [ ] Database at 100k files: measure keyword search (LIKE scans on path/description/tags), counters and FTS on a synthetic 100k-row catalog; add trigram FTS for paths if LIKE dominates.
- [x] Restructure (macOS): respect-your-organization stage (see RESTRUCTURE.md); Adlon/Camera 2,966 shuffling moves → 0, 28 folders kept; planning >10 min → ~19 s; "Staying put" shows real file count; models banner offers Rescan when CLIP is installed but files were scanned before it.
- [x] Restructure (shared Rust engine): `pipeline/restructure_respect.rs` ports the same rules and Keep accounting for Windows, Linux, CLI and TUI; 6 Rust tests mirror the Swift ones; strict Clippy clean.
- [ ] Restructure: test on a genuinely messy folder (Downloads-style dump) with CLIP embeddings present, and review the loose-file folder names it proposes.

## Next, in dependency order (see `NEXT_VERSION.md` M0–M6)

1. **Linux inference runtime packaging**: Linux packages do not bundle llama-server yet. Review the archived `parity/completion-runtime` llama.cpp source builder (license, provenance, offline policy) to ship a pinned CPU/Vulkan build; whisper.cpp is no longer needed now that speech runs on llama.cpp.
2. **Evaluated timeline candidates** (M2): labeled rights-clear fixtures; measure interval recall, timestamp error and missed fast actions; reviewable native result with exact seeking.
3. **People** (M1): durable refresh backlog, incremental assignment against trusted exemplars, merge/split/ignore review queues, held-out precision/recall.
4. **Smart names** (M1): saved styles, length budget, confirmed-person/event tokens, companion-aware (RAW+JPEG, Live Photo, sidecar) apply/Undo, blinded preference comparison.
5. **Best takes** (M4): automatic related-attempt grouping from durable library-wide jobs, goal-conditioned outcome evidence from timeline intervals, native GTK/WinUI review.
6. **Toolbox** (M5): trim, remux, proxies, audio extraction, repair diagnosis; RAW/HDR only with color management.
7. **Stabilization, enhancement, tracked reframing** (M6).

## Release gates (unchanged; details in `SHIP.md`)

Signed native packages (owner-local for macOS), physical GPU/NPU and ARM64 model runs, ≥140 files/s and 100,000-file throughput, offline network capture, codec/legal review of the FFmpeg/OpenH264 runtime, crash/restart output reconciliation. Do not claim production readiness from VM or hosted-CI results.

## Working rules

Land work on a branch, merge to `main` after exact-head CI is green, confirm exact-main CI, delete the branch. Keep Adlon example data read-only. No new dependencies or model downloads without the owner's approval; record non-obvious calls in `DECISIONS.md`.
