# NEXT — resume here

Short, current priorities only. History lives in `STATE.md` (newest first) and git; release gates live in `SHIP.md`; the feature ledger lives in `NEXT_VERSION.md`; model choices live in `MODELS.md`.

## Where things stand (2026-10-15)

- `main` is the only branch. Unmerged checkpoint work is recoverable from `archive/2026-10-13/*` tags; the pre-merge Mac completion checkpoint (including its dropped portable face-refresh Rust slice) is commit `03c50bcd`.
- The Mac completion pass (best takes, video tools, moments/cast, reviewed naming, People refresh, MLX model browser, right-side chat) is folded into `main` together with the restored shared engine (#248) and the GTK theme fix (#249); IPC is 1.16.0.
- The shared Rust engine (`platforms/windows/src/engine`) serves Windows, Linux, the CLI and the TUI. macOS has its own Swift engine and remains the behavioral reference.
- Test hosts: the Linux VM (`adlon-ci-linux`), the Windows 11 VM (`ADLON-CI-WIN`, also a self-hosted CI runner) and the owner's Mac (headless `swift build`/`swift test` over SSH only; the owner does interactive macOS testing and package signing locally).

## In flight

0. **Finish the August-rollback restore** (engine landed first): restore the GTK app (paged restructure plans, engine client, settings) and the WinUI app (47 conflicted files) area by area, each as its own PR verified on its host. The macOS engine now implements `reassignFace`; the remaining portable-only commands (`findMergeSuggestions`, `embedTextQuery`, `embedImageQuery`, `restoreFromTrash`, `revertMerge`, `generateVideoThumbnail`) are served in-app on macOS and stay `not_implemented_yet` in its engine.
1. **Redesign** (FileID Prototype v3, contract in `DESIGN_V3.md`): macOS shell, design system, fonts, lava, Library/People/Cleanup/Settings/Welcome landed. Finish the macOS page layouts (Library selected pane, Moments tracks, Takes/Tools/Chat panels, Restructure step cells), then port the shell to WinUI and GTK (tokens are in; the title bar, area sidebar, pipeline block and page headers are not). Keep the grain fine (2× multi-octave) and polish each page fully.
2. **Benchmarks on real hardware**: run the model catalog benchmark on the RTX 2060 PC and the Mac; exercise the WinUI Models workbench interactively; add MLX benchmarking on macOS (catalog MLX loading exists through the bundled `MLXModelCatalog`; `listModels` reports bundled entries as `candidate`).
3. **Portable People/naming parity**: re-derive the dropped portable face-refresh/crop-publication/naming-context slice (commit `03c50bcd`) on top of the restored shared engine, per the MAC_FINISH.md handoff matrix.

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
