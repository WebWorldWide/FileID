# NEXT — resume here

Short, current priorities only. History lives in `STATE.md` (newest first) and git; release gates live in `SHIP.md`; the feature ledger lives in `NEXT_VERSION.md`; model choices live in `MODELS.md`.

## Where things stand (2026-10-13)

- `main` is the only branch. Unmerged checkpoint work is recoverable from `archive/2026-10-13/*` tags (`BRANCH_AUDIT.md`).
- The shared Rust engine (`platforms/windows/src/engine`) serves Windows, Linux, the CLI and the TUI. macOS has its own Swift engine and remains the behavioral reference.
- Test hosts: the Linux VM (`adlon-ci-linux`), the Windows 11 VM (`ADLON-CI-WIN`, also a self-hosted CI runner) and the owner's Mac (headless `swift build`/`swift test` over SSH only; the owner does interactive macOS testing and package signing locally).

## In flight

1. **Portable timeline and significant moments** (`feat/portable-timeline`). The shared engine runs `enqueueTimeline` with the vetted FFmpeg runtime and the resident VLM: change-guided samples, overlapping moment windows, draft chapters, durable pause/resume/cancel. GTK and WinUI expose quick-samples/significant-moments and real job controls. Verified: Linux engine (639 lib tests, strict Clippy), GTK (50 tests + real-engine Xvfb fixture), Windows engine timeline tests and .NET (54 IPC + 576 App). Remaining: hosted CI, a real-model run (no VM has VLM weights), portable speech (needs the whisper.cpp runtime review below).
2. **Extended model catalog and benchmarking.** Generate `shared/models/catalog.json` from Hugging Face (trusted publishers, upstream license Apache-2.0/MIT or opt-in Gemma, immutable revisions, SHA256 pins). Resolve catalog ids in the engine registry, add a `listModels` IPC with install status and hardware fit, then a `fileid-bench` harness (load time, prefill/decode tok/s, peak memory, per-file latency, task quality on rights-clear fixtures) and magnitude-style llama-server auto-tuning (threads, batch, flash attention, partial GPU offload) that keeps the fastest configuration whose greedy output is unchanged. Entries stay `candidate` until benchmarked on real hardware.

## Next, in dependency order (see `NEXT_VERSION.md` M0–M6)

1. **Portable speech**: review the archived `parity/completion-runtime` llama.cpp/whisper.cpp Linux source builder (dependency, license, provenance, offline policy) before extracting it; then timestamped speech passages on Windows/Linux.
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
