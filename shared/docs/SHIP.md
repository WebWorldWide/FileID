# FileID — Ship readiness (v1.0)

## 2026-10-08 21:18 CDT merge/release checkpoint

Mac fit-and-pad video export is merged in PR #232 at `3df088db5ef1c73a4cc51c32a8e61ce5b07680e7`; all 19 exact-head checks passed. Eight exact-main workflows are still pending, so merged-main CI is not yet certified. This is a partial next-version capability, with tracked reframing, stabilization, enhancement, portable video parity, and the remaining quality/hardware gates still open. App Store Connect still shows **No Builds** for TestFlight upload 875 at 21:18 CDT; tester installation, packaged acceptance of current source, metadata/compliance, and App Review remain unverified. Adlon is read-only example data.

## 2026-10-08 Mac video frame checkpoint

Native Mac File Tools adds 9:16, 16:9, 1:1, and 4:5 fit-and-pad exports to existing H.264/AAC conversion, with source-preserving staged export/Undo and generated media validation. This is one production-capable format behavior, not the full auto-reframing request: tracked subject crop paths, safe areas, stabilization, AI photo/video enhancement, broader format fidelity, and portable video adapters remain release gates. The uploaded TestFlight build 875 predates this change; do not claim it contains framed export. Exact-head/main CI and packaged sandbox acceptance remain required.

## 2026-10-08 19:58 CDT release gate

Xcode uploaded signed FileID 0.1.1 (875), but authenticated App Store Connect still shows **No Builds** in TestFlight and no selectable distribution build. Processing/ingestion and owner installation are unverified, so TestFlight is not ready. The App Review sign-in-required setting is saved off; the version is saved for manual release after any approval. Screenshots, product copy, support/review contact, content-rights/age data, and export-compliance classification need accurate completion before App Review. Do not submit or publish by inference. If the build remains absent 24 hours after its 12:53 CDT October 8 upload, follow Apple's build-processing support path. The next-version quality, hardware, model, and distribution gates below remain open; Adlon remains read-only example data.

## 2026-10-08 evening validation checkpoint

The compact macOS runtime and opt-in model candidate are merged at `bec145e55f99bc0ad8ef72314150fcab72e7dfc6`. All eight exact-commit CI workflows passed, including Adlon Windows engine x64/ARM64 native/ARM64 cross-build. This is a development checkpoint, not the next-version release: the 2B model has no downloaded weights or inference benchmark, and many feature and hardware gates below remain open. App Store Connect still shows no TestFlight build after Xcode uploaded 0.1.1 (875); do not claim tester installation or sandbox acceptance until Apple exposes a processed build and it runs. Adlon remains read-only example data.

## Compact macOS VLM candidate (2026-10-08)

The opt-in Qwen3.5 2B model and MLX Swift LM 3.32.3 runtime compile with the macOS app and engine. A revision-matched Metal library builds, and an isolated debug app/engine bundle launches. This does not clear the model release gate: run actual local inference on rights-cleared positive/negative/uncertain media, compare it with the incumbent on identical fixtures, measure memory and latency under background load, and validate packaged sandbox behavior. Keep it out of automatic recommendations until those results pass. No model weights are bundled, and Adlon remains read-only example data.

## Related-take proposal checkpoint (2026-10-08)

IPC v1.11 adds read-only visual grouping proposals among selected search results. Local Swift, pinned Rust, and GTK suites and strict Clippy pass; C# and hosted CI remain the merge gate. This is a usability step toward Best Takes, not the release target: no calibrated broad-event detection, catalog-wide grouping job, held-out eight-category ranking benchmark, or GTK/WinUI review UI exists yet. Apple still must expose uploaded macOS build 875 in TestFlight before installation or sandbox acceptance can be claimed. Keep the broader conversion, face, search, chat, privacy, hardware, and distribution gates in this file open.

## Internal testing availability (2026-10-08, 13:08 CDT)

FileID 0.1.1 (875) uploaded through Xcode and passed App Store Connect validation, but Apple had not exposed a build to TestFlight at the latest check. The existing group contains only the owner's Apple account and shows **No Builds Available**. Tester install and sandbox runtime acceptance remain open gates. All three unusable Developer API keys are revoked; zero active team keys remain.

## TestFlight build 875 checkpoint (2026-10-08)

Xcode Organizer App Store Connect validation passed and uploaded signed FileID 0.1.1 (875) using the signed-in Apple team. The local installer is `platforms/apple/dist/FileID-0.1.1-875.pkg` (SHA-256 `886f10c6c944476e5ee281168915f72b4b51bf622dc01f149e44ef79410cdf27`). The previous 874 validation identified missing `LSApplicationCategoryType` and two root-only readable bundle files; both were corrected in packaging before build 875. TestFlight showed **No Builds** immediately after upload at 12:58 CDT, so processing and tester availability are open gates. This upload is from the PR #227 source, with exact-head/main CI still to be verified.

## Best-take review checkpoint (2026-10-08)

Mac and shared Rust now support manual event groups, desired-outcome feedback, preference, correction/Undo, and honest recommendation abstention under IPC v1.10; Mac has a native review window. Automatic broad best-take detection, measured event accuracy, native GTK/WinUI review controls, and the remaining conversion/media/hardware gates are open. The 4B model fixture did not pass memory admission on the 16 GB Mac, so no recognition quality is claimed. Updated signed Mac package `FileID-0.1.1-874.pkg` passes installer and extracted-app signature checks; TestFlight still has no uploaded build because the replacement Apple team key's one-time download did not reach local storage.

## Signed Mac package checkpoint (2026-10-08)

PR #225 is merged at `4e3d708a0bcfa153714ddb3d772f9d52d8294fe1`. Exact-main package `platforms/apple/dist/FileID-0.1.1-867.pkg` passed installer and extracted-app signature checks and a real sandboxed internal-fixture scan/search. App Store Connect has no TestFlight build yet: its one-time API-key download was not delivered by the in-app browser. All eight exact-main CI workflows passed, including Adlon guest Linux/Windows jobs; see `AGENT_HANDOFF.md` for run IDs and upload recovery. This checkpoint is not a release or completion of the next-version gates.

## Next-version checkpoint (2026-10-08)

The isolated Mac fixture now verifies native scan completion, timestamped manual chapter editing, document-content chat retrieval, and original-preserving photo enlargement/Undo. Swift and Rust suites, pinned Rust lint, and actual cross-engine catalog/chat/tool probes pass locally. This is a functional checkpoint, not release acceptance: model-backed accuracy, best takes, stabilization/enhancement/reframing, broad formats, native Linux/Windows execution, strict offline egress, protected-source write tracing, and the 100,000-file/16 GB performance targets remain open. The visual-model test requires explicit rights-clear frames and suitable available memory; default tests skip it. TestFlight has no uploaded build yet.

## Mac TestFlight scaffold (2026-10-03)

`com.fileid.app` and macOS App Store Connect record `6818813859` are registered; internal TestFlight group `FileID Internal Testers` exists with no testers/builds and automatic distribution disabled. Dedicated sandbox entitlements, separate persistent app and implicit-scope IPC bookmarks with bounded engine access leases, app-container model cache, and `platforms/apple/scripts/testflight.sh` are implemented. The local Mac App Store profile and Apple Distribution / Mac Installer Distribution identities are installed, and a signed package passed strict deep verification and installer signature checks. Apple’s API-access acknowledgment is accepted. Team-key creation was authorized, but browser delivery of the one-time `.p8` repeatedly failed; see the newest checkpoint. No build has been uploaded, no testers have been added, and sandbox runtime acceptance is open. See `NEXT.md` and `AGENT_HANDOFF.md` for exact continuation.

## Native persistent search checkpoint (2026-10-02)

Native Library hybrid/similarity IPC and persistent CLIP snapshots/deltas use catalog v23 provenance, SQLite epochs, restart/divergence checks, durable index jobs. PR #201 added progress, pause/cancel/retry and interruption recovery; its eleven checks, 466 native tests and actual-process recovery fixtures passed on merged head. PR #203 adds process-local memory/CPU/I/O admission to Mac catalog indexing and DeepAnalyze work, including resident-model memory claims. Exact-head and merged-main macOS/policy checks passed, as did Adlon Linux and Windows app/engine runs; see STATE.md for runner identities and run IDs. The scheduler still does not cover every heavy file pipeline, separate Apple GPU/ANE budgets, multiple resident models, or full release acceptance. Real 100,000-file latency, text/catalog indexes, person/event filtering, calibrated accuracy, hardware tests, offline network capture, six existing runtime-egress blockers, broader Windows app service-test discovery remain open.

## Latest Mac checkpoint and runner acceptance (2026-10-02)

PRs #195/#196 integrate verified CLIP compatibility and native graph/snapshot groundwork; all 450 native tests pass. Both PRs passed their exact-head checks and fresh merged-main Mac/policy validation. All nineteen health merged-main checks passed, including Adlon Linux/Windows and the genuine zero-skip 48-test IPC gate. Full legacy Windows service tests, physical hardware and the remaining `NEXT_VERSION.md` gates remain open. No release was published.

## Current owner priority and retrieval gate (2026-10-02)

Complete the Mac next-version implementation first; new Windows/Linux implementation and physical acceptance are deferred until the owner works on the PC. Verified CLIP cache identity is a prerequisite, not completion of the persistent search-index gate. Native accuracy, broad best-take evaluation, conversion fidelity, physical hardware, strict offline egress, signing and all remaining `NEXT_VERSION.md` acceptance gates remain open.

## 2026-10-02 — CI infrastructure checkpoint, release still blocked

Reviewed main Linux and Windows builds route to repository-scoped Adlon runners on separate existing CI guest disks; PRs, macOS and native Windows ARM64 remain hosted. See CI_RUNNERS.md for exact service accounts, completed results and the capacity/compiler fixes. Corpus /srv/data remains unmounted and untouched. No release was published for this runner setup.

The Windows app workflow's passing packaging jobs currently skip both suites because of a doubled relative Tests path. Full app-test acceptance is therefore still blocked. An archived repair restores strict TRX execution but exposes missing safety APIs; complete it and require every suite, analyzer/format and native gate before release. The broader feature, model/hardware, held-out accuracy, fidelity, privacy and signing gates in NEXT_VERSION.md remain open. Recovery tags are backups, not release approval.

> The v1.0 release-readiness inventory. Tracks what's done, what's left, and the
> bar each piece is held to. Not a session log — for *what happened* see
> [`STATE.md`](STATE.md); for *what's next* see [`NEXT.md`](NEXT.md); for *why*
> see [`DECISIONS.md`](DECISIONS.md).


## Next-version release status (2026-10-01)

The next-version branch is not release-ready. Its implemented foundation and unfinished features are tracked in [NEXT_VERSION.md](NEXT_VERSION.md). Native macOS catalog/manual-marker UI and sampled timeline jobs and initial local chat/photo/chapter exports do not establish automatic chapters, best takes, full conversion/enhancement, hybrid conversational search, or port parity. The existing notarized v0.1.0 release remains separate from this work.

Block release until all accepted feature milestones, held-out accuracy tests, 100,000-file search/interactive-contention benchmarks, original/derived output fidelity, protected-source write tracing, offline network capture, exact dependency/model distribution review, native platform builds, hardware acceptance, signing, and hosted CI pass. Record measured results; do not present proposed targets as achieved.

## What FileID is

An on-device, privacy-first AI file organizer — tag, dedupe, restructure, rename
tens of thousands of files locally. Two platforms ship at v1.0:

- **Windows** — Rust engine (`fileid-engine`) + WinUI 3 / .NET 8 C# app.
- **macOS** — Swift / SwiftUI app + engine, MLX inference. The visual + UX reference.

Linux is deferred. The two binaries on each platform talk newline-delimited JSON
over stdio; the engine owns a SQLite WAL DB (migrations v1–v12, byte-faithful
across the macOS GRDB and Windows rusqlite stores).

## Non-negotiables

These hold for every shipped feature, on every platform.

- **No telemetry, ever.** No analytics, no crash reporting, no update pings, no
  download instrumentation. The only network egress is user-initiated model
  downloads from `huggingface.co`. CI scans the shipped binaries for telemetry
  strings as a release blocker. See [`PRIVACY.md`](PRIVACY.md).
- **Apache-2.0.** Root `LICENSE`. Every weight FileID downloads by default is
  Apache-2.0 or MIT — no non-commercial or research-only models in the core
  feature set (see [`MODELS.md`](MODELS.md)). The project is free to be
  open-sourced *and* commercialized without a licensing blocker.
- **Performance is a feature.** Match or beat the macOS pipeline on comparable
  hardware; use the GPU/NPU when present.
- **The macOS app is the visual reference.** Windows is a 1:1 port — same palette
  (gold `#FFCC00`, lavender `#B19BCE`, cyan `#A0E2EA`, pink `#F2A6C0`), same
  spring motion, same LavaLampBackground. Native primitives, never web tech.

## Quality bar

For every shipped feature:

1. Works on first run, every time — no "click twice if it didn't work."
2. Empty / loading / error states are designed, not afterthoughts.
3. No dev-tool affordances in user-facing copy — no debug paths, internal IDs, or
   pipeline-phase jargon.
4. Accessible: screen reader reads every meaningful element; keyboard nav covers
   the primary flow; WCAG AA contrast.
5. No silent failures — anything that can fail is surfaced, dismissible, and
   explains what to do.
6. Animation + transitions match the native design language.
7. Documented: appears in the README with at least one screenshot.

For the overall product:

- Crash-free for 1 hour of normal use on a fresh DB over a 50K-file library.
- Memory bounded: peak RSS under budget during scan; idle RSS low after scan.
- Signed, packaged, downloadable from a public GitHub release.
- README + LICENSE + CONTRIBUTING + PRIVACY + screenshots in the repo.

## Model stack (commercial-clean)

Every default weight is Apache-2.0 / MIT and downloaded at runtime from upstream,
SHA-pinned, after the user explicitly triggers it. Full registry in
[`MODELS.md`](MODELS.md).

| Capability | Model | License |
|---|---|---|
| In-scan image tagging (primary) | RAM++ Swin-L @384 — 4585-tag ONNX, per-class thresholds + generic-tag suppress-list | Apache-2.0 |
| Image tagging (fallback) | CLIP zero-shot scene tags (when RAM++ isn't installed) | MIT |
| Image + text semantic search | CLIP ViT-B/32 — 512-d embeddings | MIT |
| Face detection + 5-pt landmarks | YuNet | MIT |
| Face embedding | SFace — 128-d, 5-point aligned | Apache-2.0 |
| Deep Analyze (VLM, opt-in) | Qwen2.5-VL 7B (default) · Gemma 3 4B · Mistral-Small-3.2 24B, via llama.cpp | Apache-2.0 (Gemma: Gemma Terms) |

Removed in the commercial-clean pass: the non-commercial Qwen2.5-VL-3B,
InsightFace ArcFace/SCRFD, and research-only MobileCLIP-S2.

On Windows, ONNX Runtime auto-selects the execution provider
(CUDA / TensorRT / DirectML / OpenVINO / QNN / CPU). NVIDIA cards without the CUDA
pack installed run on DirectML — fully functional, ~80–90% of native CUDA
throughput for ML inference. macOS uses MLX + CoreML + the Neural Engine.

## Restructure — butler-grade overhaul

Restructure is being rebuilt from a flat rule cascade into a "butler" that
proposes a reorganization feeling like *you* organized it: cluster by meaning,
extend your existing folder conventions, auto-file what it's sure of and ask about
the rest, always previewable and reversible. Full design in
[`RESTRUCTURE.md`](RESTRUCTURE.md).

| Phase | Scope | Status |
|---|---|---|
| **P1** | Engine: semantic + learn-your-style classify — fuse CLIP + tags + time, density-cluster (reuses `identity_clustering`), route each cluster to the nearest existing folder prototype or propose a new group; rule cascade is the fallback | **Landed (both engines)** |
| **R1** | Extend the butler to **all file types** — additive filename+tag bag-of-words pass for documents/video/audio (separate `nonImageProfile`, junk folders barred as prototypes); image path byte-identical | **Landed (both engines)** — owner threshold calibration pending |
| **P2** | VLM cluster naming (label-then-reason, constrained decoding) + label-then-group hierarchy | Planned (next) |
| **P3** | Confidence tiers (auto ≥ 0.95 / suggest 0.70–0.95 / ask < 0.70) gated by action risk + reversible command journal + learn-from-corrections | Planned |
| **P4** | Win2D Sankey upgrade (barycentre ordering, destination-color links, Okabe-Ito palette, hover path-highlight, drill-down) + before/after tree + weight sliders | Planned |

The Sankey is the chosen primary reorg visualization. macOS mirrors each phase
after Windows lands.

## CI gates

A green CI run is required before any feature is called done. Telemetry +
source-URL scans are hard release blockers — no exceptions.

- **`windows-engine.yml`** (x64 + arm64-native + arm64-cross): `cargo fmt`,
  `cargo clippy --all-targets -D warnings`, `cargo-deny` (license + advisory +
  dup-version + ban), source-URL allowlist, release build, `cargo test`, engine
  startup + `verifyCudaPack` smokes, telemetry-string scan.
- **`windows-app.yml`** (x64 + arm64): `msbuild` Debug + Release,
  self-contained publish, xUnit test projects, `dotnet format --verify-no-changes`,
  vulnerable-package scan, telemetry-string scan, app startup smoke.
- **`macos.yml`**: `swift build` (app + engine), `swift test`, source-URL
  allowlist, telemetry-string scan, engine startup smoke.

Dev verifies headlessly in the agent environment (`cargo clippy`/`test`,
`dotnet build`/`test`/`format`); on-hardware verification runs on an RTX 2060
against the `G:\TrueNAS` corpus via `platforms/windows/build/iterate.ps1` +
`build/scan_assertions.py` (asserts file count, low failure rate, RAM++/CLIP tags
present, 128-d/512-byte SFace prints, person clusters formed).

## Remaining to v1.0

Priorities in [`NEXT.md`](NEXT.md). **Done as of 2026-06-10** (branch `fix/bug-audit-sweep`,
pending CI + hardware UAT): the full bug-audit campaign (zero open confirmed findings across
all records — see STATE.md), security hardening (SHA256 manifest + TLS pinning + tokenizer
bounds — SECURITY.md), macOS Finder-tag undo + bulk-rename polish, and the **macOS
sign/notarize/DMG pipeline** (`platforms/apple/scripts/release.sh`; real signing owner-gated
on the Developer ID cert). The major open items:

- **Restructure P2–P4** — VLM naming, confidence tiers + journal, Win2D Sankey.
- **macOS lockstep (WS-MAC)** — mirror the commercial-clean swap into the Swift
  app: RAM++ tagger, ViT-B/32, SFace (128-d) with Apple Vision detection, VLM
  ladder. Goal: a face DB written on one platform round-trips on the other. Until
  then, treat face DBs as platform-local.
- **Throughput re-baseline** — DirectML on the RTX 2060 measures ~6–7 files/s
  (RAM++ Swin-L-bound); host the ORT CUDA EP DLLs for the NVIDIA 3–5× path.
- **Face clustering** — Pass-1 single-linkage chains distinct people through
  bridge faces on very large libraries; structural fix (mutual-kNN / density-gated
  edges) + calibration against labeled faces.
- **Rename-heal exact-duplicate fix** — coexisting byte-identical files currently
  collapse to one row; fix so N pairs yield 2N rows and Cleanup surfaces the group.
- **Packaging + signing (Windows)** — WiX MSI + Authenticode EV cert.
- **Per-vendor on-hardware verification** — see the matrix below.

## Appendix — Windows per-vendor verification matrix

The engine's ORT execution-provider picker auto-detects the best accelerator on
each vendor's silicon. **GPU Performance Packs were removed** (no shippable,
license-compliant per-vendor URLs) — DirectML is the universal GPU path for every
D3D12-capable vendor, CPU is the floor. Rationale in `DECISIONS.md`. The
Intel OpenVINO and Snapdragon QNN packs remain unhosted; power users who install a
vendor SDK locally get the engine's auto-pick (OpenVINO / QNN), but the default
ship target is DirectML or CPU.

Run a 1,000-file scan on representative hardware per row and confirm the engine log
+ throughput.

| Vendor | Reference hardware | Expected EP | Status |
|--------|--------------------|-------------|--------|
| NVIDIA | RTX 2060 / 3060 / 4060+ | DirectML (CUDA with the EP DLLs) | ⬜ pending re-baseline w/ RAM++ |
| AMD | RX 6600 / 7600+ | DirectML | ⬜ pending |
| Intel | Arc A380 / Iris Xe / UHD iGPU | DirectML | ⬜ pending |
| Qualcomm | Snapdragon X Elite | CPU (QNN if the SDK is installed) | ⬜ pending |
| CPU | i7-12700 / Ryzen 7 7700 | CPU | ⬜ pending |

### Per-vendor acceptance (each row passes when all hold)

1. **Engine log shows the expected EP.** `%LOCALAPPDATA%\FileID\logs\app.log`
   after a fresh scan — the `ep=` field on `[EP] built session` matches the table.
2. **Throughput target met** over a representative 1,000-file image library.
3. **Memory ceiling honored** — peak RSS within budget across the scan.
4. **No crash dumps** in `%LOCALAPPDATA%\CrashDumps\` during the run.
5. **Deep Analyze succeeds on 10 sample images** (llama.cpp Vulkan covers NVIDIA /
   AMD / Intel; CPU on Snapdragon) — surfaced via `[VLM]` log lines.
6. **`iterate.ps1` corpus regression green** on the host (`scan_assertions.py`).

Code-level certainty is in place: `models/runtime.rs` unit tests cover every
vendor's EP pick + fallback, and the picker fails safely down the chain when an EP
can't build a session. Hardware certainty — proving drivers, DLLs, and ORT line up
on real silicon — is the missing layer the six checks above provide.

### Build pre-reqs for the verification pass

- Authenticode EV cert installed + `FILEID_EV_THUMBPRINT` set, so signed binaries
  aren't SmartScreen-blocked on first run.
- `llama_runtime_x64` (Vulkan llama.cpp) downloadable from GitHub.

### Lane gate

Windows v1.0 ships when at least 4 of the rows are green — CPU plus at least one
each from NVIDIA / AMD / Intel. All rows is the goal; Snapdragon may launch in a
follow-on if hardware availability blocks. macOS ships once WS-MAC lockstep lands
and its existing CI + on-device checks pass.

## Initial tools milestone

Native macOS photo/chapter exports and Rust adapter contracts now exist; TOOLS.md lists exact supported pairs and fidelity/recovery limits. These do not satisfy the full conversion, enhancement, reframing, broad toolbox, port UI, or hardware gates. Keep the draft PR open until the accepted feature scope and release checks are actually complete.

Initial memory headroom checks, native residency cancellation, and decoder-watchdog coverage are implemented. Parallel model/resource admission, inference worker isolation, and model/hardware measurements remain release gates; refer to SCHEDULER.md.

Face-cache acceptance additionally requires legacy clustering-space isolation, complete resumable refresh/backlog handling, same-size/same-mtime replacement detection, held-out identity precision/recall and actual model inference checks. v22 provenance and synthetic geometry/migration tests establish none of those accuracy targets.

Native H.264/AAC SDR export has generated portrait/audio/metadata/Undo tests. It still requires portable worker/UI parity and the full real-world timing/color/fidelity matrix before release; its bounded presets do not establish stabilization, AI enhancement or tracked reframing.

Face comparison rejects unknown or mixed model/processing namespaces, stale revisions and invalid 128-d vectors before persistence. Stable reclustering identity persistence and atomic explicit-merge regression fixtures pass locally. Explicit source aliases/history, incremental refresh/assignment and held-out accuracy calibration remain release gates. Legacy person centroids lack provenance and cannot drive inheritance. See FACE_CACHE.md for whole-pass refresh limitations.


### Health-contract follow-up (under review, 2026-10-02)

A dedicated Windows IPC schema suite/report/format gate replaces the prior absence of IPC test execution. The isolated Adlon Windows VM passes 48 IPC tests and nine report-guard fixtures; new exact-head CI is still pending. The legacy app-service test discovery is still broken, so the full Windows safety/test repair and app-suite release gate remain open. Actual-process nonce/PID and invalid-probe checks are now required in macOS, Linux and native Windows engine CI. No server runner or corpus configuration changed in this follow-up.


October 2026 native index-job follow-up: persistent state, interruption recovery, pause/cancel/retry and conservative memory admission now have synthetic regression coverage. Complete exact-head/main CI and actual allocation/latency measurements before calling admission or end-to-end retrieval accepted. General atomic resource budgets, dense analysis/best takes, enhancement/broad converters, port UI/hardware, accuracy/fidelity, strict privacy and distribution gates remain open.
