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
- [ ] Reach ≥140 files/s: remaining per-file cost is decode ~40 ms, Vision ~40 ms, OCR ~35 ms (only text-like images); try Vision on a smaller working image for classify, overlap OCR, and batch the DB writer.
- [ ] Hash only once per image (content hash currently reads the file a second time after decode).
- [x] Compress AI memory: MLX buffer cache capped (Qwen3-VL 4B peak 4.33 → 3.92 GB, no speed loss); benchmark now reports real process footprint. KV quantization measured as negligible at current prompt sizes (see DECISIONS.md).
- [ ] Compress AI memory further: free vision-encoder activations between passes; route tag/rename passes to a small model (Qwen3.5 0.8B peaks at 1.1 GB) when quality holds.
- [ ] Unload idle models after a timeout; share one resident model between Deep Analyze, Chat and Moments.
- [ ] Chat: stream first token faster (prompt cache reuse across turns, smaller retrieval context).
- [x] Video moments: next window's frames extract while the model runs; revision check in-process (one helper spawn and one frame decode fewer per window).
- [ ] Video analysis: keep one long-lived frame helper per job instead of a process per window; batch frames per model call; measure on Adlon clips.
- [ ] Scan: overlap I/O with ML (read-ahead on USB/NAS); move RAM++ to the GPU the same way as CLIP once installed and measured.
- [ ] Size: audit bundle size (MLX metallib, ONNX Runtime, fonts) and strip unused architectures/resources.
- [ ] Audit third-party packages for hot-path overhead (GRDB writes, swift-transformers tokenizer, ORT session setup) and replace or rewrite where measurably faster.
- [x] Run `benchmarkModel` on Qwen3-VL 4B and Qwen3.5 0.8B (results in STATE.md and MODELS.md).
- [x] Qwen3-VL adapter forwards per-token generation (every Qwen3-VL generation aborted the engine before).
- [x] Free-memory admission uses the kernel availability level (refused a 0.8B model with 50% free).

UI loop (Prototype v3 simplicity plus the old feature set, pixel-checked against Apple's Human Interface Guidelines):
- [x] Rounded geometry, Archivo text, simplified sidebar, compact scan card.
- [x] Library Selected pane (single click), full preview on double-click, accessibility actions on tiles.
- [x] Finder-style file sizes everywhere.
- [ ] HIG pass: hit targets ≥ 28 pt on Mac controls, focus rings, keyboard navigation for every page, VoiceOver labels, Reduce Motion/Transparency, Increase Contrast, Dynamic Type-like scaling.
- [ ] Moments tracks, Takes/Tools/Chat panels, Restructure step cells in v3 layout.
- [ ] Model browser "Speed on this Mac" panel checked on screen (results now exist for two models).
- [ ] Title bar: rounded controls, consistent heights, toolbar spacing per HIG.

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
