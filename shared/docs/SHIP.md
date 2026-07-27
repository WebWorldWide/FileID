# FileID — Ship readiness (v1.0)

> The v1.0 release-readiness inventory. Tracks what's done, what's left, and the
> bar each piece is held to. Not a session log — for *what happened* see
> [`STATE.md`](STATE.md); for *what's next* see [`NEXT.md`](NEXT.md); for *why*
> see [`DECISIONS.md`](DECISIONS.md).

## What FileID is

An on-device, privacy-first AI file organizer — tag, dedupe, restructure, rename
tens of thousands of files locally. The primary v1.0 targets are:

- **Windows** — Rust engine (`fileid-engine`) + WinUI 3 / .NET 8 C# app.
- **macOS** — Swift / SwiftUI app + engine, MLX inference. The visual + UX reference.
- **Linux** — shared Rust engine + native GTK4/libadwaita app, with packaging and behavioral UAT still hardware-gated.

Linux ships all six tabs over the shared engine. The two binaries on each desktop
platform talk newline-delimited JSON over stdio; the engine owns a SQLite WAL DB
(migrations v1–v19, byte-faithful across the macOS GRDB and Windows/Linux
rusqlite stores).

## Non-negotiables

These hold for every shipped feature, on every platform.

- **No telemetry, ever.** No analytics, no crash reporting, no update pings, no
  download instrumentation. The only network egress is user-initiated model
  downloads from `huggingface.co`. CI scans the shipped binaries for telemetry
  strings as a release blocker. See [`PRIVACY.md`](PRIVACY.md).
- **Apache-2.0 project, commercial-clean models.** Root `LICENSE`. Core weights
  are Apache-2.0/MIT and no non-commercial-only model may ship. Optional
  restricted models such as Gemma are commercially usable under separately
  accepted upstream terms (see [`MODELS.md`](MODELS.md)).
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

## Current audit status (2026-07-27)

Locally runnable source gates are green on Windows and native WSL: locked Rust
format/clippy/tests for the shared engine, CLI, TUI, and GTK app; Linux release
build; Windows x64 Release app build plus App/IPC tests and format; 71 shared
policy regressions; model-license/bootstrap/workflow/current-doc policy; and the
reviewed runtime-egress known-blocker baseline. A current read-only Adlon
fingerprint matches the preserved pre-audit result exactly. Final independent
review found no remaining locally actionable blocker/high/medium code issue.

This does **not** authorize publication. The strict no-flag runtime-egress gate
still rejects the ten reviewed GitHub/NVIDIA archives and widened downloader host
set described in `PRIVACY.md`; release staging must remain blocked until those
artifacts are removed or mirrored to Hugging Face. Native macOS, ARM64 and other
vendor hardware, distro/clean-VM lifecycle, accessibility, signing/notarization,
and hosted CI also remain required. Blocking filesystem/codec calls receive
bounded cooperative shutdown, not a guarantee that pathological kernel reads can
be cancelled inside the process.

## Model stack (commercial-clean)

Core weights are Apache-2.0/MIT. Optional restricted weights such as Gemma use
separately accepted commercially usable upstream terms. Downloads are user-triggered
and SHA/revision-pinned; the full registry and acceptance policies live in
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

On Windows, ONNX Runtime selects among execution providers actually present on
the machine. The release-approved universal GPU path is DirectML, with CPU as the
floor; owner-supplied CUDA/OpenVINO/QNN runtimes are development/BYO paths, not
approved product Performance Packs. The current development registry still
contains legacy GitHub/NVIDIA runtime downloads, so strict runtime-egress policy
blocks release staging until those entries are removed or mirrored to Hugging Face.
macOS uses MLX + CoreML + the Neural Engine.

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
- **`linux.yml`**: native engine/CLI/TUI/GTK format, clippy, tests, builds,
  schema checks, and binary privacy scanning.
- **`packaging.yml`**: required GNOME 49 Flatpak build from generated pinned
  Cargo sources plus SHA-pinned ONNX Runtime, with Cargo forced offline.
- **`tools.yml` / `release.yml`**: staged native-tool privacy, exact archive
  membership/checksums, and release validation. Publication remains separately
  gated on signing credentials and job-scoped write permission.
- **`policy.yml`**: rejects mutable external GitHub Action references.
- **`pages.yml`**: builds and deploys the static website when Pages is enabled.

Dev verifies headlessly in the agent environment (`cargo clippy`/`test`,
`dotnet build`/`test`/`format`); Windows on-hardware verification uses
`platforms/windows/build/iterate.ps1` + `build/scan_assertions.py` against the
configured real corpus (asserting count, failure rate, RAM++/CLIP tags,
128-d/512-byte SFace prints, and person clusters).

## Remaining to v1.0

Priorities in [`NEXT.md`](NEXT.md). **Done as of 2026-06-10** (branch `fix/bug-audit-sweep`,
pending CI + hardware UAT): the full bug-audit campaign (zero open confirmed findings across
all records — see STATE.md), security hardening (SHA256 manifest + TLS pinning + tokenizer
bounds — SECURITY.md), macOS Finder-tag undo + bulk-rename polish, and the **macOS
sign/notarize/DMG pipeline** (`platforms/apple/scripts/release.sh`; real signing owner-gated
on the Developer ID cert). The major open items:

- **Restructure P2–P4** — VLM naming, confidence tiers + journal, Win2D Sankey.
- **macOS lockstep (WS-MAC) — swap LANDED (2026-07), on-hardware parity verify remains.**
  The commercial-clean stack (RAM++ tagger, ViT-B/32, SFace 128-d with Apple Vision
  detection, VLM ladder) is wired as primary on `main` (verified statically —
  `shared/docs/MACOS_AUDIT_2026-07.md`). Remaining: confirm on a Mac that a face DB
  written on one platform round-trips on the other; treat face DBs as platform-local
  until that check passes. Also open on Mac: F1 (non-image Exact-dedup content_hash)
  from the audit.
- **Throughput re-baseline — DONE 2026-07 on RTX 5080.** Measured ~40 f/s full corpus
  on DirectML (~5× the 2060); the GPU is dispatch-bound (idle p50=19%). Owner-local
  CUDA EP provisioning remains a development performance experiment, but no CUDA
  Performance Pack is approved for product distribution. See STATE.md.
- **Face clustering** — DONE on the Rust engine (Windows/Linux/CLI/TUI): mutual-kNN
  default-on + a pre-clustering quality gate + label-calibrated thresholds
  (pass1 0.50) took the owner's labelled People-tab precision/recall to 1.0
  (STATE 2026-07-05). REMAINING: macOS Swift carries the mechanisms (default-off)
  but needs its own on-Mac label-calibration pass (Apple Vision quality scale +
  FaceAlign) before adopting the values.
- **Rename-heal exact-duplicate fix** — coexisting byte-identical files currently
  collapse to one row; fix so N pairs yield 2N rows and Cleanup surfaces the group.
- **Packaging + signing (Windows)** — branded WiX MSI/Burn builds are verified; select and authenticate a public-trust provider using `WINDOWS_SIGNING.md`.
- **Per-vendor on-hardware verification** — see the matrix below.

## Appendix — Windows per-vendor verification matrix

The engine's ORT execution-provider picker auto-detects the best accelerator on
each vendor's silicon. **GPU Performance Packs are not approved for release** (no
shippable, license-compliant per-vendor URLs) — DirectML is the universal GPU path
for every D3D12-capable vendor and CPU is the floor. Legacy pack definitions remain
in the development registry and are a deliberate strict-egress release blocker,
not a shippable product path. Rationale in `DECISIONS.md`. Power users who install
a vendor SDK locally can use the engine's auto-pick (CUDA / OpenVINO / QNN), but
the default ship target is DirectML or CPU.

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

- Public-trust Authenticode provider configured through `WINDOWS_SIGNING.md`; verify signatures on a clean Windows VM. Signing improves reputation but cannot guarantee first-run SmartScreen suppression.
- Mirror `llama_runtime_x64` and the remaining reviewed GitHub/NVIDIA runtime archives to Hugging Face, or remove those download paths; the strict egress gate must pass before staging.

### Lane gate

Windows v1.0 ships when at least 4 of the rows are green — CPU plus at least one
each from NVIDIA / AMD / Intel. All rows is the goal; Snapdragon may launch in a
follow-on if hardware availability blocks. macOS ships once WS-MAC lockstep lands
and its existing CI + on-device checks pass.
