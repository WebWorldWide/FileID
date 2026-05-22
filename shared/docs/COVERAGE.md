# FileID — Code Coverage

> Per-module line-coverage targets and the current actuals. Regenerated each release. A drop > 2 percentage points on any module blocks merge (Phase 8 CI gate).

## How to regenerate

| Platform | Command | Output |
|---|---|---|
| Rust (Windows) | `cd platforms/windows/src/engine && cargo llvm-cov --workspace --html --lcov --output-dir target/coverage` | HTML at `target/coverage/index.html`, lcov at `target/coverage/lcov.info` |
| .NET (Windows) | `cd platforms/windows && dotnet test --collect:"XPlat Code Coverage" --results-directory coverage` then `reportgenerator -reports:coverage/**/coverage.cobertura.xml -targetdir:coverage/html` | HTML at `coverage/html/index.html` |
| Swift (macOS) | `cd platforms/apple && swift test --enable-code-coverage` then `xcrun llvm-cov export --format=lcov -instr-profile=$(swift test --show-codecov-path | head -n1 | xargs dirname)/default.profdata $(swift build --show-bin-path)/FileIDPackageTests.xctest/Contents/MacOS/FileIDPackageTests > coverage/swift.lcov` | lcov at `coverage/swift.lcov` |

Targets are **line coverage**, not branch. Branch is nice-to-have; line is the gate.

## Rust (Windows engine)

| Module | Target | Status | Notes |
|---|---:|:---:|---|
| `db/` | ≥ 90% | TBD | Migrations + pragma init have inline tests |
| `ipc/` | ≥ 90% | TBD | Includes `bounded_read` (6 tests) + `sink` |
| `util/hmac` | ≥ 95% | covered | RFC 4231 vectors + constant-time-eq edge cases (5 tests) |
| `util/path_safety` | ≥ 95% | covered | Example + property tests (6 tests) |
| `util/zip` | ≥ 95% | covered | Extract + slip rejection (2 tests) |
| `pipeline/discovery` | ≥ 85% | partial | Kind filter + sort tests exist; symlink + permission-denied pending |
| `pipeline/tagging` | ≥ 85% | partial | Behavioral tests exist; mock-ORT helpers pending |
| `pipeline/dbwriter` | ≥ 85% | partial | Batch boundary + FK partial-write tests pending |
| `pipeline/face_clustering` | ≥ 85% | partial | Cluster-stability + uncertain-band tests exist |
| `pipeline/identity_clustering` | ≥ 85% | partial | Empty + clear-identities tests exist |
| `pipeline/restructure` | ≥ 85% | partial | Category + folder classification tests exist |
| `pipeline/restructure_apply` | ≥ 85% | partial | Containment tests exist |
| `pipeline/deep_analyze` | ≥ 85% | partial | Size-estimate test exists; cancel-during-inference pending |
| `pipeline/batch_clip` | ≥ 85% | pending | Batch boundary + L2-norm tests pending |
| `models/` | ≥ 75% | pending | EP-priority + CPU-fallback (mock ORT) pending |
| `commands/hardware` | ≥ 80% | pending | `emit_ready` + `verifyCudaPack` tests pending |
| `commands/embed` | ≥ 80% | pending | Mock-DB tests pending |
| `commands/restructure` | ≥ 80% | pending | Tests rely on integration; partial |
| `commands/face_clustering` | ≥ 80% | pending | Tested via `pipeline::face_clustering` indirectly |
| `commands/bulk` | ≥ 80% | pending | Per-handler tests pending |
| `commands/trash` + `trash_log` | ≥ 80% | covered | HMAC seal/verify cycle test exists |
| `commands/deep_analyze` | ≥ 80% | covered | Caption-chunks tests (4) |
| `commands/prewarm` | ≥ 80% | pending | Mock-HTTP tests pending |
| `commands/scan` | ≥ 80% | pending | Tested via integration |
| `shell/` | ≥ 70% | partial | `tags` round-trip exists; Win32 error paths gap |
| `platform.rs` | ≥ 80% | partial | Worker-cap math + path-redaction tests exist |
| `coordinator.rs`, `scan_session.rs`, `job_queue.rs` | ≥ 80% | pending | Pause/resume/cancel state tests pending |

## .NET (Windows app + IpcSchema)

| Project / Module | Target | Status | Notes |
|---|---:|:---:|---|
| `FileID.IpcSchema` (all) | ≥ 95% | 30 tests | Round-trip + decode |
| `FileID.App/Services/PathRedactor` | ≥ 95% | 6 tests | Includes case-insensitive Win + cross-platform Mac path |
| `FileID.App/Services/UndoStack` | ≥ 90% | 5 tests | Push/pop/redo/capacity |
| `FileID.App/Services/EngineProcessManager` | ≥ 80% | pending | Mock-`Process` harness not yet written (test-backlog item, non-blocking) |
| `FileID.App/Services/IpcDispatcher` | ≥ 80% | pending | Synthetic-stdout pending |
| `FileID.App/Services/ModelInstallerService` | ≥ 80% | pending | Mock-HTTP + resume + SHA mismatch pending |
| `FileID.App/Services/ReadStore` | ≥ 80% | pending | In-memory SQLite pending |
| `FileID.App/Services/AppSettings` | ≥ 90% | pending | JSON round-trip + migration pending |
| `FileID.App/Services/SafeOpen` | ≥ 90% | pending | Extension whitelist pending |
| `FileID.App/Services/WorkflowAutoTabRouter` | ≥ 90% | pending | Phase → tab pending |
| `FileID.App/ViewModels/EngineClient` | ≥ 70% | pending | State machine pending |
| `FileID.App/ViewModels/{Library,People,Cleanup}` | ≥ 70% | pending | Binding/state pending |
| `FileID.App/Views/*.xaml.cs` | ≥ 40% | pending | UI-thread-bound smoke tests pending |
| `FileID.Theme` | ≥ 70% | pending | `SpringEasing` + `ReducedMotion` tests pending |

## Swift (macOS engine + app)

> Filled in once you run `swift test --enable-code-coverage` and commit the lcov report. The targets below match the engagement plan.

| Module | Target | Status |
|---|---:|:---:|
| `shared/Sources/FileIDShared/*` | ≥ 95% | TBD (existing SharedTests cover ~50%) |
| `engine/Sources/FileIDEngine/Pipeline/*` (non-ML) | ≥ 85% | TBD |
| `engine/Sources/FileIDEngine/Storage/*` | ≥ 90% | TBD |
| `engine/Sources/FileIDEngine/IPC/*` | ≥ 90% | TBD |
| `app/Sources/FileID/Database/*` | ≥ 80% | TBD |
| `app/Sources/FileID/Services/*` | ≥ 75% | TBD |
| `app/Sources/FileID/Views/*` | ≥ 40% | TBD |

## Coverage exempt list (justified, not silent gaps)

| File | Reason |
|---|---|
| `Theme/LavaLampBackground.swift` + `Motion/LavaLampBackground.cs` | Visual; no automated test path; user's favorite touch. |
| Win2D + Metal kernel shaders | GPU-resident, no CPU-runnable assertions. |
| `shell/video.rs` Media Foundation roundtrip | Requires real codec; covered via `iterate.ps1` integration only. |
| `models/{arcface,scrfd,mobileclip,clip_text,vlm}.rs` ORT `Session::builder()` paths | Require shipped DLLs; covered via integration only. |
| `fn main()` + `async fn async_main()` in `engine/src/main.rs` | Covered by the engine-smoke CI job, not by unit tests. |
| `MainWindow.xaml.cs` UI wiring | Mica/Acrylic + drag region — covered by manual smoke. |

## What "status: pending" means

Pending = the test exists in the plan and target is set, but the actual code-coverage measurement hasn't been generated yet because the test code itself hasn't been written. As tests land for each module the table moves to `covered` or `partial` with a percentage.

## CI enforcement

- Phase 8 lands the gate: each PR regenerates coverage, compares against the committed baseline, fails the build if any module drops > 2 percentage points.
- The baseline is whatever's committed in this file at the time of the merge — updating it requires its own PR with the new actuals.
