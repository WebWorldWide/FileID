# FileID — Ship readiness (v1.0)

> The v1.0 release-readiness inventory. Tracks what's done, what's left, and the
> bar each piece is held to. Not a session log — for *what happened* see
> [`STATE.md`](STATE.md); for *what's next* see [`NEXT.md`](NEXT.md); for *why*
> see [`DECISIONS.md`](DECISIONS.md).

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
| **P1** | Engine: semantic + learn-your-style classify — fuse CLIP + tags + time, density-cluster (reuses `identity_clustering`), route each cluster to the nearest existing folder prototype or propose a new group; rule cascade is the fallback | **Landed (engine)** |
| **P2** | VLM cluster naming (label-then-reason, constrained decoding) + label-then-group hierarchy | Planned |
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

Priorities in [`NEXT.md`](NEXT.md). The major open items:

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
