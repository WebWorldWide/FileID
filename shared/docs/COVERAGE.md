# FileID — Test Coverage

> Where the tests live, what they exercise, and how to measure line coverage locally.
>
> **There is no committed coverage baseline and no coverage gate in CI.** CI runs the
> test suites (`cargo test`, `dotnet test`, `swift test`) and fails on a red test, but it
> does not measure or threshold coverage. The commands below are for a developer who wants
> to look at coverage on their own machine. If a hard per-module gate is added later, this
> file is where the baseline and rules would be documented.

## What CI actually enforces

| Workflow | Test gate | Notes |
|---|---|---|
| `windows-engine.yml` | `cargo test --all-targets` (x64 + arm64-native) | Plus `cargo fmt`, `clippy --all-targets -D warnings`, `cargo-deny`, source-URL allowlist, startup + `verifyCudaPack` smokes, telemetry-string scan. arm64-cross skips tests. |
| `windows-app.yml` | `dotnet test` on both test projects (x64) | Plus `dotnet format --verify-no-changes`, vuln scan, telemetry scan, app-startup smoke. |
| `macos.yml` | `swift test` | Plus engine-startup smoke, source-URL allowlist, telemetry scan. |

None of these collect or compare coverage.

## Measuring coverage locally (optional)

| Platform | Command | Output |
|---|---|---|
| Rust (Windows) | `cd platforms/windows/src/engine && cargo llvm-cov --workspace --html --lcov --output-dir target/coverage` | HTML at `target/coverage/html/index.html`, lcov at `target/coverage/lcov.info`. Requires `cargo install cargo-llvm-cov` + the `llvm-tools-preview` rustup component. |
| .NET (Windows) | `cd platforms/windows/Tests && dotnet test --collect:"XPlat Code Coverage" --results-directory coverage` then `reportgenerator -reports:coverage/**/coverage.cobertura.xml -targetdir:coverage/html` | HTML at `coverage/html/index.html`. Requires the `dotnet-reportgenerator-globaltool`. |
| Swift (macOS) | `cd platforms/apple && swift test --enable-code-coverage` | profdata under `.build/.../codecov/`; export with `xcrun llvm-cov export -instr-profile <profdata> <test-bundle>`. |

These are not run in CI and the tooling is not pinned; treat the numbers as a local diagnostic.

## Rust (Windows engine)

228 unit tests live in `#[cfg(test)]` modules across the crate (the `cargo test` gate runs all of
them on x64 + arm64-native). Coverage is strongest on the pure-logic modules and absent on the ORT
inference paths, which need shipped model DLLs and are exercised only by `build/iterate.ps1` on real
hardware.

Well-covered (pure logic, no I/O or GPU):

- `util/hmac` — RFC 4231 vectors + constant-time-eq edges.
- `util/path_safety` — example + property tests for path containment/normalization.
- `util/zip` — extract + zip-slip rejection.
- `util/keywords`, `util/content_hash`, `util/hnsw_index` — keyword extraction, BLAKE3 hashing, HNSW index.
- `ipc/` (`bounded_read`, `mod`) — framing, bounded reads, command/event round-trip.
- `db/migrations` — migration + pragma init (v1–v12).
- `platform.rs` — worker-cap math, memory probe, path redaction.
- `commands/trash` + `commands/trash_log` — HMAC seal/verify cycle.
- `commands/deep_analyze` — caption-chunk splitting.

Behaviorally tested (logic covered; the ORT/codec leaf is mocked or skipped):

- `pipeline/discovery` — kind filter + sort.
- `pipeline/tagging` — RAM++ generic-tag suppress-list + scene fallback gating.
- `pipeline/dbwriter` — batch + rename-heal lookup.
- `pipeline/face_clustering`, `pipeline/identity_clustering` — cluster stability + uncertain-band behavior.
- `pipeline/restructure`, `pipeline/restructure_apply`, `pipeline/restructure_semantic`, `pipeline/cluster_suggestions` — category/folder classification, containment, semantic fusion, cluster summaries.
- `pipeline/doc_extract`, `pipeline/audio_meta` — doc keyword + audio-metadata extraction.
- `models/registry`, `models/variants`, `models/runtime` — registry URL/alias/sentinel invariants, per-EP variant resolution, session tuning.
- `models/ram_plus`, `models/sface`, `models/yunet`, `models/face_align` — preprocessing + L2-normalize + alignment math (the `Session::run` call itself is not unit-tested).
- `models/scene_vocab`, `models/wordpiece_tokenizer`, `models/vlm` — scene-label set, tokenizer, VLM prompt/parse helpers.
- `shell/tags`, `shell/ocr` — tag round-trip; OCR plumbing.

Not unit-tested (integration-only; see exempt list): the `commands/*` dispatch handlers (covered
indirectly via the pipeline tests + CI smokes), `coordinator.rs` / `scan_session.rs` / `job_queue.rs`
state machines, and every `models::*` ORT `Session::builder()`/`run()` path.

## .NET (Windows app + IpcSchema)

Two xUnit projects under `platforms/windows/Tests/`, both run by `windows-app.yml`:

- `FileID.IpcSchema.Tests` — `IpcCommandTests`, `IpcEventTests`: encode/decode round-trip for the
  generated DTOs against `shared/ipc-schema/ipc.schema.json`.
- `FileID.App.Tests` — `PathRedactorTests`, `UndoStackTests`, `SafeOpenTests`,
  `ThumbnailDiskCacheTests`, `AppSettingsTests`, `ViewModelBindingTests`: path redaction (incl.
  case-insensitive Windows + macOS paths), undo/redo stack, safe-open extension allowlist, thumbnail
  LRU cache, AppSettings JSON round-trip + migration, and view-model binding/state.

Untested by design: WinUI views (`Views/*.xaml.cs`), the engine process/IPC plumbing
(`EngineProcessManager`, `IpcDispatcher`), the model installer, and `ReadStore` — these are
UI-thread-bound or require a live engine subprocess, and are covered by the app-startup CI smoke +
on-hardware runs rather than unit tests.

## Swift (macOS engine + app)

`swift test` runs `SharedTests` + `EngineTests` (the macos.yml gate). Corpus-dependent tests skip
themselves when `Tests/Corpus/` is absent (it's fetched locally, too slow/fragile for CI). The macOS
side is the reference implementation but the Windows port currently carries the broader unit suite;
macOS coverage is not separately tracked here.

## Not under test (justified, not silent gaps)

| File / area | Reason |
|---|---|
| `Theme/LavaLampBackground.swift` + `Motion/LavaLampBackground.cs`, Win2D canvases (Sankey, IridescentBorder), Metal/Win2D shaders | GPU-resident visuals; no CPU-runnable assertion path. |
| `shell/video.rs` Media Foundation keyframe path | Needs a real codec; covered via `iterate.ps1` only. |
| `models/{ram_plus,sface,yunet,clip_text,vlm,florence2}.rs` ORT `Session` run paths | Need shipped model DLLs; covered via on-hardware `iterate.ps1`. |
| `main.rs` `main()` / `async_main()` | Covered by the engine-startup CI smoke, not unit tests. |
| WinUI `*.xaml.cs` wiring, `MainWindow` Mica/Acrylic + drag region | UI-thread-bound; covered by the app-startup smoke + manual verification. |

> `models/scrfd.rs` and `models/mobileclip.rs` still carry tests but are dead reference code — the
> shipped pipeline is YuNet + SFace (faces) and CLIP ViT-B/32 (embeddings). They are slated for
> removal; until then their tests still run.

## On-hardware verification

Behavior that no unit test or CI smoke can prove — real ORT execution-provider binding, GPU/NPU
throughput, model accuracy on real files — is verified on the dev's RTX 2060 against the `G:\TrueNAS`
corpus via `platforms/windows/build/iterate.ps1` (+ `build/scan_assertions.py`). That harness, not a
coverage percentage, is the source of truth for the inference paths.
