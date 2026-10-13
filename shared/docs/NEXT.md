## Current checkpoint — Resident owner checkout race resolved; advance Deep scope and Windows offline verification

Iteration20 closed the P1 close/cancel during awaited checkout race in resident service `acquire_with`. Explicit no-post-close-spawn and commit fences ensure that closed or cancelled service state before or during load/proof immediately shuts down the spawned child process, refuses commit to resident cache, and records sticky cleanup failures typed as `HostAdmissionError`. Additive policy-change checks reap active processes if resident policy changes to Disabled during startup.

Windows native canonical path normalization: `runtime_cache.rs` normalized with `crate::util::path_safety::strip_extended_length`, resolving Windows verbatim (`\\?\`) drive comparison mismatch where `canonicalize(&path)? == path` previously failed.

Five new resident owner unit fixtures added in `resident_owner_tests.rs`:
1. `resident_owner_close_during_awaited_proof_denies_load`
2. `resident_owner_cancel_during_awaited_proof_denies_load`
3. `resident_owner_close_during_awaited_load_shuts_down_process_without_commit`
4. `resident_owner_cancel_during_awaited_load_shuts_down_process_without_commit`
5. `resident_owner_policy_change_during_startup_reaps_process_and_refuses`
All 15 resident owner fixtures pass serialized (`--test-threads=1`). Strict engine `cargo clippy --all-targets -- -D warnings` PASS.

Linux GTK app: fixed brittle newline assertion in `tabs/deep_analyze.rs` (line 673). Linux app compilation, `cargo test` (50 passed), and all-target Clippy PASS with 0 warnings.
Windows app: `dotnet test FileID.IpcSchema.Tests` 52/52 pass; `dotnet test FileID.App.Tests` 542/542 pass.
Runtime egress release gate: `check_runtime_egress.py` and `test_check_runtime_egress.py` updated with fresh SHA256 hashes and test file exclusions; both unit tests (24/24) and release gate script PASS with exit 0.

Next: Advance remaining board items:
- Task 10: Deep Analyze scope enforcement integration.
- Task 26: Windows offline verification & design-time resolver.
- Task 34: Safe branch publication per gate requirements.

## Prior checkpoint — Fence awaited resident checkout and close before further native acceptance

Iteration19 integrates an original-compatible resident owner into actual Deep checkout/finalization and main shutdown, not an older candidate. Exclusive owned async guard spans the job; healthy completed CPU-requested policy may retain one server/proof/host residency across jobs. NonCPU jobs retire at terminal until verified accelerator accounting; CPU-requested flags are not proof of provider or working set.120s idle/1s reaper and actual shared-scheduler memory pressure retire only idle entries; close denies initial new checkout, native reaping/sticky failure stays owned. Model proof is reused with admitted source snapshot rechecks, changed weight source refuses and retires before another job.16MiB cancellable/abandonment-aware runtime fingerprint hashes bounded canonical candidate bytes before/after startup, includes SHA256/size/time/UNIX device+inode; runtime change causes awaited old-owner retirement then load. No duplicate internal512MiB/private production tracker. Optional CLI discovery now permits a healthy cached native owner without treating it as permission for inference fallback: separate open/fault-free/not-stopping discovery predicate; R3 fallback still requires confirmed clean exits.

Corrected strict original all-target Clippy PASS17s; only5NEW runtime fingerprint fixtures PASS36s phase/0.02s bodies +10NEW owner/proof/reuse/pressure/idle/closed/waiter/missing actual weights/source fixtures PASS0s phase/0.16s bodies. Two actual complete_with_cancel calls through the SAME cached generated native owner and authenticated loopback; one model proof/load across jobs, not vendor/model inference.13new roots archived inside WSL, oversized sparse fixture remains in original WSL with Windows descriptor.12source roles/before/after/full-role patches/full source/cache/index/locks/schema proof. Missing guessed commands/models.rs read retained before work; first Clippy101/10s suspicious consecutive-if formatting inside select macro stopped before tests, failed entire source kept; exact newline correction in distinct LF driver, no suppression/old/full/review/model/native vendor/GPU/Windows/Apple/acquisition replay. Legacy terminal/provenance source contracts adapted/compiled only.

P1 OPEN discovered during post-validation source disposition: acquire_with checks service closed before lock/preparation, then awaits proof/key/load and commits without a new close/cancel fence. Current Deep shutdown also signals its job and shared tracker later closes, but the generic close-during-checkout/startup race is NOT accepted. Preserve current validated bytes and add bounded delayed-producer/load/close fixtures before correcting; no source change or accepted-test replay this iteration. GPU/provider/DLL/loader/file hierarchy/Windows identity/device-cache replacement/real native model reuse/working set/status IPC/text/stream/independent NEW-source gates remain OPEN. Required reviewer delivery FAILED twice/UNDIAGNOSED, original8350MB/smoke UNKNOWN/core/Apple/generation/Tools/timeline/native/Store/performance/hardware unchanged. Board16/34complete; no app publication/final external command/completion promise.

Next approximately two5/22: named P1 close/cancel during awaited proof/key/startup, requiring a real no-post-close-spawn/commit fence rather than just another pre-call bool; add delayed producer/key/load/native-owner fixtures without real vendor/model work. Then sticky cleanup/unconfirmed ownership and cache failure/terminal/shutdown disposition. Retain shared registry atomic close/spawn, conservative host demands and independent host read lifetime; do not suppress failures or replay accepted fixtures. Status IPC/text/stream and verified provider/accelerator/actual native acceptance remain later gates.

## Prior checkpoint — Actual resident service consumers after verified job and request separation

Iteration18: original runner job owns one canonical registered pinned pair; successful preparation hashes once, alias reuse stays same proof, changed metadata/canonical source/inode or bad pins stick refusal without automatic rehash. Actual CLI caption requires matching prepared paths and freshness after device discovery; Deep/cluster prepare before raster/composite work.16MiB metadata admission ends before model CPU-I-O. Metadata/forged unchanged stamps/Windows identity/post-check native loader and runtime/provider cache invalidation remain unaccepted. Persistent healthy start stores its own stop flag, not startup-job cancellation; explicit public complete_with_cancel observes independent request/stop across turn, admission, host and dispatch. Predispatch cancellation preserves idle native and independently live512MiB reader; dispatched cancellation awaits bounded confirmed reap before return; actual payload and per-file pipeline no longer race/drop outside cleanup. Original R1 terminal/R4 same-Arc/native registry/scope and512MiB work preserved. Corrected strict Clippy12s; only9NEW job fixtures35s phase/0.02s bodies +8NEW request fixtures1s phase/0.29s bodies,15roots archived inside WSL. Own executable/authenticated loopback/FIFO/generated non-model bytes only; no vendor/model/GPU/Windows/Apple/old/full/review/acquisition. Unique legacy-call edit rejection and two pre-test Clippy failures (used underscore guard101/8s; binary dead default wrapper101/15s) preserved; exact rename and cfg(test) helper/public explicit API, no suppression/dummy consumer. Legacy constructor/signature fixtures compiled only.

Next approximately two5/22: integrate a genuine original resident service/cache owner and real reuse/status/idle-pressure consumers without duplicate admission/private tracker or stale stream/text contracts. Cache identity must cover actual runtime/provider/source/CPU policy and verified lifetime; aliases/headers/listings/metadata alone are insufficient. Keep explicit request flags, R1 full-response/native terminal and R4 independently live host lease; service retirement awaits confirmed reap and preserves sticky faults. Native generation/installed working set/accelerator capacity and independent NEW-source acceptance remain separate. Do not reopen generation or infer8350MB diagnosis from these checks.

Board16/34complete. True resident service/native model reuse/cache/active status/idle-pressure/text/stream/shutdown, verified provider/accelerator/other ONNX/media/whisper and independent NEW-source/native acceptance remain open; this is not model residency/inference acceptance. Required reviewer delivery FAILED twice/UNDIAGNOSED; original8350MB/smoke UNKNOWN/core/Apple/generation/Tools/timeline/native/Store/performance/hardware remain. No app publication/final external command/completion promise; guarded drivers NOT final commands.

Additional scope inventory: original src grep finds verify_pair_async only at its declaration, no shipping caller; cluster change is function-level verified-input graft, NOT reachable cluster-generation acceptance. Runner constructor/raw public binary alone does not establish runtime/provider provenance; source metadata/forged unchanged stamps are not immutable loader rights.

## Prior checkpoint — Verified CLI inputs and independent request cancellation prerequisites

Iteration17 actual resident compatibility inventory frozen/dispositioned: candidate4/original16; external384MiB/128MiB work, private tracker, old startup/request cancel coupling, result.is_ok/kill-on-any-error, stream/text/idle/pressure/status/cache identity differ from original internal512MiB/R1–R5/shared owner. No candidate wholesale import or8350MB diagnosis. New original pinned verification worker16MiB/CPU1/I-O1 supports actual VlmServer::start_verified/Deep persistent startup with canonical registry SHA256 pair BEFORE runtime/model admission, no duplicate work.1MiB cancellable chunked hash/type/pin/size/opened/path/UNIX identity checks; verified bytes not active/loaded/capable/license acceptance. CLI/cluster and true resident cache/service remain open.

First-source Clippy15s/only12new generated-byte/worker/cancel/queued/abandoned/type/pin/symlink/replacement/registry/missing verified-start/source fixtures32s phase/0.08s bodies,10roots archived inside WSL. No installed model/vendor/native/GPU/Windows/Apple/old/full/review/acquisition replay. Five-path before/after/patches/full source/cache/index/lock/schema proof; zero-call preparation syntax and initial board timing descriptor retained/corrected only metadata. Evidence verified-resident-prerequisites-20261012, iteration17-checkpoint.json. Post-hash loader/file identity/hierarchy replacement, Windows identity/universal native acceptance, measured working set/verified capacity and GPU99 remain unaccepted.

Next approximately two5/22 slices: close actual CLI/cluster weights provenance without expensive per-caption/per-file rehash; split original persistent startup/stop cancellation from per-request cancellation to support genuine resident service reuse while preserving R1 safe terminals/R4 host work and awaited cleanup. Service/cache/active status/idle-pressure/reuse/shutdown/stream/text/other ONNX/media/whisper and verified provider/accelerator acceptance stay separate. No external384MiB or second internal512MiB lease/private-tracker bypass, stale service imports or result.is_ok-based release. Required reviewer outputs remain FAILED twice/UNDIAGNOSED; no unchanged review retry/harness/model/protocol mutation. Preserve historical8350MB/smoke UNKNOWN, do not replay them automatically.

Board16/34complete; core/Apple/generation/Tools/timeline/native/Store/performance/hardware open; no app publication/final external monitor-rerunnable command, one-shot drivers NOT final acceptance.

## Prior checkpoint — Actual resident/service consumers after owned runtime discovery

Iteration16 named probe/discovery slices accepted: actual async version/device run_probe/shared atomic owner/captured pipes,128MiB/CPU1/I-O1/256KiB-per-pipe/5s execution deadline/confirmed cleanup and same host Arc through returned buffers. Server device discovery BEFORE per-candidate model startup admission, no nested CPU-I-O. Deep optional CLI discovery preserves typed host/control refusal instead of .ok(); clean unsupported listing may select no device, never verified capacity. Narrow original native_runtime worker16MiB/CPU1/I-O1 validates absolute canonical regular/header/host architecture/exec runtime, configured FILEID_LLAMA_CLI/FILEID_LLAMA_SERVER overrides or host-suffix owned bundles; invalid explicit config fails/no PATH search. Only synthetic Linux headers/pure discovery/own test executable; headers never executed and vendor/Windows/Apple/whole-handler/provider/inference unaccepted.

Corrected strict Clippy14s/only15new fixtures32s phase/0.38s bodies,14roots archived in successful WSL invocation; seven-path source/protected/index/schema/lock/cache ledger. Initial compiler101/7s before tests and exact-edit/zero-call script failures retained with narrow timer/import/checked-driver corrections, no suppression/old/full/model/review/acquisition replay. Evidence owned-runtime-probes-20261012 and iteration16-checkpoint.json. GPU99 remains unaccounted;128/16MiB conservative bounds not measured working sets; file identity/cache replacement/universal Mach-O/native acceptance open.

Next approximately two5/22 slices: reconcile current authoritative server/request/CLI/probe API with retained candidate resident-service/cache consumers; integrate actual original resident reuse/lifetime/shutdown without importing stale modules or external384MiB/duplicate internal512MiB work admission. Preserve confirmed-clean versus sticky/unconfirmed owner and handler terminal semantics. Other ONNX/media/whisper/verified provider-capacity/accelerator callers and independent NEW-source/native closure remain separate gates. Do not retry unchanged failed reviewer delivery/mutate harness/model/protocol or repeat historical8350MB/smoke/native suites. Required outputs FAILED twice/UNDIAGNOSED, existing recovered advisory R1–R5 source closure not NEW source acceptance.

Board16/34complete; core/Apple/generation/timeline/Tools/native/accessibility/Store/performance/hardware and historical UNKNOWN open; no application publication/final external monitor-rerunnable command, guarded drivers NOT final acceptance.

## Prior checkpoint — Own bounded native probes before model admission

Iteration15 actual original CLI caption now has typed model refusal, one model-estimate+512MiB/CPU1/I-O1 admission after legacy device discovery, shared atomic captured-pipe native owner, bounded1MiB stdout/256KiB stderr, retained identical host/native/drain lease and awaited cleanup/cancel/error propagation. Dropped helper requests native stop/aborts asynchronous reader; native registry retains through confirmed exit. First-source strict Clippy12s +only10new production-helper/own-executable/preflight/source fixtures30s phase/0.09s bodies pass,9roots archived within WSL. No vendor/model/GPU/Windows/Apple/old/full/review replay. Protected source/index/locks/schema retained; four-path cli-owned-caption-20261012 ledger. Guessed vlm_native.rs read ENOENT preserved; original vlm.rs verified.

Next approximately two5/22 slices: replace actual version/device probes with bounded shared-owned/captured/admitted workers; move device probe BEFORE persistent model startup lease to prevent nested CPU-I-O. CLI already reserves after probe. Keep typed admission/cleanup failures from becoming fallback permission; require confirmed probe cleanup and retain host buffers to actual end. Then reconcile original runtime env/absolute host-compatible validator and resident-service contract against existing RequestWork/internal512MiB, not wholesale candidate resident/native_runtime (incompatible server/external384MiB/sync ignored cleanup). Shared capacity/provider/device mapping must be verified, not a parseable listing relabeled inference or guessed GPU budget.

NEW CLI source/native/whole-engine/inference review remains open. Prior R1–R5 recovered advisory source closure still bounded; required artifact delivery FAILED twice/UNDIAGNOSED, no unchanged reviewer retry/model/protocol/harness workaround. Legacy GPU99 unaccounted, actual resident/cache/service/other callers/ONNX/media/whisper/native/provider/core/Apple/generation/Store/performance/hardware and original8350MB/smoke UNKNOWN open. Board16/34complete; no app publication/final fresh-shell monitor-rerunnable command, guarded drivers NOT final acceptance.

## Prior checkpoint — Integrate authoritative resident/native service consumers; retain failed review delivery

Iteration14 R5 new terminal/helper/owned mock/source seams accepted (Clippy13s,8new cases28s phase/0.02s bodies,7roots archived). Fresh independent corrected-source reviewer ab470666-4aeb-4049-9a03-880826c4aa5a: all R1–R5 SOURCE RESOLVED, No issues found, OK with notes. Required file-only reviewer-report.json MISSING: run/mission FAILED, runner exit0/attestation/structured response not successful delivery. Complete report recovered separately under corrected-ownership-review-20261012/parent-consumed; parent accepts advisory source closure, not delivery/runtime/release. Both historical required report files remain absent, no re-review/relaunch/substitution/manufacture.

Nineteen frozen/live source/index pins unchanged, mutation attempted/observed false. Handler delta basename collision/empty artifact and initial failed R1/R4 Clippy receipt retained; direct-current-handler review plus distinct parent-only delta and existing corrected Clippy12s receipt saved, no source or validation/review replay. Management key/root lookup failures preserved; exact API-key/CWD corrections closed failed mission. Required output routing root cause remains UNDIAGNOSED; do not launch new reviewers or change harness/model/protocol to disguise it without an authorized diagnosis.

Next approximately two source slices5/22: inspect current original resident/native-service/CLI lifecycle/resource seams against retained reviewed candidates, then integrate narrowly with genuine ownership/admission callers. Preserve R1–R5/scoped planning/root/raster/empty/index/schema/locks and failure charges; no nested CPU-I-O, guessed GPU budgets, older whole handler/module/DTO imports, automatic historical model/8350MB/accepted-suite reruns. Actual provider/capacity/service/cache/other callers/native/GPU acceptance remains separate. Core relocation/genuine Apple/generation/Tools/timeline/native/Store/performance/hardware and original8350MB/smoke UNKNOWN still open. Board16/34complete; no app publication/final monitor-rerunnable external acceptance command; guarded drivers NOT final commands.

## Prior checkpoint — Independently review corrected R1–R5 ownership delta

Iteration14 R5 named source/mock seams accepted: production finalizer waits retained-server bounded shutdown before success/cancel/scope/admission terminal and handler return; existing probe/startup/fallback error cleanup remains. Cleanup failure emits owned error with original cause, unchanged processed/failed/cancelled/model counts (no fabricated failed file), retains sticky/unconfirmed residency; total_seconds includes cleanup. Main single-flight guard still spans awaited handler, verified source-only plus actual finalizer future-return ordering, NOT real whole-handler/native gate execution.

Strict original all-target Clippy13s +8new production terminal-helper/OwnedProcess GateChild mock/source fixtures28s phase passed first-source; 7new roots archived before WSL exit. No new failure/acquisition/old/full/native-smoke/model/GPU/Windows/Apple/review replay. Preserve batch-terminal-cleanup-20261012 three-path source/patches/cache/bindings/commands/results/state and prior R1–R4 protections; index/schema/locks/main/scheduler/scope/planning/raster unchanged.

Next: fresh independent corrected-delta R1–R5 review over frozen current original bytes/immediate consumers, same retained builtin reviewer/model/native/file-only protocol, explicit structured-output binding. Previous failed reviewer required artifact stays absent/failed; advisory recovery not relabeled acceptance. Consume actual new delivery/result and disposition before ralph_done. Native/inference/GPU/provider/service/other callers/core/Apple/generation/hardware/Store/performance and original8350MB/smoke UNKNOWN remain; board16/34complete, no app publication/final external monitor-rerunnable command.

## Prior checkpoint — Finalize retained server before batch terminal and gate release

Iteration13 R2/R3 corrected original consumers and new named seams accepted: typed/contextual HostAdmissionError from actual midbatch payload probe becomes owned failure after retained server cleanup, not CLI selection. ProcessTracker::allows_fallback includes sticky failed-reaped and closed/unconfirmed state; actual candidate retry/startup classifier/initial payload/midbatch handoff use it. Atomic spawn/close, ordinary clean retirement and sticky fault reporting unchanged; old has_unreaped test-only. Strict Clippy13s +4new actual probe cases28s compile/0.08s test +5new mock/consumer cases0s phase/0.01s test pass, four roots archived inside WSL. No old/full/model/GPU/vendor/native-smoke/Windows/Apple/review replay or new failures/acquisitions.

Next R5: every post-start success/cancel/scope/admission/error return must finalize retained server before emitting terminal and releasing single-flight batch guard; preserve processed/failed/cancelled semantics and emit cleanup failure instead of falsely clean completion. Add new delayed-confirmed-reap/failure/event-order/single-flight tests on actual finalizer/handler seams, not vendor/model/full suites. Then independent corrected-delta review over current frozen original authority, keeping earlier missing required output/run failure intact, no unchanged-source review rerun/manufactured artifact/alternate model/CLI.

Preserve probe-fallback-guards-20261012 six-path source/patches/cache/bindings/results/state; request-terminal-lifetime-20261012 R1/R4 dual host/native/native-terminal closure and prior failure records. Scope/planning/raster/main/scheduler/locks/schema/index unchanged by this slice; whole post-start/native inference not yet exercised. GPU99/resource service/other callers/core/Apple/generation/native/hardware/Store and original8350MB/smoke UNKNOWN remain. Board16/34complete, no app publication/final externally rerunnable command; guarded drivers are not final commands.

## Prior checkpoint — Preserve typed probe refusal and sticky cleanup fallback eligibility

Iteration12 R1/R4 original request seam corrections have strict Clippy+8new generated complete/child/HTTP/FIFO fixture acceptance only; independent corrected-source review is still pending. RequestWork now distinguishes predispatch/full bounded response terminal from unconfirmed dispatch, so safe local/full400/interpretation errors preserve healthy reuse while truncation/transport/capped/abandoned dispatched work stops/retains native charge to reap. Same Arc independently owned by host guard and native owner; admitted blocking image closure owns it through real read end, avoiding premature host RAM/CPU-I-O retirement after native exit/caller abandonment. No second demand charge. Existing old dispatched fixture only amended/compiled, not replayed.

Next approximately two corrections: R2 actual midbatch probe must retain HostAdmissionError and refuse CLI fallback on admission rejection; R3 failed-but-reaped sticky cleanup must deny subsequent candidate/startup/CLI fallback just like OwnedProcess::spawn. Add exact new refusal/no-fallback/failed-reaped fixtures, not old/full suites. Then R5 finalize retained server before every post-start success/cancel/scope/error terminal and single-flight return, report bounded cleanup failures; acquire independent corrected-delta review without replaying the failed unchanged-source run or manufacturing its missing artifact.

Preserve request-terminal-lifetime-20261012 four-path source/patch/cache/binding/failure/validation/state evidence: Clippy12s,8newcases26s compile+0.11s test,8roots archived inside WSL. Initial unused_async wrapper failure and doubled lock-binding preflight failure retained; exact source/path corrections used installed Rust1.90/same cache/protocol, no completed check replay. Controlled FIFO durable WSL original plus Windows metadata archive, not private IO/model/native acceptance. Index/schema/locks/main/scheduler/native owner/Deep scope/planning/raster unchanged. GPU/service/other callers/core/Apple/generation/hardware/Store/native and original8350MB/smoke UNKNOWN remain open; board16/34complete, no app publication/final external command.

## Prior checkpoint — Fix request-terminal and dual-lifetime blockers from failed-delivery review

Iteration11 preflight typing source+4new fixtures accepted; original independent reviewer d26358b9-4979-4f87-acbe-96ac00c61317 FAILED required file-only delivery. Full supported report recovered from status.steps[0].acceptance.childReport into separate parent-consumed JSON, failure preserved and original expected file remains absent. No re-review/relaunch/alternate CLI/model or accepted check replay. Fifteen frozen/live bindings unchanged; parent disposition five P1 VALID/BLOCK, not passed review/native acceptance.

Next approximately two coupled request seam corrections: R1 distinguish safe pre-dispatch or confirmed received terminal errors from unconfirmed/abandoned dispatch, preserve healthy server reuse; R4 keep lease Arc in host guard as well as native owner, so child exit cannot uncharge active host read/encode/HTTP/parse. New missing-file/full400/malformed-response/next-success and controlled native-exit-during-blocked-HTTP fixtures required; retain abandoned dispatched work to confirmed native reap and avoid premature host I-O retirement. No unconfirmed/sticky failure erased to make tests pass.

Then R2 preserve/classify midbatch health probe HostAdmissionError before CLI fallback; R3 candidate/startup eligibility must include sticky failed-reaped owner; R5 finalize retained server before every post-start success/cancel/scope/error terminal and single-flight return, report bounded cleanup failures. Correct/verify new deltas independently; do not infer failed-delivery report as approval. Source, index/locks/schema/scope/planning/root/raster/empty behavior remain protected. Legacy GPU99 unaccounted, actual service/native/core/generation/Apple/hardware/Store and historical8350MB/smoke UNKNOWN still open. Evidence original-host-owner-review-20261012/parent-consumed and server-preflight-typing-20261012; board16/34complete, no app publication/final external command.

## Prior checkpoint — Consume original ownership review; fix proven healthy-server regression

Iteration11 raw headroom preflight now typed before reserve_host; actual Deep startup fallback classifier refuses contextual host failures while absent-runtime fallback remains separate. Strict original Clippy+only4new actual preflight/type/classifier fixtures PASS; three roots archived inside WSL. CRLF generated-driver header exit2 preserved before workloads, separate LF copy same protocol/toolchain/cache/checks accepted. No old/model/GPU/HTTP/native execution.

Next22: consume in-flight read-only reviewer d26358b9-4979-4f87-acbe-96ac00c61317, frozen original-host-owner-review-20261012 twelve+three immediate consumer sources, classify findings. Already VALID/BLOCK healthy-server stop from pre-dispatch read errors or fully received400s: RequestWork only completes on successful result, contradicting keep-healthy/reprobe handler. Correct narrowly with new ownership/terminal distinction and generated regression evidence; preserve unconfirmed/dispatched abandonment charges. No rerun of accepted old suites, no source/runtime acceptance inferred from ongoing review. Supervisor request answered; pending channel empty, no steering/relaunch/alternate CLI needed.

Next5: real accelerator capacity/device/provider provenance and resident cache/service/other native/ONNX callers still absent; legacy GPU99 remains unaccounted/unaccepted. Preserve scoped/admitted planning/empty/root/raster/schema/index/locks and original8350MB/smoke UNKNOWN. Evidence server-preflight-typing-20261012 and pending review record. Board16/34complete; no application publication or final externally rerunnable acceptance command. Finish iteration11 review/disposition before ralph_done.

## Prior checkpoint — Review real host/request ownership; add verified native service accounting

Iteration10 original server now owns host estimate admission through actual native process and sheds startup CPU-I-O at health, not memory. Actual complete() has serialized turn,512MiB/CPU1/I-O1 work admission and stop-on-abandon/error guard retaining charges until confirmed reap. Queued abandonment removes waiter without native dispatch; duplicate/stopping/reaped/faulted work cannot replace charge. Typed admission rejection is not CLI fallback; fallbacks require confirmed native cleanup. First-source original Clippy+9new generated own-child/loopback/mock-reap fixtures PASS; five roots archived inside initial WSL invocation. No old/full/model/GPU/Windows/Apple checks replayed; prior owner assertion updated/compiled only.

Next22/5: independent integrated source review of original host/native/server/Deep fallback/main seams; additionally close raw require_model_headroom preflight error typing (currently generic before reserve_host, so the typed reserve rejection gate does not cover that earlier refusal) and verify narrow new regressions, not prior suites; preserve atomic close/spawn, ordinary compaction, sticky failures and charges through unconfirmed exit. Real GPU capacity/provider/device mapping and actual accelerator charge remain absent (legacy offload99 unchanged); HOST-only tests are not GPU/model acceptance. Graft resident cache/service/verified runtime and remaining CLI/probe/media/whisper/ONNX consumers without nested CPU-I-O. Snapshot/pressure still test-only; work APIs now real consumers. Diagnose original8350MB independently, not by declaring new fixtures equivalent. Actual native Windows/vendor/engine shutdown/core/Apple/hardware/generation/performance/Store remain open.

Evidence iteration10-checkpoint/server-host-admission-20261012 exact backups/after/patches/source/cache/Clippy/new fixtures/retained state; durable WSL same-name state and identical original target. Original index/schema/manifests/locks/scheduler/main protected, original six-field scope/admitted planning/empty/root/raster preserved. Board16/34complete; no application publication or final external rerunnable acceptance command. Do not speculative-rerun formatter26 or accepted GTK/native/full checks;32/34 remain pending.

## Prior checkpoint — Review integrated owner and graft actual residency/admission

Iteration9 minimal original native_process owner now used by persistent server, with immediate post-spawn ownership, ordinary confirmed-clean retirement, sticky fault/unconfirmed ownership, fail-closed new spawning and atomic closed registration barrier. Main aborts dispatch/cancels Deep/closes and awaits tracker before sink teardown, reports failed native cleanup and returns error. Corrected original all-target Clippy and8new generated-child/fault/source-seam fixtures PASS. Seven roots archived before leaving WSL. Preparation CRLF/unchecked-offset truncation and initial needless-update Clippy failure preserved/corrected without suppression, no old/full/model/GPU/Windows/Apple/whole-engine replay.

Next22: independent integrated source review plus actual Windows/engine/vendor lifecycle gates; new Linux own-executable children are not real model or8350MB acceptance. Preserve registry barrier/ordinary compaction/sticky fault resources.

Next5: server still passes None lease; graft real resident service/startup/request CPU-I-O/RAM/GPU admission with actual capacity provenance, CLI/probe/media/whisper callers and shutdown. Do not inherit planning/global CPU-I-O through nested native-service reserve. Enable test-only diagnostics/work APIs only with real consumers. Preserve original six-field Deep scope/admitted planning/raster/empty/terminal behavior and server privacy/auth/offload policy; no wholesale older handlers/modules/DTO imports. Whole real-engine teardown remains source-only; original8350MB UNKNOWN.

Evidence iteration9-checkpoint.json/native-owner-integration-20261012 exact backups/failed source/checked assembly/after/patches/bindings/cache/new Clippy+tests/state; durable WSL native-owner-integration-20261012/state and original same-lock target retained. Original scope/scheduler/schema/manifests/locks/index protected, board16/34complete, no app publication/final external rerunnable acceptance command. Windows formatter26/GTK32/Store34/core/Apple/generation/media/performance/native gates remain.

## Prior checkpoint — Integrate actual resident/native consumers and preserve admitted scope

Iteration8 actual original scope planning now admitted128MiB/CPU1/I-O1, blocking/paged/cancellable, and released before model handoff; explicit-empty stays immediate/no queue/no loader, confirmed planning cancellation terminal only. Initial maximum-ID keyset pagination256rows, bounded output/path allocations, existing scope/privacy/raster guards retained. Additive original scheduler accelerator/queued APIs preserve legacy FIFO/priority/background behavior and unknown GPU/NPU capacity. Snapshot/pressure diagnostics cfg(test), not production monitoring.

Strict all-target original-core Clippy and only8new scope/5new resource fixtures PASS. Initial unused diagnostics/full-lock Android cache-verifier failures preserved/corrected without suppression/download/toolchain/lock change. Old7handler/standalone/full/candidate tests not replayed. Two generated513-row roots archived inside the initial WSL invocation, evidence scope-admission-prerequisites-20261012 and iteration8-checkpoint.json. Source/manifest/schema/lock/index protected; identical original target reused, no mixed GTK artifacts.

Next5/22: minimal reviewed resident/native_process/native_runtime consumers/startup/pipeline/shutdown graft, real GPU-capacity provenance and queued fairness; preserve ordinary confirmed-clean tracker retirement and failed/unconfirmed ownership. Enable snapshot/pressure production API only with actual consumers. Do not import older Deep handler/module/DTO tables or inherit shared CPU-I/O admission across nested service reserve. Original8350MB UNKNOWN and native process/provider acceptance still open.

Next10: integrated independent review/native Windows/file-handle/hierarchy replacement/cancellation boundaries; per-file and raster filesystem operations still legacy synchronous even though target planning is admitted. Actual inference/generation remains unaccepted/closed. Board16/34complete; no application publication or final external acceptance command. Do not replay completed checks or speculative formatter;26/32/34 and remaining core/Apple/media/performance/native gates remain.

## Prior checkpoint — Admit scope planning and graft runtime without losing new guards

Iteration7: actual original DeepScope now registered/enforced through DeepAnalyzeAll selection, explicit-empty no-loader terminal, whole exclusions/current roots, per-file check and CLI/server rasterization with resolved allowed source. Strict original-core all-target Clippy and seven new generated handler/SQLite/terminal/root/raster fixtures PASS. Initial fixture-only lint/exact-edit/preparation failures preserved. No schema/DTO/lock changes, capability opening, old/full suite or actual decode/inference/Windows acceptance.

Next10: move synchronous filesystem/SQLite scope planning to bounded cancellable admitted blocking work; do not hold that CPU-I-O admission through resident/native model admission. Add new phase/cancellation/no-nested-admission fixtures without replaying accepted suites. Retain explicit empty and terminal ownership, selected bypass/exclusion/current-root/raster guards. File-handle/hierarchy-replacement snapshot and native Windows/independent gates remain unaccepted.

Next5/22: minimal runtime prerequisites/service/Lease/Tracker and shutdown graft against actual original authority. Closure review froze twelve source pairs; original resident/native_process/native_runtime files are absent and startup/pipeline signatures differ. Candidate older Deep handler drops IDs/exclusions/new guards; never wholesale copy it or an older module/DTO table. Retain failed/unconfirmed cleanup evidence and ordinary tracker retirement. Original8350MB UNKNOWN; no runtime/shipping/native acceptance inferred from candidate tests.

Preserve iteration7-checkpoint.json, deep-scope-handler-20261012, runtime-integration-closure-20261012 and durable WSL deep-scope-handler-20261012 target/state (seven generated roots retained after invocation). Original index/schema/locks unchanged; no application publication, board16/34complete. Windows formatter26, native32/Apple/core/generation/media/performance/Store and exact final external acceptance command remain open; no speculative formatter or completed-suite replay.

## Prior checkpoint — Integrate validated tracker candidate and enforce actual Deep scope

Iteration6: exclusive runtime candidate ordinary confirmed-clean registration retirement repaired and validated with actual scheduler.314lockedarchiveSHA256 checks, strict all-target Clippy and three new generated child contracts PASS. Initial prerelease-filename verifier failure preserved/corrected without acquisition retry; no version mixing with GTK lock. Shipping original has no native_process.rs; candidate validation is not shipping integration or native runtime acceptance. Original8350MB UNKNOWN remains.

Next5/22: bounded runtime/module/admission integration against actual original schema/backend authority with exact backups; preserve failed/unconfirmed ownership and new ordinary retirement contract. Next10: register/wire DeepScope current-root/per-file/handler/admission semantics without opening unaccepted generation. Runtime integration/native independent review, real generated Chat/timeline/speech/media, authoritative shared Rust core/Apple hosting and hardware/performance/Store/final externally rerunnable gates remain.

26: all247savedXAMLreferences exist; missing files do not explain design-time XamlCompiler.exe exit1/66CS0246. Continue only with evidence-backed projection/design-time resolution, not speculative reruns/SDK substitution/diagnostic suppression or source masking. No successful compiler/contract/IPC/full-suite replay.32: corrected native Deep presentation fixtures accepted; independent integrated/broader native closure remains. Archive future generated state inside its original WSL invocation.

Preserve `iteration6-checkpoint.json`, `tracker-retirement-20261012` and durable WSL caches/target/state. Original Git index/locks unchanged; no application code published, board16/34complete. Continue bounded engineering without routine prompts under existing privacy/license/dependency/publication limits.

## Prior checkpoint — Terminal presentation fixed; finish formatter and integrated native closure

Iteration5:26 fixture encoding/logical-text closure and cache-only IPC assets rebinding passed; first incompatible global-RID restore failure retained. Format has zero encoding/line-ending/whitespace findings but exits2 on App design-time XamlCompiler.exe exit1/66CS0246. Formatter-only per-project RID alignment did not resolve. Next inspect saved XAML input/output and reference graph against existing compiled artifacts; do not suppress diagnostics, edit source to mask design-time loading, substitute toolchains or replay successful compiler/contract/IPC/full suites.

32: first dedicated native Deep fixture exposed idle capability messaging overwriting terminal transport status. Failed binary/source/log and state hashes preserved; subsequent archival found temporary directories absent (cause UNKNOWN). Corrected DeepRun terminal ownership and idle presentation guard; begin resets ownership, capability gates closed. Corrected-source strict Clippy, one new focused unit and both exact generated-state native Deep fixtures PASS. Empty generated settings unchanged during tests; logs/hashes retained, not expired /tmp directories. Archive future fixture state before exiting its WSL invocation; do not replay accepted tests to recreate evidence. Next integrated independent source review and broader dedicated native closure; no old/unexplained smoke replay or inference/native performance acceptance by assumption.

Original Git index/locks unchanged; no application publication. Preserve `iteration5-checkpoint.json`, `windows-remaining-close-20261012`, `gtk-terminal-status-fix-20261012` and WSL prior binary/generated fixture roots. Board16/34complete. Continue bounded engineering without routine prompts; existing privacy/license/dependency/publication limits remain. Resource-tracker retirement, Deep backend/admission, generation/media/core/Apple/native/Store/performance and exact final externally rerunnable command remain outstanding.

## Prior checkpoint — Focused validation recovered; finish format and native fixture closure

Iteration4: isolated official locked Windows restore and34canonicalNuGetcontenthashes PASS; changed fixture compile0warnings/errors and exactly one previously failing Chat contract PASS1/1. Raw ZIP hash mistake preserved/corrected with explicit authority; no completed IPC52/full app-suite replay or UNKNOWN relabeling. Two policy file BOM/CRLF-only corrections proved identical logical text. Format/diagnostic exit2 retained: remaining NativeWorkbenchCompletionContractTests.cs encoding and IPC test assets using missing old package cache. Next26: back up/normalize only reported fixture encoding, cache-only locked IPC asset rebinding into verified isolated cache, then new source-bound format verification. Do not disable diagnostics or edit workspace CS0246 source references to mask loading problems; do not replay successful compiler/contract/IPC suites.

32: same installed Rust1.90/WSL; path-conversion launch failure preserved/corrected with explicit authority. Official locked fetch,324archivehashchecks, strict all-target GTK Clippy and7selectednewunitfixtures PASS. Original Rust/source locks unchanged. Native/ignored Deep fixtures and independent integrated-source review remain unaccepted. Next use dedicated generated-state fixtures; no old/unexplained second smoke or full-suite replay, inference/model/provider/performance acceptance by assumption. Preserve WSL `gtk-locked-recovery-20261012/{cargo-home,target,state}` and exact command/source/hash receipts.

Owner requests continuing with engineering judgment without routine prompts until done. Existing dependency/toolchain/privacy/license/publication policies remain. Original Git index unchanged; no application code published. Finish34 projected/source/native/format closure before scoped publication; prior doc commit CI remains NOT RUN. Artifact roots `windows-locked-recovery-20261012`, `gtk-locked-recovery-20261012`; aggregate `iteration4-checkpoint.json`. Board16/34complete; core/Apple/generation/resource/runtime8350MB/performance/final externally rerunnable command remain outstanding.

## Prior checkpoint — Prepare newly authorized locked build-package recovery

Iteration3 reflection recorded; board16/34complete. Historical WSL target missing (cause UNKNOWN) and authorized isolated offline seed stopped on271missingarchive/indexentries before copy/compiler. Windows earlier locked offline restore remains failed NU1101/NU1004; no automatic retry. Owner now explicitly permits missing existing locked build dependencies from official NuGet/crates.io sources with generated isolated caches, unchanged versions/locks and verified hashes. Existing installed toolchain only; no separate model/CUDA/vendor inference runtimes, private config, new dependencies or toolchain acquisition. No execution under this new authority yet. Evidence `locked-build-package-authorization-20261012.json`.

Next26/32: source/lock-bound one-shot official-feed package recovery, .NET invocation RID alignment with existing locks and GTK installed Rust1.90. Stop/report fresh failures. Then changed Windows fixture compilation, only previously failing Chat ownership contract and pending format; GTK changed-source compilation/lint and appropriate new isolated fixtures. No completed IPC52/full-suite/old-smoke replay. Preserve UNKNOWN app-suite/runtime8350MB/second-smoke dispositions.

34: twelve startup/Store/theme/icon snapshots frozen; ten historical protected bindings match. Complete tracked startup diff read, XML/Store version/icon dimension checks passed, NOT full closure/projected build/native/Store/CI acceptance. MainWindow.xaml includes separate workbench changes: no wholesale startup staging. No app source published this iteration; only prior documentation commit remains pushed with CI NOT RUN. Finish validated closure before scoped source publication; original dirty work/index remain protected. Reflection and artifacts `store-startup-review-20261012/{source-manifest,bounded-source-review}.json`/`gtk-minimum-overlay-20261012/locked-cache-seed-result.json`. No final external acceptance command yet.

## Prior checkpoint — Documentation pushed; GTK minimum Deep source integration awaits verification

Ralph iteration2:16/34complete;32/34remain in progress. Isolated documentation-only branch `codex/fileid-completion-progress-20261012` pushed commit `a29f715575a9da61641464a9e54ac90efdbaec4d`. New checkpoint doc reviewed/current-doc contract/whitespace passed; original index byte hash unchanged, dirty application/history not staged. GitHub CI query returned no runs: NOT RUN, not green. Continue34 reviewing/validating existing Store/startup/icon/parity closure before scoped source publication. No main/release push or auto merge.

32: minimum Deep overlay applied to original using hash-bound reviewed candidate, nested status helper, main module wiring and thin closed-capability bridge (not unrelated runtime-config/Settings/whole client or older DTO). Original schema1.8 six-field contract retained. Exclusive generated-state requirement now guards both ignored Deep fixtures; original native fixture matches new unavailable view and closed controls and reaches generated-event regression helper after readiness. New modules rustfmt/launcher bash syntax/source whitespace passed, NOT compilation/native acceptance. Review new integration delta and compile/lint suitable source; validate isolated new fixtures without replaying unexplained second smoke. Evidence `gtk-minimum-overlay-20261012/integration-result.json` plus exact snapshots.

26 remains paused: NETSDK1004missingassets then owner-approved single locked offline restore failed NU1101cache/NU1004RID; no dependency drift/downloads/retry. Determine bounded authorized cache/RID recovery, preserve historical build/IPC52 and app-suite UNKNOWN. Deep backend/admission, runtime8350MB/tracker, generation/media/core/Apple/native/performance gates and exact final externally rerunnable command remain outstanding.

## Prior checkpoint — Continuation/publication authorized; offline cache recovery blocked

Owner now authorizes completing all remaining work and incremental GitHub publication, including existing branch work after review/validation. No commit/push yet. These instructions supersede historical phase prohibitions below, not dependency/download/privacy policies. Board16/34complete;26paused and32source closure in progress.

26: same installed PowerShell executable works by absolute shell invocation, but changed-fixture build failed NETSDK1004 before compilation. Prior generated assets/output are missing, cause UNKNOWN. Explicitly authorized one locked offline cache-only restore failed NU1101missingpackages and NU1004Theme/IPC RID mismatch; no network feeds/downloads/version changes or retry. Four dependency locks/central versions unchanged. Further recovery must resolve both cache and invocation/RID without silently disabling locks or restoring over network. Preserve historical production build/IPC52 and full app-suite UNKNOWN; changed fixture and format remain unrun. Do not rerun completed IPC52.

32: original-compatible test readiness adapter and exact reviewed timelineSample job-controls guard authored with three-file backups. Lifecycle and six-state unit fixtures unrun; native fixture now denies unsupported timeline controls while retaining synthetic catalogIndex cancellation persistence coverage. Only source whitespace verified. Review/compile new GTK delta, complete minimum Deep overlay against original six-field schema1.8 DTO, and validate appropriate isolated fixtures; do not rerun unexplained second smoke or import whole older candidate EngineClient.

34: review/validate Store/startup/icon/parity dependency closure, preserve dirty original/index, publish scoped progress on an isolated branch and check CI. No force push/main merge, private/generated artifacts, or unreviewed whole-file staging. Runtime8350MB UNKNOWN, tracker/Deep admission, real generation/media/core relocation/Apple hosting and native/performance acceptance remain open. Final monitor-rerunnable acceptance command not yet established; do not emit completion.

## Prior checkpoint — Chat fixture/wrapper source correction complete; execution gated

Todo refresh:16/33complete,17remaining,0active. Immediate priority26: finish authorized Windows verification scope only after explicit execution/lifetime-evidence approval; corrected fixture remains uncompiled/unrun and format pending. Task25 is completed Windows Chat-only integration, not whole-platform parity. Broader GTK/Apple/core/runtime/media/performance work remains paused or blocked; no new execution authority from status/todo update.

Explicit source-only correction complete (Task33): backed-up fixture now scopes ChatAsync before the separated non-send helper and retains strict ownership assertion; scratch wrapper captures native exit status without stderr terminating result delivery. Other16 integrated paths unchanged. No production changes/build/test/format/process inspection/intervention. Corrected fixture uncompiled/unrun; wrapper unexecuted. Task26still incomplete; prior clean build/IPC52 receipt retained, final app suite result and owned test-process termination UNKNOWN.

Report this scoped completion before additional work. Further execution/owned-test lifetime evidence needs explicit authorization; do not rerun IPC52 or infer whole-app acceptance from source fixes. Preserve original failed commands/statuses and two exact byte backups/deltas at `windows-chat-integration-20261011/fixture-wrapper-backups`. GTK/WSL/Mac/native/model/runtime/acquisition/publication scope remains prohibited; original8350MB UNKNOWN/BLOCK/trackerP2 unchanged.

## Prior checkpoint — Windows compiles; Chat fixture/app test delivery blocked

Explicit invocation correction completed: offline build PASS25.36s/0warnings/errors; IPC52passed withTRX. App sourcecontract `DispatchAndChatSendObserversSurviveOnlyUiAwaitCancellation` FAIL observed. Its ChatAsync→CatalogSearchAsync text slice includes the separated non-send history helper; narrow method-boundary fixture correction required, no production guard weakening. PowerShell native stderr/Stop aborted result metadata; final app suite receipt/summary and owned test-process termination UNKNOWN. Stop/no format/test rerun/source correction yet. Task26incomplete;17 integrated Windows Chat paths/backups retained.

Ask for narrowly scoped fixture/harness correction, and explicit remaining execution/owned-lifetime evidence if requested. Do not rerun completed IPC52, assume app-run termination, weaken send ownership, alter production behavior, or expand to GTK/WSL/Mac/native/model/runtime/acquisition/publication. Preserve all earlier failed commands/statuses and source bindings; GTK/runtime8350MB UNKNOWN/P2 remain. Evidence `windows-chat-integration-20261011/app-test-stop.json`.

## Prior checkpoint — Windows Chat integrated; offline verification stopped

Metadata recovery explicitly approved and completed once. Backed-up17 Windows Chat-only compatible paths applied; integrated source bindings passed,586 protected sources and Git index unchanged at binding. Original Tools/Catalog/IPC/backend/dependencies preserved; only reviewed Chat test subsets adopted. Task25complete for Windows-only integration, no native acceptance.

First offline solution build failed before compilation (`NETSDK1134`, solution-level RuntimeIdentifier unsupported, exit1/0.50s). Task26stopped; tests/format unrun. Ask for one invocation correction removing solution RID, then continue pending cached offline .NET checks under the same generated isolated state. No source fix, restore, completed-review rerun, GTK/WSL/Mac/native/model/runtime/acquisition/publication authority. Preserve original failure/log; stop new failures. Exact artifacts/backups at `windows-chat-integration-20261011`. GTKtestoverlay and original8350MB UNKNOWN/BLOCK/runtimeP2 remain unchanged.

## Prior checkpoint — Windows integration preparation stopped

Owner authorized Windows-only backed-up Chat-compatible integration and cached offline .NET verification. Before backups/apply, a truncated read-tool manifest result caused JSON parsing failure at position51160. No retry/source edits/build/tests/format; failure retained under `windows-chat-integration-20261011/preparation-stopped.json`. Task25 stopped,26 not started. Ask for bounded metadata recovery (parse the existing manifest without tool-output truncation), then continuation of the same Windows-only scope; stop on actual source conflicts/unreviewed dependencies or new execution failures. Do not expand to Tools/Catalog/IPC/backend/GTK/Apple/native/model/runtime work. Prior source candidate verdicts and GTK/runtime blockers remain unchanged.

## Prior checkpoint — NEWsource candidates OK; choose next scope explicitly

Bothfullcheckedsource reports consumed. Windows6NEWChatpaths sourceOK(noissues); GTK5NEWproductionpaths sourceOK(noissues), but minDeep testoverlay/broaderWorkbenches integrationBLOCK. Task32unapproved: originalTEST-ONLY publish readinessadapter missing; originalnativefixture timelineCancel expectation mustmatch unavailablebackend(no reopeningguards); unchangedcandidate EngineClient4fieldfixture excluded fromminimal6fieldoriginal target. Tasks28/29/30closed ONLYboundedsourcecandidate disposition, originalfailedworkers unchanged. No originalintegration/sourceapply/build/test/native/model/WSL/inference/acquisition. Tasks25stopped/26notstarted. Explicitnextchoice required: narrowWindowsonlyChatcompatiblehunkintegration/offlinecachedverification OR source-onlyGTKtestclosurecorrection/review first; do not importunreviewedTools/Catalog/IPC/wholeEngineClient. Original8350MB UNKNOWN/BLOCK/trackerP2deferred/incidents unchanged. Reports/disposition `parent-metadata-recovery/parent-consumed`.

## Prior action — owner-approved NEW-delta source reviews after metadata recovery

Owner **Recover metadata and review**. Parentmetadataonly assemblyPASS:6Windowsafterhash/diff/protectedpostchecks,216originalfrontend snapshotspinned; GTKshared109checks retainednotrerun, knownunownedtest-readiness/timelinefixture conflicts recordedunresolved. Task31complete,29inprogress, validated source-onlyWindows/GTKfreshreviews awaitinglaunch atrecord. Consume fullsaved reports/status and parentdispose sourceboundNEWdeltas, no oldinspection/testreruns. Sourcechanges/integration/build/native/model/WSL/inference/acquisitionstillprohibited; alloldfailedrunstates unchanged. Task28/30sourceacceptance incomplete,25stopped/26notstarted, schema1.8preserved; runtime8350MB UNKNOWN/BLOCK/trackerP2deferred. Evidence `prerequisite-fix1/parent-metadata-recovery`.

## Prior blocker — both sourcepartial; metadata consolidation needs approval

GTKonecontinuation failed criterion1 after5allowed sourcepaths/6new+1updatedUNRUNfixtures and byte-boundwriter snapshots/hashes/delta/protected/whitespace checks; remainingclosuregenerator KeyErrorlane stopped. Fullhandoff/meta captured `gtk-continuation/parent-consumed`; no pendingrequests or active continuation. Windows6partialpaths lackafterhash/diff/protectedpostchecks. No newretry/sourcechange/review/integration inferred. Ask explicitparent-only metadata recovery: preservefailedscripts/states, retainGTKpassedchecks instead ofrerunning, captureonlymissingWindowsevidence and missingoriginal-boundclosure; do not addunowned test-readyadapter or fix timelinefixture implicitly. Then NEW-delta reviews only whenclosure evidenceavailable/authorized. Tasks28/30incomplete,29notstarted,25stopped/26notstarted; no builds/tests/native/model/WSL/inference/acquisition/backend/schema/DTO. Original8350MB UNKNOWN/BLOCK, runtimeP2deferred.

## Prior action — exactlyone authorized GTKcontinuation; Windowsstopped partial

Owner approves exactlyone same-native retained GTKcontinuation after noeditspathfailure/timeout. Existing briefscope/exact absolutepath/sourceonly; Task30inprogress, validated `prerequisite-fix1/gtk-continuation/workflow.js` awaitinglaunch atrecord. Original two-writer workflowfinished failed2/zero reviewers. Windowslate steer received: metadata succeeded then6allowedChatpaths modified, new rglookaroundunsupported query stopped beforeafterhash/diff/protectedchecks; partialsourceunaccepted, fixturesunrun, no Windowsfollowup implicitlyapproved. Preserve oldfailedreports/states. HoldreviewTask29notstarted untilboth byte-boundhand-offs; Task28incomplete. No sourceapply/integration/tests/build/format/compiler/native/model/WSL/inference/acquisition/backend/schema/DTO/runtimeP2changes. Runtime8350MB UNKNOWN/BLOCK, originaldirtysource preserved. Further Windowssource/artifact correction needs explicitauthority.

## Earlier blocker — GTKpreparation/communication stop, no source edits

GTKsourceworker failed before edits on guessedpathENOENT then supervisorwaittimeout; handoff fullyread/preserved. Correctabsolute auditpath verified/briefclarified; no retry/resume. Windowsmetadata KeyErrorlane pre-edits; bounded predicateget/oneinvocation decision queued existinglive-native steer, no confirmeddelivery. Bothnative reply attempts foundnopending; no exposedintercomtool, no rootcause assumed. Reviewstage cannotlaunch afterfailedwriter. Preserve exact failed statuses and scratch evidence; request explicit same-native GTKcontinuation before new child. No originalapply/build/tests/native/model/WSL/acquisition. Tasks28/29incomplete,25stopped/26notstarted, runtimeP2deferred/8350MB UNKNOWN/BLOCK.

## Prior action — authorized bounded frontend corrections, original untouched

Owner approves **Correct Windows and GTK sources**. Packet `hardening-integration-20261011/prerequisite-fix1`:118source pins/protected manifests, two exclusive isolated writers plus independent NEW-delta reviews. Tasks28/29inprogress; consume managed full handoffs/metadata and disposition minimal sourceclosure before any integration. WindowsChat-only capabilities/ACK/citations; GTKordinary startup/defaultclosed unaccepted actions/actualsixfield literal None/None and uninspected original-bound closure only. No old inspection/test reruns, backend/schema/DTO/wholeTools/Catalog copies, native/model/WSL/compiler/build/format/tests/inference/acquisition/sourceapply. Extra unowned requirements/failures must stop. Task25stopped/26notstarted; original8350MB UNKNOWN/BLOCK and runtimeP2deferred.

## Prior blocker — prerequisite source findings, explicit correction authority needed

All3 scoped checked reports consumed; Task27complete only source triage. WindowsBLOCK: generation/cancel backendgate, cancelACK presentation identity, historycitations; preserve schema1.8 DTO and isolate Chat-only closure. Appleconsumer3paths sourceOK with notes/eightpath closure; no native/Maccompile/20fixtures acceptance. GTKstopped/incomplete: potential startup/loader/terminal/capability defects plusactualsixfield DTO omission. Parent sourceaudit rejects the reported snapshotfailure: frozen5contracts match actualoriginal; reviewerrelativepaths used oldLinuxworktree. Preserve original report and audit; actualoriginalGTKtest-only/backend closure not complete. No repairs/apply/tests/build/model/WSL/native/acquisitions. Task25stopped,26notstarted. Ask for bounded sourcecorrections and path-bound uninspected closure review before assuming merge/build authority. Do not repeat completed narrow reviews or import Tools/Catalog/IPC/backend by implication. Runtime8350MB UNKNOWN/BLOCK and trackerP2deferred.

## Prior action — authorized NEW prerequisite source review, no apply (2026-10-11)

Owner approved3targeted read-only prerequisite closure reviews after safe integration stop: Windows earlier persistentChat/FIFO/control/client interfaces and actual backendcapability gates; GTK native_runtime/dispatch/module/settings/enginecontract; Apple earlier atmosphereconsumer/package wiring. Freeze459source records/validateworkflow under `hardening-integration-20261011/prerequisite-review`. No old narrow inspection/test rerun, sourcechanges, native/model/WSL/build/execution/acquisition/integration. Exact closure/guard/sourcefindings required; honest not-run declarations, no claims sourcechecks prove runtime. Task27inprogress,25blocked,26notstarted. Consume saved completion results/metadata before any broader authority inference.

## Earlier blocker — prerequisite-only source closure BEFORE integration (2026-10-11)

Owner approved reviewedcompatible sourceintegration+offlinecached .NET checks. Exact13texts baseline-match/safe-new, no appliedsource/build/tests. Stoponunreviewed-prereqs triggered: Windows persistentChat/clientControlButton/generation API absent; GTK native_runtime/Deep dispatch absent; Apple atmosphericconsumer wiring absent. Prior completion handoffs/tests do not substitute cumulative prerequisite review. Preserve dirtyoriginal/source snapshots; do not copywholemissinglayers or enable backend/native guards. `hardening-integration-20261011/dependency-blocker.json` plus attempt2 compatibility/backups are authoritative. Preparationcp1252BOMserializerfailure preserved; separate scratchUTF8metadataattempt succeeded.

Need explicit prerequisite-only source-review/closure disposition before applying originals or starting Windowschecks. No extra inference/Rust-runtime/WSL/GTK/Mac/smoke/acquisition/commits authorized. Runtime8350MB UNKNOWN/BLOCK, trackerP2 deferred, originalincidents unchanged. Scope sourcecandidateacceptance from lastwave is not whole-layer/native certification.

## Earlier checkpoint —13 source candidates bound; integration/native gates pending (2026-10-11)

Pass2 eightchildworkflow complete, allfulloutputs/statuses read. Reportdelivery resolved without rerunning oldinspections/tests; checkednot-run evidence is not command/test/native success. Current Windows/perf sourceOK-notes and new motion/GTKprep sourceOK. Parentbound13 actualsource hashes/copiedbytes and fouremptyindexes; SOURCE-only tasks20/23/24complete. Originalfailedreports remainfailed, historicalmotion2P1/GTKP2 findings retained aspass1baseline records.

No source applied to original or native/combined tests run. Motion20Swift fixtures and GTKisolatedfixture/launcher UNRUN. Prepare dependency-aware three-way integration compatibility against frozenoriginal/currentbytes before any authorizedapply; do not silently copywholefiles/enableguards. Integration/newWindows/WSL/native verification not included in last boundedcorrection approval. Runtime8350MB UNKNOWN/BLOCK, trackerP2 deferred, nativeMac unavailable, npm/Linuxsmoke/runner unknowns preserved. Need explicit next disposition; no extra inference/smoke/acquisition/reviewrerun/publication inferred.

## Earlier action — approved source pass2 and original-report-only redelivery (2026-10-11)

Owner chose Continue bounded corrections. Pass2 inputs/briefs/authorization frozen and one native workflow validated. Resume2 retained source writers for motion scheduling resume/live-detach fixes and isolated GTK fixture preparation; resume4 failed completed reviewers ONLY to redeliver exact old findings with truthful explicit not-run evidence (no source reinspection/test commands). Independently inspect only2 newly changed source deltas afterward. Old failed metadata/statuses remain failed, old motion BLOCK and GTK P2 remain historical source findings, not reinterpreted as pass2 verdicts. Tasks23/24 in progress, task22 runtimeP2 deferred.

No new Swift/GTK/WSL/native/model/app/media/launcher execution, dependency/acquisition or integration. Source/artifact checks allowed; fixtures authored/unrun. Stop/report any new execution/delivery failure without automatic retry or protocol/model substitute. Native acceptance and original8350MB failure remain BLOCK/UNKNOWN. Packet `hardening-wave1/repair-pass2`; no extra performance/native claims.

## Earlier action — explicit disposition after fresh review delivery failure (2026-10-11)

Workflow06da6bd3 complete/BLOCK after9children. All4candidate/fullhand-offs and4freshreview bodies read: Windows19focused/perf16fake/GTK4WindowsSTD tests; motion9newSwift authored15totalunrun. Source:Windows/perf OK notes, GTK P2 isolated-XDG fixture prep, motion2P1 scheduling resume/live-detach BLOCK. Every fresh reviewer host rejected commandsRun=[] under checked-level minimum evidence; no rerun, invented executed command or relabeling accepted. Preserve actual independent source findings and failed metadata.

Obtain disposition for next bounded motion source fixes/GTK fixture preparation plus report-only supported native delivery corrections accurately declaring nonexecution (not new review inspection/runs); root/native/performance gates stay closed. Task22 runtime trackerP2 separately unapproved; original8350MB causeUNKNOWN/nativeBLOCK. All24tasks retained. No extra inference, WSL/GTK/Mac/native app/model/media/smoke execution or new dependencies/acquisitions. Do not integrate until parent exact-byte/source/acceptance disposition and separately authorized combined/native gates.

## Earlier action — consume remaining candidates and independent new-diff reviews (2026-10-11)

Workflow `06da6bd3-ef4d-4551-b754-972c9984738e` active. Perf16 fake tests/GTK4 Windows STD-only tests handoffs read; fresh source reviews pending. Supported native report delivery confirmed in retained metadata; no native/runtime/integration acceptance. Runtime reader reports source OK-with-notes/original failure UNKNOWN/nativeBLOCK plus process-tracker P2 (task22, not part of current app repair authority).

Motion serializer paused before invocation; parent authorized scratch-only preparation validation and one corrected delivery, preserving source/failed helper/body/log. Second no-change edit mismatch does not permit more execution or candidate edits. If syntax invalid/delivery fails, stop/report. Consume actual remaining Windows/motion and review completion notifications; no polling/repeated finished work. Native/GTK/Swift/render/AT/performance/model gates remain explicit and no extra inference is authorized.

## Earlier action — bounded source corrections and supported native reports (2026-10-11)

Owner approved continuation after schema-rejected reviews. Inputs frozen and four retained writers available; launch validated `hardening-wave1/repair-pass1/workflow.js` for the four priority source corrections plus read-only runtime evidence review, followed by independent NEW-diff reviews. Use supported acceptance-report fields only; preserve old rejected runs. No old-review rerun, extra inference, WSL/Linux smoke, native Mac/app/media execution, dependencies or downloads. Windows cached headless focused checks/Python fakes/optional STD-only presentation logic allowed; remaining native gates explicit. Runtime original failure UNKNOWN/BLOCK despite one passing diagnostic. No automatic integration or whole-app/performance acceptance from worker/report completion.

## Earlier disposition — retained review delivery failure and bounded app corrections (2026-10-11)

Single diagnostic execution is finished: 1passed, tracked cleanup/accounting0; stop honored. **No extra runtime execution authorized.** Original8350MB cancellation failure remains UNKNOWN and runtime BLOCK. Current old isolated settings also contain CPU override; no historical/cause proof from that read. Obtain independent source/evidence review and parent disposition, not another inferred retry.

Five source responses recovered from retained transcripts after host acceptance-report schema rejection (unsupported parent-requested JSON fields; advertised files empty). Preserve failures; no reruns/integration. Candidate defects independently source-traced: Windows stale deferred Chat history overwrite, GTK actionable-error loss on Failed/Exited, Apple synchronous State publication from representable update/teardown. Controls/media have source-only OK verdicts, not native acceptance. Sixth perf review recovered: P1 first-use queries contaminate warm-labelled samples at low/zero warmups; P2 fake-positive responses violate CatalogResponse/CatalogHit required fields. Include these in bounded task20. All six native reports remain rejected; workflow aggregate completion/ok=true is not review acceptance. Stop new delegation launches until corrected native handoff contract disposition; use supported report fields and retain original rejected metadata. Scope bounded tasks20/21 under broad rewrite authority, reconcile cross-lane stale suggestions before broader work, and measure allocations/frame time rather than invent speedups.

## Earlier actions — app hardening review and single runtime classification (2026-10-11)

Owner permits broad app refactoring for hardening, performance and code cleanliness. Launch six fresh independent source-only reviews of saved disjoint candidates and adjacent native app risks; no competing builds/tests/models while classification runs. Prioritize safety/lifetime/correlation correctness and measured bottlenecks; preserve native identity, fail-closed guards, portable Rust architecture and in-process Apple adapters. Reviews do not establish native or performance acceptance. Parent owns bounded repair scopes and integration; no package/acquisition/private-data/migration/publication expansion.

Corrected classification script validated and workflow `3d0948f1-f726-48ec-8dce-2403cc7d7453` launched under explicit owner approval. Consume its completion rather than polling; one exact instrumented Windows lib regression after offline compilation, then stop for disposition. No additional runtime test/repair authorization. Prior failed launch `e17e75f2` remains failed, and runtime remains unaccepted.

## Earlier next action — validate and launch corrected classification (2026-10-11)

Owner explicitly selected **Correct and launch once** after parent reported the rejected output argument. Correct `file` to `file-only` in the same native retained-worker request, validate the saved workflow, then launch only the previously approved single diagnostic test and stop. No classification test has executed. Script/authorization: `scratch/runtime/classification-once-20261011/workflow-corrected.js`, `launch-correction-authorization.json`. Original failed workflow stays failed and runtime stays unaccepted.

## Earlier blocker — classification launch parameter rejected (2026-10-11)

Classification workflow `e17e75f2-3b80-4ccd-b42c-f495d5ffbbc5` failed BEFORE child dispatch: parent used `outputMode: file` instead of valid `file-only`. Receipt reports complete inventory with no child run IDs. The approved single classification test has not been dispatched. Stop/report; obtain disposition for a corrected same-native launch, not another inference retry or alternative protocol. Failure receipt retained; parent incident record is `scratch/runtime/classification-once-20261011/launch-rejected.json`. Runtime classification/acceptance and all fresh reviews remain pending.

## Earlier next action — one owner-approved classification run (2026-10-11)

Recovery2 ended blocked: runtime acceptance criterion-1 not satisfied; **no fresh reviewers launched**. Parent consumed its blocked handoff. The returned cleanup error/native child PID remain unrecoverable from the original evidence. Owner explicitly selected one instrumented Windows resident regression after offline diagnostic compilation: same approved server/models, generated fixture and isolated CPU configuration, assertions unchanged, then stop for parent disposition. No retry, fallback, repair, WSL/handler/CLI/full-suite checks or unknown-resource release. Preserve the original failed run and distinguish classification evidence from runtime acceptance. Brief/authority: `briefs/runtime-classification-once-20261011.md`, `scratch/runtime/classification-once-20261011/parent-authorization.json`. Classification has not run yet; missing reviews/integration/hardware gates remain.

## Earlier blocker — classify real resident cancellation failure (2026-10-11)

Runtime recovery's real resident regression failed once after unit/focused/full Windows tests, strict Clippy and canonical CLI validation passed: expected memory accounting 0, observed 8350 MB after 70.01s, with active model None and cancellation returning an unprinted error. Keep runtime unaccepted. Supervisor disposition permits only retained failure/code and redacted exact fixture-owned process classification; no inference retry or WSL checks. If the cleanup error is unrecoverable, obtain explicit approval for one separately hashed instrumented classification run; confirmed new native termination/wait failure stops under the recovery rule. Preserve `scratch/runtime/fix-pass1/byte-bound/windows-real-resident.log` and `parent-resident-failure-disposition.json`.

All three native recovery candidate handoffs are consumed: lexical harness (14 passing fake-engine tests, no real timing evidence), control semantics (one Swift file, no behavior tests) and atmosphere lifecycle (six owned files, six Swift tests authored but unrun). Fresh independent review and native Mac gates remain pending; no integration/acceptance. Do not relocate active source, unblock scope guards, or infer best-performance/visual completion.

## Earlier blocker — native runner recovery disposition (2026-10-11)

Both current workflows failed because runner process 26980 disappeared without results. Preserve `%TEMP%/fileid-completion-20261010/runner-failure-20261011` and original incident/review failures. Runtime fix run `5aa79136-afb0-4c9d-b024-020a3565fd14` has a retained resume route; three native writers have no session and unchanged frozen source inputs. Owner explicitly approved same-native-protocol recovery; current doctor/owned-process checks are saved and workflow `d5871f1f-fc06-4301-8163-94833b1b4c7b` resumes the retained runtime session plus only unchanged native candidates, then seven fresh inline reviews. Consume completion wakes and arbitrate exact handoffs; no silent restart/CLI/model substitution if recovery fails. Fresh core/native reviews and accepted prerequisite integration remain absent. Parent `commands/deep_scope.rs` is an unregistered helper candidate with six standalone Windows tests only; finish admitted handler/per-target wiring, Linux/Mac/full-crate tests and fresh review before enabling native scope UI.

## Native UI craft and parallel evidence (2026-10-11)

Apply the owner's `~/.codex/skills/adam-ui-design` guidance to cohesive native task hierarchy, palette/material/type/spacing, selection/detail motion, keyboard/focus/errors, resizing/text scaling, reduced-motion and occlusion fallbacks. Preserve FileID gold/LavaLamp/springs/six-tab identity; do not use web tech. All six read-only platform UI, migration/Apple-adapter and performance inventories are consumed/preserved under `%TEMP%/fileid-completion-20261010/native-wave1/inventories`. Disjoint Apple motion/control-semantics and lexical-harness candidates run in workflow `53652b69-9372-46ed-ac91-e5f345fd2f44`, with fresh inline reviews and no real engine/heavy/native-Mac execution. Follow candidates with parent disposition and real generated-fixture rendered/interaction/profiling evidence; source builds do not establish visual perfection or Mac hardware acceptance. Parent owns coordination and TODO updates; no overlapping writers, uncontrolled heavy builds/model execution or Agent Fleet/GitHub publication.

## Active target — shared Rust engine and native desktop frontends (2026-10-11)

Owner approved one Rust behavioral core for Windows/macOS/Linux, explicitly retained Apple acceleration adapters and chose all Apple-native services in-process. Frontends are presentation/interaction only; adapters retain narrow OS/hardware service mechanics, never a second pipeline/catalog/job engine. Preserve each platform's best hardware/media paths and prove performance with separate measured native baselines; source convergence is not acceleration/throughput evidence. Keep SwiftUI, WinUI and GTK native. Inventory relocation to `shared/engine`, all build/policy/relative-include dependencies, Rust-vs-Swift IPC/behavior/schema differences, frontend-owned inference and catalog/settings/bookmark migration. Wait for active component writers before relocation; integrate reviewed fixes without losing their hashes/evidence. Preserve reference Swift code until native Mac parity, round-trip data, signing/sandbox and hardware gates pass; do not claim a directory move is macOS support. No new dependencies/downloads, personal-catalog mutation or publishing is authorized.

## Active completion continuation — Windows/Linux (2026-10-10)

The owner requested remaining actual Mac parity. Workflow `76b7aa97-e394-4b97-8bb2-2153a963db86` stopped after a two-hour runtime timeout, missing handoff and unauthorized build-time npm install (`LLAMA_BUILD_UI=ON`). Owner authorized a bounded retained-evidence audit and fresh cached-only headless rebuild in workflow `09d54fe2-eb8c-4b57-8cbd-67ed1d2ef5b6`; downstream work remains gated on recovery, fresh reviews and parent acceptance. The audit established no attributable outside-area write, but cache/network/lifecycle effects remain unknown and are not cleared by a fresh build. Preserve logs, source state and installation evidence; do not reuse the Linux binary as an accepted prerequisite or silently rerun its build. See the newest `STATE.md` entry. Snapshots, model cache, fixtures, prerequisites and logs are under `%TEMP%/fileid-completion-20261010`. Windows/Linux/media handoffs exist; independent reviews, downstream implementation, integration and full acceptance remain pending.

1. The cached-only headless runtime recovery is saved, not independently accepted. Complete bounded fixes for the nine concrete runtime/media/Windows/Linux P1s and two P2s in separately owned lanes. All four review required-file deliveries failed despite saved BLOCK responses; preserve labeled captures and use same-native-protocol inline structured changed-blast-radius review, not a CLI/model fallback. Preserve audit unknowns and stop on newly established outside-scope effects or unexpected acquisition/provisioning. Do not broaden installation investigation, perform cleanup or inspect personal configuration. An approved replacement must disable UI installation/fallback provisioning, use approved cached sources and retain fresh identities. Recover the missing handoff and reconcile three stale source hashes before fresh independent review. Resolve review findings before reusing runtime/media APIs. Preserve true decoded timestamps, raw 16-kHz PCM anchoring and staging ownership; the media helper owns transient RAM/CPU/I/O admission, so consumers must not double-reserve it. Retain explicit Windows delayed-audio export refusal and actual sparse-coverage warnings.
2. Complete schema 1.8.0 scope enforcement and typed Windows/Linux exclusion wiring. Codec tests pass but the existing backend does not yet enforce the new fields. Keep fail-closed guards until integrated tests cover directory boundaries, malformed settings/paths, resolved targets, empty selections and explicit-selection semantics. Preserve unknown-key/startup safeguards; no raw-JSON workaround.
3. Wire parent-owned scan ModelStack startup CPU/I/O admission around load/warmup, releasing it on success/failure/cancel; avoid nested reservations under existing catalog leases on single-core hosts. Validate combined resident models/decoders/index and interactive priority under real generated workloads.
4. Run parent actual inference/media/restart/cancel/no-clobber checks using the 12 hash-verified approved model files and generated fixtures. Current 100,000-record keyword baseline p95 is 112.01 ms; semantic/hybrid/graph restore, real embedding acquisition, scan throughput and contention remain unmeasured. Do not substitute synthetic data for personal-library quality or native Linux/ARM64 acceptance.
5. Investigate the unexplained Linux smoke failure, run combined strict Windows/WSL/.NET/GTK suites, review new network-capable source diffs and refresh only reviewed pins, and update final records. No commit/push, Store install/trust change, dependency/package install, CUDA download or restricted-model acceptance is authorized by this continuation.

## Earlier integrated slice — Windows/Linux feature parity (2026-10-10)

The reviewed catalog/search/index-job and native-workbench slice is integrated locally, preserving existing Store/runtime/restructure edits. No commit or push was requested. Read the newest `STATE.md` entry and `NEXT_VERSION.md` before continuing; earlier Mac-first port deferrals below are historical, not a reason to discard this integrated work.

1. Continue the actual macOS timeline/speech/chapter-generation and model-backed chat implementations into the shared engine and native ports. GTK Deep Analyze currently reviews stored captions only; it must not pretend inference is available. Keep unsupported video/tool capabilities explicit.
2. Add isolated, interruptible conversion/video workers and complete model/decoder/GPU/NPU accounting, fairness and cancellation. HNSW phases are cooperative between phases, not preemptible within graph operations; benchmark graph restore/rebuild and warm hybrid retrieval on 100,000 files before claiming latency targets.
3. Validate real embedding acquisition/model identity, native WinUI interaction, Linux hardware/aarch64 and output fidelity on approved fixtures. `check_portable_parity.py --engine <binary>` covers only disposable catalogs/files and synthetic vectors; cross-device acceptance used WSL `/dev/shm` exports. Tools Undo and GTK Restructure Apply/Undo are enabled after shared-backend safety checks. Conservative legacy recovery still needs manual review rather than inferred ownership.
4. Keep the prior Windows Store/CUDA release work below open. Headless builds/tests do not prove package startup, provider execution or release readiness. When the owner requests committing/pushing, land the reviewed changes on a branch, rerun exact-head GitHub CI, then merge; do not publish the dirty checkout as an already accepted release.
5. Whole-app Linux formatting still reports existing main/spring/Cleanup/People/Settings formatting; the parity files pass targeted rustfmt. Keep any unrelated formatting cleanup separate from this feature integration.

Parent checks passed: Windows build/format and 502 app + 49 IPC tests; Windows/WSL strict engine Clippy and tests; GTK app tests, real-engine Xvfb workbenches and entrypoint smoke; portable safety/index probes; strict egress and 24 policy tests. Logs are `%TEMP%/fileid-parity-integrated-*.log`. Original-edit backups and component patches are `%TEMP%/fileid-parity-integration-20261010`; isolated lanes are `../FileID-parity-worktrees/{engine,windows,linux}`.

## Earlier owner request — Store startup and CUDA validation (2026-10-05)

Windows release audit is still in progress. Read [`WINDOWS_AUDIT_2026-10-05.md`](WINDOWS_AUDIT_2026-10-05.md) first. Finish the managed security scan (currently discovery 0/399); fix the confirmed restructure stale-plan source identity gap and add a regression; verify a Store-identity package launch without replacing the installed Store app; remove the temporary diagnostic certificate/private key through the supported Windows certificate-management path; obtain owner acceptance of NVIDIA/cuDNN upstream terms before any runtime download; then run GPU/corpus, App Certification Kit, exact-head GitHub CI, and Partner Center listing checks. Keep the demo video excluded. Current headless Rust and .NET checks pass, but this is not a release-ready sign-off.

The Store-installed 0.1.1.0 app still fails in `MainWindow.InitializeComponent()` with a generic XAML parse exception. Reproduce the package-only failure and identify the missing resource before calling FileID ready. Release logging captures `XamlResourceReferenceFailed`. Optional CUDA runtime packs install only after an explicit user action and license acceptance, from exact SHA256-pinned Microsoft/NVIDIA URLs. Verify ORT CUDA scanning and CUDA Deep Analyze on this system’s RTX 5080; users must install the matching runtime and restart the engine before CUDA scanning can activate. The old `G:\TrueNAS` corpus is not mounted; supply a representative copy using `iterate.ps1 -Corpus <path>`. Later dated RTX 2060 and `G:\TrueNAS` entries record the former machine or historical measurements; use this current profile when carrying out open hardware checks. The Store build continues to use its bundled Vulkan Deep Analyze runtime by default. The listing still needs authentic screenshots after the package launch issue is fixed. Do not use the promo-video draft. The public product site is `https://fileid.webworldwide.online/`.


# NEXT — resume here

## Earlier owner request — Windows Store update (2026-10-05)

The owner reports FileID has Store approval, and this PC has `AdamNolle.FileID` version `0.1.1.0` installed. Partner Center was not reverified during this turn because the browser is at Microsoft sign-in. After fetching, this checkout and `origin/main` match at `754f8b0b69b0e04ac6764cff01fa34d136ebb8ad`; all 18 workflow checks for that commit completed successfully. GitHub's combined status endpoint still returns `pending` without status contexts.

The installed Store package still crashes while loading `MainWindow` XAML (`Microsoft.UI.Xaml.dll`, `0xC000027B`). The local unpackaged Release build reached engine-ready and a Settings binding event, but this does not validate Store package identity. Continue with package-specific diagnosis; do not call the update ready until a fixed `0.1.2.0` MSIX is tested and the App Certification Kit report is reviewed. Then capture genuine screens for Library, People, Cleanup, Deep Analyze, Restructure, and Settings (the current listing draft has one), verify CUDA scan and Deep Analyze on the RTX 5080 using a copied corpus, and rerun release gates. `G:\TrueNAS` is absent. Keep the promo-video draft out of the listing.

## Historical Store handoff — 2026-10-03

PR [#213](https://github.com/WebWorldWide/FileID/pull/213) prepared the Store package path and merged to `main` at `a11cbd09284b7e8305f298baf502473a3ed2b392`. PR [#219](https://github.com/WebWorldWide/FileID/pull/219) raises the Windows Store package to `0.1.1.0`; its exact-head Store workflow [37160912487](https://github.com/WebWorldWide/FileID/actions/runs/37160912487) built and verified `FileID-0.1.1-x64.msix` for identity `AdamNolle.FileID`. SHA-256: `99b8661db9fec40d375cf1f55d06d0acfe18699683d7adc6c211030983216aba`. The Windows app, Linux, Linux packaging, repository policy, Store package, and native tools CI workflows passed on that head. The Windows engine x64 test is still running.

Main advanced after PR #219 was opened, producing documentation conflicts in `NEXT.md` and `STATE.md`. Merge the latest main update and rerun exact-head CI before merging the PR. The Partner Center product `9PC8HSD86887`, submission `1152921505702035520`, has complete pricing, properties, IARC 12+ / ESRB Teen age ratings, and English listing; automatic publication after certification is selected. It still contains only the validated `0.1.0.0` package. A stale duplicate upload was canceled. The verified `0.1.1.0` artifact remains to upload and validate, then submit for certification. WACK and on-hardware model/GPU acceptance remain unverified. Do not modify Adlon example data.

## Historical priority — macOS first (2026-10-03)

Continue the accepted next-version plan on macOS. The Windows app packaging job currently skips `FileID.App.Tests`; hosted discovery run [37161178829](https://github.com/WebWorldWide/FileID/actions/runs/37161178829) exposed 157 existing test compilation errors against current app APIs. Repair that suite on the owner's PC without exclusions. Latest `main` is `a11cbd09284b7e8305f298baf502473a3ed2b392`. Read the newest `STATE.md` and `AGENT_HANDOFF.md` entries for CI status and exact continuation.

Local TestFlight signing is provisioned and the package passed strict signature checks. The owner accepted Apple's internal-use API acknowledgment, but has not separately approved creating the team API key that grants access to every app on the team. Until approved, do not create it or upload the build; no TestFlight build or testers are present. Adlon CI uses isolated guest disks; its example-data volume stays unmounted and untouched.

## TestFlight completion gate

The App Store build path uses scoped source/output bookmarks, a sandbox-inheriting helper, container-local model weights, and `platforms/apple/scripts/testflight.sh`. App ID `com.fileid.app`, App Store Connect record `6818813859`, and the empty `FileID Internal Testers` group exist. The Mac App Store profile and app/installer signing identities are installed locally, and the signed package passed strict verification. API access is enabled, but creation of a non-expiring team key with access to every app is awaiting owner confirmation. If approved, store its one-time `.p8` only in protected local storage, upload the validated package, wait for Apple processing, then validate sandboxed scan/export/model-download behavior. No testers have been invited. Keep signing material out of Git and Adlon.

## Prior Mac increment — make timeline coverage reliable

The current timeline samples one frame every ten seconds. It now creates clearly labeled low-confidence draft chapters only when a changed caption is supported by the following sample, and stores timestamped on-device speech passages from overlapping chunks. These signals do not establish that every event occurred or did not occur. Next add shot/activity proposals, dense candidate analysis, face tracks, and explicit visual/audio coverage through long recordings and chunk boundaries. Add fixtures for brief actions, overlaps, scene boundaries, and events that cross chunks.

Continue `shared/docs/NEXT_VERSION.md`: improve face accuracy/speed, best-take ranking, conversion/enhancement/reframing, hybrid search, and local model scheduling. Validate macOS changes natively; keep port feature work deferred until the owner is on the PC. Use only isolated Adlon runner guest disks for CI and keep the example-data volume unmounted.

## Historical day-end continuation — superseded by Mac-first priority

The full accepted goal remains active after the owner resumed sustained work; this is a recovery checkpoint, not a pause or release completion. Read AGENT_HANDOFF.md and ARCHIVED_BRANCHES.md first. Resume the unfinished Windows safety/test-parity checkpoint from archive/2026-10-02/windows-test-parity-wip; do not merge the recovery snapshot wholesale. Restore real behavior, canonical IPC fields and genuine test execution, then require both test suites, x64/ARM64 builds and format checks. Main app packaging success currently omits the suites because of a path bug.

The compiler-path fix passed all three fresh main Windows engine jobs in run 37033359014 (x64/cross on Adlon, native ARM hosted); all four Linux jobs passed after disk expansion in run 37028488104. Preserve this routing and its assertions. Corpus /srv/data remains strictly read-only and unmounted in CI guests. Continue the complete NEXT_VERSION.md ledger after these safeguards; no release or full feature/accuracy acceptance has been established.

## Historical checkpoints — superseded by the priorities above

The dated notes below retain prior work and release/signing history. Branch names, in-progress status and earlier owner instructions describe those checkpoints; they are not current continuation instructions. Use AGENT_HANDOFF.md and the day-end priorities above.

Adlon x64 engine passed; ARM64 cross failed because clang was absent. Complete the signed Visual Studio LLVM component installation and verify the new cross-job PATH step on actual main before declaring server acceptance. Preserve all test/build gates and corpus isolation. PR #190 contains atomic merge fixes; #191 is an unfinished Windows safety/test-parity draft. Do not merge #191 until every app suite runs and passes without exclusions.

## Immediate continuation — 2026-10-02

The atomic-person-merges follow-up passes local full suites, actual-engine merge fixtures and policy checks. Inspect its fresh hosted results before main integration. Preserve failed completion assertions, structured-name transfer and rollback checks. Source identity aliases/history still need design and migration; explicit merges currently delete the source identity. Inspect every serial Adlon Windows main matrix job, then fix the skipped WinUI test gate without weakening assertions.

Inspect the fresh People merge-head CI after resolving documentation conflicts with main, then integrate PR #189. All sixteen checks passed for its original 7f03fff head. Bootstrap #188 merged at bb33211 after all six checks passed; actual Adlon Windows main jobs still need final results. Continue validating the isolated atomic-person-merges draft, which reproduced swallowed SQL errors and lost structured names. The full next-version goal and NEXT_VERSION ledger remain active; passing these milestones is not release completion. Fix the skipped WinUI test gate and wire the GTK shell before claiming port acceptance.



## 2026-10-02 — Continue the entire accepted next-version plan

The owner revoked the natural day-end stop and requested sustained work until everything is complete. Finish and verify Adlon Windows service bootstrap, merge the bounded fix into main, and keep the goal active. Then finish stable People IDs and cross-engine transactional persistence, durable refresh and incremental assignment, persistent model-separated hybrid search, resource-budgeted model routing, typed chat execution, full temporal analysis/chapters/best takes, enhancement/reframing and broad conversions, native port parity, real-data/hardware gates and release readiness. NEXT_VERSION.md is the scope ledger; passing existing checks alone is not completion. Keep Adlon example data strictly read-only and use separate CI guest disks.

## 2026-10-01 — Start the next session from the integrated milestone

Read AGENT_HANDOFF.md, NEXT_VERSION.md and CI_RUNNERS.md before editing. Verify main and its exact CI runs; do not reconstruct the superseded branch history or reinstall runners. The full next-version plan remains unfinished. First finish stable person identities across reclustering on both engines: protect named/unknown/offline identities, partition protected/negative constraints before merging, update existing IDs transactionally rather than deleting People, preserve manual observations/corrections, and test rollback/empty/mixed-cache cases. A native/portable-helper prototype exists on the owner's internal Mac; its Rust database handler is not wired and it must not be treated as production code.

Then implement durable face refresh/incremental assignment and persistent model-separated hybrid indexes. Follow the ordered delivery ledger for scheduler, typed chat operations, timeline/ASR/tracks, best takes, stabilization/upscale/reframing, broader toolbox and native ports. Strict runtime egress, GTK wiring, hardware accuracy/fidelity, recovery, licenses and release gates remain open. Adlon runners use only separate CI guest disks; the example-data corpus remains read-only. Unrelated Store/Windows-gate/dependency proposals must be reviewed separately; do not discard another worktree's work.

## 2026-10-01 — Continue from compatible face-space guards

FACE_CACHE.md records the conservative whole-pass guard. Do not relabel legacy vectors, accept mixed processing namespaces or use unversioned person centroids to bypass it. Complete durable bounded refresh and explicit failure/offline coverage, namespace-aware incremental assignment and stable user-managed IDs, then held-out calibration. The current guard can defer clustering until a legacy cache refresh finishes. Native suite is 430; Rust is 385 library/386 executable plus two integrations. The full accepted ledger remains active. Continue verified pushes and keep owner edits and Adlon untouched.

## 2026-10-01 — Continue from bounded native video export

IPC v1.4 and TOOLS.md describe native SDR H.264/AAC exports with actual worker/output/Undo tests. Port the worker and native Tools UI; do not let unsupported persisted recipes execute through a fallback adapter. Complete resource-safe portable execution/cancellation, trim/audio/remux/proxies, stabilization/enhancement/tracked reframing and broad adapters. Persistent hybrid retrieval, full People space isolation/backlog/calibration, typed chat execution, dense timeline/ASR/tracks, automatic chapters/best takes and all release gates remain active. Keep implementing and pushing verified milestones, preserve owner work and never modify Adlon.

## 2026-10-01 — Continue after versioned face metadata

v22 and FACE_CACHE.md define new verified face-cache provenance and bounded refresh. Continue complete clustering-space isolation, durable refresh/backlog and incremental assignment; resolve ambiguous CLIP producer/query model namespaces before persistent hybrid indexing. Complete resource routing, typed chat execution, timeline shots/ASR/tracks/dense outcomes, best takes, media/broad tools and native port UI. Strict runtime egress remains blocked by the existing GitHub/NVIDIA runtime artifact download paths; replace them with vetted packaged runtime provisioning before release. Keep all accepted gates in NEXT_VERSION.md active, preserve owner edits, and keep Adlon read-only.

## 2026-10-01 — Continue from initial model admission

Initial physical/available-memory checks and native residency leases now protect model switches; SCHEDULER.md defines exact limits. Continue persistent hybrid indexes and processing-version caches, durable CPU/I/O/GPU/NPU reservations, multi-model/task routing, fairness and general restart recovery. Then complete typed chat operations, timeline speech/shots/tracks/dense outcomes, automatic chapters, best takes, video/broad tools and native port UI. Preserve owner work and strictly read-only Adlon; push coherent verified milestones to PR #186.

## 2026-10-01 — Search refinement landed; continue scheduler and hybrid retrieval

Contextual media-filtered keyword search now works in both engines and cross-engine history. Continue memory/resource admission and model residency, persistent model-separated hybrid retrieval, versioned People processing, and typed reversible chat operations before timeline/ASR/tracks, best takes, video tools, broad adapters, port UI, and release acceptance. Continue verified incremental pushes to PR #186; preserve owner edits and strictly read-only Adlon.

## 2026-10-01 — Continue after local chat

Work on codex/fileid-next-version-current and draft PR #186. Initial native chat and cross-engine local history now work; CHAT.md records precise boundaries. Complete contextual/filtered hybrid retrieval, resource-budgeted scheduling and model routing, persistent indexes, versioned People processing and incremental assignments, and typed chat operations. Then complete dense timeline/ASR/tracks, automatic chapters, goal-conditioned best takes, video/audio/enhancement/reframing and broad format adapters. Wire and validate native port interfaces before release. Keep pushing verified milestones, leaving AGENT_HANDOFF.md current, and checking hosted CI. Do not modify Adlon or absorb owner release/signing changes.

## 2026-10-01 — Continue the accepted plan from PR #186

Continue on codex/fileid-next-version-current. Safe initial photo/chapter tools are now implemented; consult TOOLS.md for exact pairs and limits. AGENT_HANDOFF.md has continuation instructions and correct pinned-Clippy invocation. Keep pushing coherent verified milestones to draft PR #186, inspect hosted CI, and preserve uncommitted owner release/signing work.

Next implement the memory-budgeted scheduler, persistent hybrid indexes, People accuracy/speed, and local chat; then dense timeline/ASR/tracks, automatic chapters, best takes, video/audio/enhancement/reframing and broad adapters, followed by native port UI and release/hardware acceptance. Rust image decoding/cancellation, complete publication recovery, and cross-filesystem Undo need explicit coverage. Adlon remains strictly read-only; tests and outputs stay internal.

## 2026-10-01 — Next-version implementation in progress

Branch `codex/fileid-next-version` contains Adlon/project-original mutation guards, canonical v21 catalog migrations and IPC v1.1 mirrors, persistent name/evidence FTS, manual chapter editing with durable Undo, macOS frame-sampling jobs with isolated cancellable decoding, and concise naming. See [NEXT_VERSION.md](NEXT_VERSION.md) for the complete accepted delivery ledger, validation commands, and unfinished features. This is not a next-version release.

Next priorities: certify all source/output/companion writes with internal fixtures; finish persistent hybrid indexes and the memory-budgeted scheduler; improve People; implement local chat and unified reversible operations; then add dense timeline/ASR/tracks, automatic chapters, best takes, media tools, broad adapters, and native port UI. Preserve the fast scan and run accuracy/fidelity/hardware gates before promoting models or shipping. The current sampler needs an already loaded visual model, can miss events between samples, and retains incomplete coverage. Windows/Linux have catalog backend contracts, not the new macOS panel or timeline worker.

All tests and generated media in this work use internal temporary directories. Do not modify Adlon or the notarized installed release. Do not overwrite the prior release/signing history below.


## 2026-09-25 — Notarized macOS release built; finish owner acceptance

The installed `/Applications/FileID.app` and `platforms/apple/dist/FileID-v0.1.0.dmg` are Developer ID signed, notarized, and stapled with Team ID `RSDJXF7DXN`. Apple accepted both submissions. The DMG and app inside it pass Gatekeeper; the installed app passes strict signature, staple, and Gatekeeper checks and launches. The full 97 MB `mlx.metallib` is present. The pinned MLX build is compatible with Xcode 27 through `scripts/ensure_mlx_metallib.sh`; Xcode 26.6 is no longer needed. The distributable DMG SHA-256 is `e26face2d4c14c7f0cb9c186bc37f257ab9f5618aecd87a99b117a768eb32f78`.

CLIP, RAM++, SFace, BGE-small, and Qwen3-VL 8B are installed. A one-image scan and Deep Analyze completed successfully with the notarized installed engine and a separate temporary database (`processedFiles=1`, `failedFiles=0`, `processed=1`, `failed=0`). All 218 Swift tests pass with scratch output under `/tmp`. A temporary `.build/debug/FileIDEngine` symlink to the scratch engine binary satisfied the process-level scan cancellation test's binary lookup; recreate it if rerunning tests with `--scratch-path` after clearing `.build`.

Owner acceptance can now use a representative personal library in the UI; the release artifact has passed the native build, test, signing, notarization, staple, Gatekeeper, and installed-engine runtime gates. Do not put the app-specific password in chat or scripts; the `fileid-notary` Keychain profile is stored for future releases.

## 2026-09-24 — Finish macOS distribution signing

The local `/Applications/FileID.app` is Apple Development signed and verified after launch, but Gatekeeper rejects it as expected. The paid membership and Team ID `RSDJXF7DXN` were verified on Apple's portal; a G2 Developer ID Application certificate is installed in the login Keychain, and a timestamped test signature passes strict verification. Xcode's Personal Team label is stale but does not block command-line signing. The owner reported storing `fileid-notary`, but `xcrun notarytool history --keychain-profile fileid-notary` still reports no Keychain item. Confirm the profile name and successful output from `xcrun notarytool store-credentials fileid-notary --apple-id adammnolle@gmail.com --team-id RSDJXF7DXN` (secure prompt; never paste the password into chat).

The release binaries build with Xcode 27, but the pinned MLX Metal sources fail to produce `.build/cache/mlx.metallib` (`steel_gemm_segmented` / `mma.h`) under both Xcode 27 and 27.1 beta. Download/install Xcode 26.6 Apple silicon from Apple's More Downloads page, install its Metal Toolchain, then run `DEVELOPER_DIR=/Applications/Xcode-26.6.app/Contents/Developer bash scripts/ensure_mlx_metallib.sh`. `scripts/release.sh` deliberately refuses to ship without the library. Then run `bash scripts/release.sh v0.1.0`, verify the signed app and engine share a Team ID, and test the stapled DMG with Gatekeeper on a quarantined download. `--skip-notarize` now uses Apple Development when available for local dry runs; the release script keeps the signed app in `/tmp` through packaging to prevent FileProvider xattrs from invalidating it.

## 2026-06-16 — Restructure deep-research sweep: 4 verified wins landed; research-backed roadmap for the rest

A `/deep-research` sweep (27 web sources → 21 verified claims) + a 3-agent codebase audit graded Restructure
against best-in-class. **Encouraging headline: the architecture already matches or beats the documented field**
— density clustering (HDBSCAN family), c-TF-IDF distinctive naming, a journal-backed reversible apply, and a
barycentre Sankey are all SOTA-aligned or ahead. Several audit findings were **false positives caught by
verification** (the image profile IS calibrated; `Path::starts_with` containment is already component-aware;
Windows already surfaces confidence+reason in `DrillDownSheet` and already has the Undo button).

**Landed + verified this session (both engines unless noted; Rust 343 + clippy, macOS build + 226):**
- **Incremental crash-safe undo journal** — `restructure_apply.rs` / `Restructure.swift` now append each
  inverse move to the journal as it happens (+ periodic fsync) instead of once after the loop, so a crash
  mid-apply still leaves every completed move undoable. Directly answers the research's #1 open question
  (the field only ships "best-effort, no crash-safety").
- **Single folder-granularity knob** — `FILEID_RESTRUCTURE_GRANULARITY` ∈ {loose,normal,tight} shifts the
  cluster cosines (HDBSCAN `min_cluster_size` philosophy) so the owner tunes folder count with ONE lever.
  `granularity_delta()` / `granularityDelta()`, lockstep.
- **Empty-dir cleanup on undo** — undo now removes the orphan empty group folders apply created
  (`cleanup_empty_dirs` / `cleanupEmptyDirs`; `remove_dir` is empty-only, root-contained).
- **Confidence + reason surfaced on macOS** — `RestructureView` proposal rows now show the band badge +
  the engine's "why filed here" (Windows already did this in `DrillDownSheet`). The data was always in the
  IPC; macOS `mapProposals` was dropping it.

**Remaining restructure roadmap (priority-ordered), all research-backed:**
- **Surface-tier the main list + "apply only high-confidence"** (both apps, polish) — the badges exist now;
  add list ordering by band + a one-tap "select confident" so the user can apply the sure ones. SOTA: top
  band prominent, low band hidden, never silently auto-file (reconsider the silent `Auto` tier accordingly).
- **Name-based routing signal** (both engines) — ✅ DONE + CI-green. `FolderPrototype.name_tokens` (folder +
  sibling filenames); routing computes an overlap coefficient vs the cluster's filename tokens and upgrades a
  thin-margin content match to Auto when names agree strongly (Dropbox Smart Move). Additive — never overrides
  the calibrated content path. Validated against authored labeled ground-truth scenarios (the assistant acting
  as the domain-expert labeler in lieu of real-data UAT).
- **Learn-from-corrections** (both engines) — ✅ DONE + green. The `restructure_feedback` module
  (`pipeline/restructure_feedback.rs` / `Pipeline/RestructureFeedback.swift`) consumes the v18 table:
  `record(applied_moves, now)` credits each moved file's `filename_tokens` toward its destination folder basename
  (UPSERT +1 weight), wired into the apply loop ALONGSIDE the undo journal (shares the forward-only gate, one
  batched write after the loop); `boost` sums each proposed move's (tokens → dest folder) feedback and upgrades to
  Auto at/above `FEEDBACK_AUTO_WEIGHT = 3` (additive — never re-routes), wired into the plan command on the full
  proposal set before the anchor strip. `filename_tokens` is now `pub(crate)` (Rust). Pinned with labeled parity
  tests on both engines (Rust 3, macOS 3): apply 3 similar files → a new similar file's move upgrades Auto; an
  unrelated move stays Review; weight accumulates. Instance-based, no retraining (SOTA-validated). Last-mile
  fine-tuning to the owner's real library is what this closes vs. the authored-ground-truth validation.
- **Per-bucket approval + before/after tree preview** (both apps, large) — approve per destination bucket, and
  a side-by-side resulting-structure tree as the pre-apply confirmation (R3, design in RESTRUCTURE.md).
- **Granularity Settings slider** — wire the env knob above to a Settings "folder granularity" picker
  (engine reads the env at spawn; the knob itself is done + verifiable today).
- **Deep-Analyze mid-review re-plan fix** (macOS) — re-apply prior deselections by file-id to the fresh plan
  (`RestructureView.swift:48-58`).
- **file_ref stale-move guard** (both, narrow) — the apply stale-check is path-only; add an inode/NTFS-id
  compare so a same-path file swap in the plan→apply window can't move the wrong bytes. Platform-specific;
  low-frequency edge.
- **IPCSink drainer off-actor write** (macOS, deferred — see DECISIONS 2026-06-17) — the drainer's blocking
  `wire.write` holds actor isolation, so a parent that stalls reading fd 2 backpressures `emit()`. Bounded +
  self-healing + cancellation is independent (AtomicBool), so it's NOT a wedge; the clean fix fights Swift 6
  (`FileHandle` non-`Sendable`) and risks frame ordering. Revisit only if a real stall is observed; would need a
  Sendable fd wrapper + a single-writer off-actor queue that preserves order.
- **Owner threshold calibration** — non-image thresholds are explicit placeholders; tune on the real library
  (now via the single granularity knob). Needs the owner's hardware.
- **macOS CI green — the "non-image clustering anomaly" was a TEST bug, not an engine bug (RESOLVED).**
  On-runner diagnostics proved the engine is correct on the GitHub runner: the lone no-signal file IS excluded by
  `nonImageSignatures`' integer-frequency guard before clustering, and the resulting `moves` provably did NOT
  contain it. The failure was the test's `#expect(!moves.contains { $0.fileID == 999 })` — a negated
  trailing-closure that the runner's Xcode 16 swift-testing macro mis-evaluated (reported the id present when it
  wasn't); it does not reproduce on local Xcode 26.5. Fixed by materializing the ids once and asserting with
  closure-free `contains(Element)` (`RestructureSemanticTests.swift`). The engine determinism hardening landed
  anyway (singleton pre-exclusion + `k = min(k_nn, n-1)` clamp, both engines) and `macos.yml` now caches only the
  SwiftPM dependency download cache, not build products (a separate stale-object hazard). All three CI workflows
  are green on HEAD.

## 2026-06-16 — Whole-codebase audit done; Windows undo button landed; only cosmetic P2s + R3 remain

A 6-agent adversarial audit swept the whole codebase. Security + ML + data-integrity slices are
clean; the real bugs (Swift UPSERT metadata clobber, Restructure apply/undo wedge, Rust path_search
NFC ×3, tag-drain, engine-exit progress/auto-pilot reset, Cleanup double-tap) are fixed + verified
(macOS 222, Rust 343 + clippy). The Windows "Undo last run" button is implemented (CI-pending).

**Tracked cosmetic / low-priority P2s (no user impact — fix opportunistically):**
- **C# (unverifiable without a local WinUI build, so left for a Windows session):** drop the dead
  `"stages_skipped_missing_models"` arm in `EngineClient.Commands.cs IsNonFatalWarningKind` (no engine
  emits it); `PeopleView.xaml.cs` drag/drop `g.Tag is long` never matches the boxed `int` ClusterId
  (works via the DataContext fallback) — change to `is int` or bind a `long`; `ClipSearchService.Dispose`
  insert race is self-limiting (5 s timeout) — optional `_disposed` recheck.
- **Swift:** `ScanCoordinator.bumpProcessed(by:)` loops single bumps (perf, on-actor, no race) — one
  `+= n` would do.
- **Deferred by design:** the `face_verifications` v17 verdict key can't match cross-platform (macOS
  CSV vs Windows JSON bbox); macOS has no verdict-write path and bbox parity (#55) was read-tolerance
  only. No fix unless full bbox round-trip parity becomes a goal.

**Still open:** R3 restructure polish (per-bucket approval, before/after tree, confidence calibration);
owner threshold calibration of the non-image pass on a real library.

## 2026-06-16 — Restructure R1 validated + R2 undo landed; only the Windows undo button + R3 remain

R1 is validated on the owner's real library (and tuned — stopwords + junk-prefix detection). R2's
**one-click "Undo last run" is done** end-to-end on macOS (engine round-trip-tested, IPC, app
button) and across the whole engine/IPC contract (Rust `undo_last` + dispatch + C# command/VM).
**VLM cluster naming is cut** — real-data c-TF-IDF names ("Nolle Resume", "Copyright Limited") are
already good, so it's not worth the slow background VLM pass for v1.

**Remaining restructure work:**
- **Windows app "Undo" button (CI parity, small):** the C# `EngineClient.CanUndoRestructure` flag +
  `UndoRestructureAsync` are in place; add the XAML button in `Views/Restructure/RestructureView.xaml`
  bound to `CanUndoRestructure` → `UndoRestructureAsync(libraryRoot)`. (macOS already has it.)
- **R3 polish:** per-destination-bucket approval + before/after tree preview; fix Deep-Analyze
  re-planning mid-review + skipped-outcomes reset (`RestructureView.swift:48-58`); measured
  confidence-band calibration; optional BGE dedicated document embedder (the no-new-model CLIP-text
  reuse is the cheaper interim) for higher-ceiling document clustering.

## 2026-06-16 — Restructure overhaul: R1 landed (both engines, verified); R2 + owner calibration next

**R1 DONE + verified** (macOS swift test 218/218, Rust cargo test 343/343 + clippy clean): the
butler now clusters **all file types**, not just photos. Non-image files (docs/video/audio/
downloads) group by a filename-token + tag bag-of-words signature via an additive pass with its
own `nonImageProfile`; the image path is byte-identical and cannot regress. Junk folders
(Downloads/Desktop/Temp…) are barred as prototypes. R3-07B (64 MiB IPC cap + macOS newline
resume) confirmed already-landed — whole-library plans render.

**R1 owner UAT + calibration (the real proof — needs your Mac + a real library):** copy a messy
folder to a scratch path, `bash run.sh`, scan it, open Restructure. Confirm documents/downloads/
videos now land in **content-based groups, not one `Documents/<Year>` dump**. The non-image
thresholds are provisional — tune against what you see, no recompile:
- Over-grouping (unrelated files merged)? raise `FILEID_RESTRUCTURE_NI_AUTO_COH` (0.70) /
  `FILEID_RESTRUCTURE_NI_REVIEW_COH` (0.55) and re-plan.
- Under-grouping (too many singletons left in place)? lower them.
- Wrong files pulled into an existing folder? raise `FILEID_RESTRUCTURE_NI_FOLDER_COS` (0.60) /
  `FILEID_RESTRUCTURE_NI_AUTO_FOLDER_COS` (0.80).
- Kill-switch: `FILEID_RESTRUCTURE_NONIMAGE=0`. Report the sweet-spot values → bake as defaults
  (one line each in `restructure_semantic.{rs,swift}`).

**R2 — butler-quality naming + reversibility (next build, both engines/app):**
1. **VLM cluster naming** (design P2): reuse the installed Deep-Analyze VLM; per cluster feed the
   already-computed profile (distinctive c-TF-IDF terms + 3-5 representatives + EXIF/GPS), a
   constrained label-then-reason call (`{name,reason}`, 2-4 word Title-Case, forbid generic),
   cache by representative-ID hash, run as a deferred background job. Destination adopts the VLM
   name; no IPC schema change. *Acceptance:* new groups get semantic names ("Golden Retriever",
   not "Dog Golden") on an installed VLM. (Needs on-Mac validation — not headless-verifiable.)
2. **Command-journal undo** (design P3): the apply loop already does safe per-file moves +
   recovery sidecar; add an inverse-move journal per run + an **"Undo last run"** action (one
   macro, compensate on partial failure) in `RestructureApplyBar`; drop the "not reversible"
   copy. Cross-platform (Swift + Rust + C# + an undo IPC command). *Acceptance:* a completed run
   fully reverses in-app.

**R3 — finish-line polish (after R1 calibration + R2):** per-destination-bucket approval +
before/after tree preview (design P4); fix Deep-Analyze re-planning mid-review + skipped-outcomes
reset (`RestructureView.swift:48-58`); measured confidence-band calibration; optional **BGE
dedicated document embedder** (new model + migration + re-scan) for max document clustering —
the no-new-model CLIP-text reuse is the cheaper interim, also deferred.

## 2026-06-16 — RAM++ first-run modal + Welcome re-show parity landed (macOS verified, Windows CI-pending)

DONE (macOS verified, `swift build` + 216/216): RAM++ is now a gating row in the macOS
WelcomeSheet ("Install all" pulls it; re-show gate includes it). Re-show gate on BOTH
platforms is now the core sub-1 GB trio (CLIP+RAM+++face); the VLM no longer re-nags.

**Confirm on CI:** the Windows change is C#-only (`ModelInstallerService.CoreModelsInstalled`
+ `MaybeShowWelcomeSheetAsync` switch) — `dotnet build`/`test`/`format` must go green on the
app workflow before merge. No local C# toolchain here.

**Mac UAT (to actually see the new row as "missing"):** `run.sh` preserves model weights, so
first `rm -rf "$HOME/Library/Application Support/FileID/Models/ram_plus"`, then `bash run.sh`.
Expect 4 rows → "Install all" downloads RAM++ → sheet auto-dismisses ~800 ms after all four
land → relaunch does NOT re-pop once CLIP+RAM+++face are present (even with the VLM skipped).

**Audit cleared (no work):** Deep-Analyze VLM already family+tier-aligned across platforms;
CLIP byte-identical (same SHA-256s). See DECISIONS.md 2026-06-16.

**Offered, not yet run — full 6-tab cross-platform parity sweep.** This pass covered only the
onboarding / model-download surface. A systematic Library · People · Cleanup · Deep Analyze ·
Restructure · Settings Win↔Mac behavior diff is the remaining "parity in general" ask — run on
owner's word (parallel per-tab agents → drift table).

## 2026-06-15 (lockstep) — macOS model-stack lockstep COMPLETE; remaining work is UAT + release

DONE + merged green: IPC cap (R3-07B #43), RAM++ tagger (engine #44 + install UI #46),
VLM tags (#45), **R3-15 durable face-verification keys (#48)**, delta-re-audit follow-ups
(#49), **5-point FaceAlign (#51, ON by default #54)**, **face-detection recall (#53,
512→1536 px)**, and **bbox cross-platform parity (#55, macOS read-tolerance via FaceBBox)**.
All build-verified + delta-audited + unit-tested. **No lockstep code remains.**

**Mac validation of the merged lockstep (the remaining work is mostly THIS, not code):**
1. Install RAM++: Settings → AI Models → "image tagging" → Install (~925 MB). Scan a
   folder; confirm Library tags are richer + `tags.score` populated (NULL only for
   EXIF/derived). Deep Analyze a few files; confirm `source='vlm'` tags appear.
2. Faces (FaceAlign now default-on + 1536 px detection): scan, open People — expect more
   faces found + fewer different-people merges. The `face_align_applied` log shows
   detected/aligned/fallback per image.
3. **Face threshold calibration (in progress, owner-driven, env knobs, no recompile):**
   if different people still merge, raise `FILEID_FACE_PASS1_COS` (0.66) / `FILEID_FACE_TIGHT_COS`
   (0.65) / `FILEID_FACE_SMALL_COS` (0.55) and wipe+rescan; if faces still missed, raise
   `FILEID_SCAN_MAX_PIXELS` (1536). When the owner reports the sweet-spot values, bake them
   as the new defaults (one-line each). Disable alignment for an A/B with `FILEID_FACE_ALIGN=0`.

**bbox parity — DONE (#55), macOS-side read-tolerance.** `FaceBBox.parseNormalized`
parses both macOS CSV (normalized, bottom-left) and Windows JSON (pixels, top-left) →
normalized bottom-left, used by the 3 macOS bbox readers. Windows needed no change (it
never reads bbox back for cropping — saves face-crop JPEGs at scan + clusters from
embeddings). CSV path byte-identical; FaceBBoxTests cover the conversion. Only matters
cross-platform (Windows-scanned library opened on a Mac); single-platform unaffected.

**Face-threshold calibration (optional, owner-driven, env knobs, no recompile):** if
different people still merge after a fresh scan, raise `FILEID_FACE_PASS1_COS` (0.66) /
`FILEID_FACE_TIGHT_COS` (0.65) / `FILEID_FACE_SMALL_COS` (0.55) + wipe+rescan; if faces
still missed, raise `FILEID_SCAN_MAX_PIXELS` (1536); `FILEID_FACE_ALIGN=0` for an A/B.
Report the sweet-spot values → bake as defaults (one line each).

## 2026-06-14 (round 7) — audit CONVERGED; the only code-side work left is the two deferred coordinated changes below

Round 7 swept the last untouched tier — the action-bearing C# Views + ViewModels and the remaining
Swift Views — and landed 39 fixes + 4 delta regressions (PRs #40/#41, see STATE.md), with delta-2
coming back dry. Across 7 find-rounds the method has now covered the **engine, data paths, and the
full UI surface on both platforms**; each round still found real bugs (8/20/12/11/7/39), and the
deepest UI round was almost all P2/P3 (zero P0/P1) — the high-severity space is exhausted. **Treat
the find→verify→expert→read→delta audit as converged.**

What remains is NOT findable by more static rounds — it needs hardware, a GUI, or labeled data:
- **macOS:** `bash run.sh` on a Mac; exercise every tab (tag/dedupe/restructure/people/cleanup/deep
  analyze) against a real library; confirm the round-5/6/7 UI fixes behave (bulk-merge in-flight
  guard, suggestions-sheet-no-force-open, preview/thumbnail `.task(id:)` re-fire, CLIP install/cancel,
  pipeline-strip floor on wipe-vs-clear). Schema v13 face-verification anchors still need a Mac to
  exercise the write path.
- **Windows:** run `iterate.ps1 -Corpus <path>` on the current RTX 5080 against an available copied corpus; confirm the WinUI-runtime fixes (the 3
  ViewModel generation-guards, the bulk re-entrancy guards, the thumbnail-CTS single-owner dispose,
  DeepAnalyze once-per-FileDone + cancel-stops-batch, the off-UI-thread IPC encode + wipe deletes,
  the O(N²)→O(N) selection coalesce, the pipeline-strip ReseedFloorFromDb). Per-vendor NPU (AMD/Intel/
  Snapdragon) + ARM64 hardware passes.
- **Labeled data:** ML threshold tuning (face-cluster precision/recall, RAM++ per-class thresholds) —
  tune against real corpora, never by guess.

The two coordinated cross-platform changes below remain the ONLY open code-side items — both still
gated on a both-engine append-only migration / four-constant cross-platform bump that CI can't fully
exercise. Everything else from rounds 1–7 is landed and green on `main`.

## 2026-06-14 (round-3 audit) — two findings deferred with full recipes (the rest landed in #25–#27)

The round-3 audit (see STATE.md) landed 20 fixes. Two were deliberately NOT landed because they
exceed what can be verified/landed safely in one pass — both have a complete recipe here.

- **R3-15 — "different people" verdict lost after a face-print id churn (P2 data-loss, BOTH engines).**
  `face_verifications.face_a/face_b` (the v13 "anchors") are face_print ids, which are DELETE+re-INSERTed
  with fresh AUTOINCREMENT ids on every `faces_evaluated` re-scan (Windows `dbwriter.rs` ~405–457;
  macOS DBWriter mirror). After a churn the stored ids no longer resolve in `cluster_of`, so the
  auto-merge "block these two" guard silently no-ops and two user-confirmed-different lookalikes can
  re-merge. **Why deferred:** the only churn-stable face identity is a stored `(file_id, bbox)` (or
  embedding) key → a NEW append-only migration (`vN+1`) that MUST be registered with the IDENTICAL
  name on BOTH engines (the code's own C12 fork-bug test, `migrations.rs` ~511, shows divergent v14
  names corrupted cross-platform libraries) AND verified on the Windows runtime (CI can't exercise the
  ORT/SQLite write path). **Recipe:** (1) `vN+1_face_verification_stable_keys` on both engines: add
  `file_a TEXT, bbox_a TEXT, file_b TEXT, bbox_b TEXT` to `face_verifications` (or a side table keyed
  by content). (2) On verdict WRITE (Windows `bulk.rs handle_mark_persons_different`; macOS
  markPersonsDifferent) populate the stable keys from the faces' current `(file_id, bbox)`. (3) On
  verdict APPLY (Windows `commands/face_clustering.rs` ~92/221; macOS `tightPairAutoMerge` verdict
  projection) resolve via the stable keys → current face ids → `cluster_of`, falling back to the
  legacy face_a/face_b for un-migrated rows. (4) Backfill once on migrate where the old ids still
  resolve. Acceptance: a verdict survives a full re-scan (id churn) and still blocks the merge — add a
  both-engine regression test that re-scans between verdict and clustering.

- **R3-07 PART B — inbound IPC frame cap 32→64 MiB + O(n²) buffer-scan (P2, both apps).** The 32 MiB
  cap drops a whole-library restructure plan above ~100k moves; PART A (landed in #27) made the drop
  graceful + actionable, but a ~200k-move target needs the bump. **Why deferred:** raising the cap
  without fixing the full-buffer newline rescan makes the accumulating-buffer scan quadratic, and the
  "matches the Windows app's inbound cap" symmetry means changing macOS `EngineClient.maxFrameBytes`
  (302), Windows `EngineClient.cs` MaxFrameChars (64) + `EngineClient.Commands.cs` MaxIpcFrameBytes
  (25) + engine `sink.rs` MAX_FRAME_BYTES (28) / `main.rs` (50) together. **Recipe:** bump all four
  constants to 64 MiB; gate the macOS newline scan so it resumes from the last search index instead of
  re-scanning the whole buffer each readability tick.
  **Round-5 update (R5-12 ≡ this):** re-confirmed in round 5, and verified it degrades GRACEFULLY both
  ways — the Windows engine `sink.rs` write_line ALSO caps OUTBOUND and substitutes a structured
  `ipc_frame_too_large` error (so an over-cap plan is never silently dropped on the wire, on either
  platform), and the Windows C# scan is already O(n) (the `StdoutFraming.Scanned` resume field). So the
  only remaining work is the 64 MiB bump (4 constants) + the macOS newline-scan resume. Also fix the
  now-false comment at `EngineClient.cs:60` ("outbound plan events are unbounded on the wire" — they're
  capped by sink.rs).

## 2026-06-14 (newest) — every actionable engine/app item is landed; what remains needs HARDWARE, a GUI, or LABELED DATA

After the 2026-06-14 push (PRs #16–#20, all merged + CI-green on `main`), there are **no open
code-actionable items left** — every remaining entry below is now in a terminal state:

- **DONE this session** (verified by build/test/on-hardware): cross-platform restructure-apply
  cancellation (#16), the macOS test-gate restoration + flaky-CI fix (#16/#18), `ReadStore` lock (#18),
  R-07 clustering shutdown signal (#18), restructure per-move `tier` + honest apply bar (#19), Windows
  `hardwareReprobed` actually-bound EP (#20), and Deep Analyze end-to-end (single/folder/corrupt-input)
  once Xcode was installed.
- **BLOCKED ON A SPECIFIC RESOURCE** (cannot be done correctly without it — recipe in each entry):
  - *Labeled data:* **F-4** face-clustering mutual-kNN structure (blind = clustering regression).
  - *Owner's Mac / GUI:* CLIP semantic **search** (app-side text encoder), the Tidy/Keep tile + apply-bar
    *visual* render, `walkStreaming` progress-UX, restructure-apply UAT on a real copy library.
  - *RTX 2060 (+ AMD/Intel/Snapdragon):* per-vendor GPU throughput + `hardwareReprobed` EP-match.
- **DELIBERATELY DEFERRED** (shipping blind would regress a working path — rationale in each entry):
  `walkStreaming` activation, **R-11** ModelLoadGate continuation, the in-process rewrite of the
  process-spawning cancel test, and the intentional per-run face-embedding backfill cap.

Detail for each is in the dated sections below. Nothing here is a vague TODO; each is "done", "blocked
on X — here's how", or "deferred because Y".

---

## 2026-06-13 (later) — macOS adaptive hardware scaling landed; per-chip tuning is a hardware-UAT knob (branch `perf/adaptive-scaling-2026-06-13`)

The macOS engine now scales its Vision/ANE concurrency and DB batch size with the
machine instead of using M1-Pro-shaped constants (commit `f80d768`). All three
changes reduce to the prior constants on the 16 GB M1 Pro baseline (gate 14,
batch 100), so this is a **no-op on the verified tier** and pure headroom on
higher-core / higher-RAM Macs — but the headroom itself is unmeasured on this dev
box (M1 Pro is already CPU-maxed at ~88% avg during a scan; the scale-up only
manifests on bigger silicon). The tuning below needs an M-Pro-Max / M-Ultra /
Mac-Pro to validate.

**What landed:**
- `VisionWorker.visionConcurrencyGate` = `Hardware.workerCap` (was hardcoded 14).
- `Hardware.MemoryTier` — low `<12 GB` / balanced `12–48 GB` / high `≥48 GB`.
- `DBWriter.maxBatchFiles` — low 64 / balanced 100 / high 500.

**Per-chip tuning recipe (run on the target Mac, read-only corpus):**
1. **Baseline the box.** Drive a full-corpus scan via `platforms/apple/scripts/iterate.sh`
   (or a FIFO+stderr driver) and capture from the `scanComplete` event:
   `processedFiles / totalSeconds` = throughput; sample `ps -o %cpu,rss` of the
   engine PID at ~0.5 s cadence for avg/peak CPU% (logical-core-count × 100 = full)
   and peak RSS. M1 Pro reference: ~62 files/s, avg CPU ~875% of 1000% (CPU-bound),
   peak RSS ~855 MB on the 9,745-file Bernadine/CD set.
2. **Vision/ANE gate.** If avg CPU is **not** near its ceiling and throughput is
   below the M1-Pro files/s-per-P-core ratio, the Vision stage is under-fed. The
   gate now opens to `workerCap` (M-Ultra → 32); if the ANE still starves, the
   limiter is the **ANE inference semaphore** `MobileCLIPService.inferenceSem`
   (currently `4`) — raise to 6–8 on chips with a 2× ANE (M-Max/Ultra) and
   re-measure; back off if throughput drops or RSS spikes.
3. **Worker cap.** `Hardware.computeWorkerCap` = `P + E + max(1, P/2)` capped at 32.
   On a Mac Pro (many P-cores) the 32 cap can bind — but raising it is
   **storage-bound, not CPU-bound**: iteration-8 data showed workers 14→18 spiked
   NAS read latency 2 ms→1510 ms. Only raise the cap when the corpus is on a fast
   **local** SSD; on a NAS/network share keep it. Re-baseline read latency after
   any bump.
4. **DB batch / memory tier.** On `≥48 GB` boxes the writer batches 500 files;
   confirm peak RSS stays well under physical RAM (the decoupled committer holds
   one batch of `TaggedFile`s — embeddings included — in memory). If RSS is
   comfortable, the high tier can go higher (mirror Windows' upper bound); if a
   low-RAM box (`<12 GB`) shows GC/swap pressure, drop low-tier below 64.
   *Acceptance for the whole recipe:* on a bigger-than-M1-Pro Mac, throughput
   exceeds the M1 Pro's files/s-per-P-core ratio with no OOM, no NAS read-latency
   regression, and avg CPU near its ceiling during the tagging phase.

Note: the truly dynamic piece (runtime memory-pressure adaptation, F-3) is still
spec-only — these are **static at startup** tiers (matching the worker-cap model).
F-3 (`MemoryPressureMonitor` driving worker admission + PDF pixel ceiling + batch
target with fast-down/slow-up hysteresis) remains the follow-on.

---

## 2026-06-13 (newest) — audit-2026-06-10 campaign: macOS apply UAT + deferred items are the only remaining gates (RESUME HERE)

The `fix/audit-2026-06-10` campaign is closed on the branch (see STATE.md 2026-06-13): 131 findings
+ 2 criticals fixed, 22 self-introduced regressions caught, all local gates green, all three CI
workflows green, and a clean **read-only** on-hardware macOS scan of the 62,746-file `Adlon/TrueNAS`
corpus. What remains are the write-path and threshold items that need the owner's Mac or labeled data.

**Mac-UAT (owner's Mac, on an isolated/throwaway library — these touch the move path):**
- ~~**Restructure apply/move on-Mac**~~ — **DONE 2026-06-13 (on-hardware, isolated sandbox).** The real
  engine applied crafted moves against a throwaway DB + synthetic `/tmp` library: **D-7 auto-rename**
  produces `name (2).ext`, the in-batch `claimed`∪lstat collision check fires, the **B4 stale-plan
  guard** fails a move whose live `path_text` no longer matches (file untouched), **SEC-7** rejects a
  `../` escape, and **`path_hash`** (not just `path_text`/`path_search`) is refreshed per move. Found +
  fixed a containment bug en route (PR #13, `b927031`: `/private`-resolved roots wrongly rejected valid
  in-root moves). Driver: `/tmp/fileid-apply-test.sh`. STILL OPEN: the privileged-denial path
  (permission error → `privilegeError` populated) needs a real permission-denied destination — low
  priority. Any real-corpus apply must run on an isolated COPY, never the live corpus.
- ~~**Restructure apply-bar relabel + Tidy/Keep tier split.**~~ **DONE 2026-06-14 (PR #19).** The
  engine `RestructurePlan` now populates per-move `tier` (Anchor/Mixed/Junk) + `folderClassifications`
  from `Restructure.classifyFolders`, so the app shows engine-authoritative Tidy/Keep tiles instead of
  its local-heuristic fallback. The vestigial two-step "apply as shortcuts → convert" apply bar (both
  buttons routed to the SAME engine real-move confirmation — its "reversible/shortcuts" copy mislabeled
  an irreversible move) is collapsed to one honest "Apply moves" button. The Tidy/Keep tile *visual*
  rendering is GUI and confirmed only by inspection; verify on the owner's Mac.
- **F-C6-005 `walkStreaming` activation — DEFERRED (engine ready + tested; UX-coupled).** The streaming
  walk is implemented and parity-tested (`streamingMatchesWalk`), but activating it interleaves
  discovery with tagging, which REORDERS the IPC contract the app's progress UI depends on
  (`discoveryComplete`/total now arrive after tagging starts). The engine side is verifiable headlessly
  (file-count parity + no deadlock via a FIFO driver), but the progress-UX behavior needs a GUI session
  to confirm, and the benefit is marginal (~12 MB RSS — the file list isn't the RSS driver — plus a few
  seconds' earlier start). Not worth shipping a possibly-confusing progress UI blind. Activate + verify
  the phase/total UX on the owner's Mac.
- ~~**`ReadStore.refreshCounters` NSLock-in-async.**~~ **DONE 2026-06-14 (PR #18).** The background
  counters worker now uses a scoped `withLock {}` (synchronous critical section) instead of bare
  `lock()`/`unlock()` in an async closure — the Swift-6 "lock unavailable in async context" diagnostic
  is gone; `swift build` clean.
- ~~**Deep Analyze (MLX)**~~ — **DONE 2026-06-14 (on-hardware, Xcode 26.5 installed).** With the cached
  `mlx.metallib` next to the engine + Qwen3-VL 4B downloaded, `deepAnalyzeFile` and `deepAnalyzeFolder`
  produce accurate captions + smart-renames offline (~8 s/image after a ~30 s load), persist
  `vlm_*` columns, and gracefully skip a corrupt non-image (no crash). See the memory
  `onhardware-engine-testing` for the exact setup. STILL untested headlessly: face crops + **CLIP
  semantic SEARCH** (text→image) — the text encoder is app-side (`CLIPTextEncoder`); the engine returns
  `not_implemented` for `embedTextQuery`, so search needs a GUI session.
- **`ScanCancellationTests` is skipped on the GitHub macOS runner** (`.enabled(if: GITHUB_ACTIONS==nil)`):
  the process-spawning test leaks the engine child and wedges the swift-testing harness to the 12-min
  SIGALRM there (does not reproduce on a dev Mac; passes <2 s; collector/GCD-watchdog/stdout-drain
  hardening didn't make it deterministic on CI). The cancel WIRING is covered on CI by
  `requestCancelCancelsRestructureTask`. *Fix:* rewrite it to drive the scan pipeline IN-PROCESS (test
  sink + temp DB, no `Process`) so the C1 cancel-deadlock regression runs on CI too. *Acceptance:* the
  cancel-deadlock test runs and passes on the GitHub runner with no SIGALRM.
- **R-11 (DeepAnalyze ModelLoadGate JobQueue wedge) — DEFERRED (LOW harm vs. deadlock risk).** When a
  same-model prewarm is *joined* to a run's single-flight model load and the run is cancelled, the run's
  JobQueue lane stays wedged until the joined download finishes — **self-resolving, NOT a permanent
  hang**. The correct fix (a per-waiter `CheckedContinuation` to free this waiter's lane immediately,
  since `await task.value` isn't interruptible) is an intricate change to the model-load path where a
  mistake introduces a REAL deadlock — strictly worse than the current self-resolving wait — and the
  exact join/cancel race can't be reproduced deterministically without a live multi-GB MLX download.
  Not shipping an unverifiable concurrency change to a working path for a self-resolving LOW issue.
  Approach is captured here; do it with the real concurrency on the owner's Mac.
- ~~**R-07 (face clustering ignores a fresh shutdown after a cancelled scan).**~~ **DONE 2026-06-14
  (PR #18).** Added a dedicated shutdown mirror on `ScanCoordinator` (set only by `.shutdown` via
  `requestShutdown()`, distinct from `.cancelScan`'s `requestCancel()`); `clusterShouldCancel` now
  aborts on a genuine shutdown regardless of the sticky scan-cancel baseline. Unit-tested
  (`clusterShouldCancelSemantics`).

**Hardware-UAT (thresholds / GPU / runtime — need labeled data or specific hardware):**
- **F-4 face-clustering pass-1 structural change (mutual-kNN / density-gated) — BLOCKED ON LABELED
  DATA.** Cannot be done safely blind: mutual-kNN/density-gating is more conservative than the current
  single-linkage, so without a hand-labeled subset to tune the bands it would OVER-split genuine
  identities (regressing the People tab, which already over-splits — 853 persons / 5000 faces on the
  test set). Shipping uncalibrated would make clustering worse, not better. *Unblock:* hand-label a
  `G:\TrueNAS` (or Adlon) subset, sweep `pass1_cosine` + the mutual-kNN/density bands, pick the F1
  optimum, THEN land the structure with those constants. *Acceptance:* on the labeled subset, no
  cross-identity merges and fewer spurious singletons than single-linkage at equal recall.
- ~~**F-C5-005 engine half — `hardwareReprobed` accuracy.**~~ **DONE 2026-06-14 (PR #20).** The reprobe
  now reports the memoized `active_provider()` (the EP the process actually bound) for
  `execution_provider`, not a fresh capability probe — so after a pack install the card shows
  "pack ✓ — restart to use it" rather than a misleading active EP. clippy + 338 tests green.
  *Remaining UAT (hardware):* on the RTX 2060, confirm the reprobed EP matches `engine.jsonl`.
- **People-tab face-embedding backfill is incremental (UX papercut, by design).** Verified on-hardware
  2026-06-13: each clustering run backfills exactly `maxExtractionsPerRun` (5000) ArcFace/SFace
  embeddings then clusters, so a large library (the test DB has 46,079 detected faces) needs ~10
  clustering passes before People shows everyone. This is deliberate (memory + responsiveness caps) and
  already noted in `FaceClustering.swift` as "tracked separately", but for a polished People tab,
  consider either (a) auto-continuing extraction across runs until `face_print_no_pending` with a
  visible "embedding faces N/M" progress, or (b) surfacing "X faces pending — run again" in the UI so
  the incremental behavior isn't mistaken for missing faces. *Acceptance:* a user with a 40k+-face
  library either sees all faces after one People visit, or is clearly told more passes remain.
- **Per-vendor Windows GPU UAT** (carried from before): RTX 2060 throughput re-baseline; DirectML +
  CUDA paths. *Recipe:* `platforms/windows/build/iterate.ps1` full corpus + `scan_assertions.py`.

**After UAT:** merge `fix/audit-2026-06-10` to `main`, confirm CI green there. (The
`fix/bug-audit-sweep` UAT checklist below remains valid for that branch's separate items.)

---

## 2026-06-10 — Campaign closed; CI + hardware UAT are the only remaining gates

Branch `fix/bug-audit-sweep` holds the full production-readiness campaign (see STATE.md
2026-06-10). All local gates are green; the branch is pushed and **CI must be green on all
three workflows** (macos / windows-engine / windows-app — macos.yml runs the swift-testing
suites this dev env can't compile) before merge to `main`.

**Hardware UAT — macOS (owner's Mac):**
- Scan a large folder; pause → resume → cancel (no hang; cancelled scan still emits
  `scanComplete` and the UI returns to idle).
- Re-scan a clustered folder — People names + manual assignments survive (H2 UPSERT on a
  real DB).
- Restructure: pick root → plan → convert to real moves on a root containing a SYMLINKED
  folder (e.g. `root/Photos → /Volumes/External/Photos`): files must NOT escape the root
  (skip+conflict), DB paths update per-move, generated folder names are Windows-safe.
- Deep Analyze: full run **offline** with a previously-installed VLM (sentinel-gated load —
  no network round-trip); double-click Analyze mid-run → benign bounce, no wedge; engine
  force-kill mid-run → UI flags reset on respawn.
- Search: `"`/`*`/`:` metacharacters return rows; an NFD-named file (e.g. `Café.jpg` typed
  composed) is found by NFC query (v16_path_search).
- Bulk tag + bulk rename + both undo paths; Spotlight `tag:` search; wipe library →
  Spotlight results for wiped files are gone (C16).
- Quit the app mid-VLM-download, relaunch, re-install: stale `.fileid-staging` parts swept
  (C4); kill engine mid-scan twice → respawn works, third crash within window surfaces the
  crashed banner.
- `bash scripts/iterate.sh` green (wire purity U4 + C1 cancel regression).
- Release: install Developer ID cert + `xcrun notarytool store-credentials fileid-notary`,
  then `bash scripts/release.sh v1.0.0` (full sign + notarize + staple) and Gatekeeper-test
  the DMG on a quarantined download.

**Hardware UAT — Windows (RTX 2060 / G:\TrueNAS):**
- `build\iterate.ps1` full corpus pass + scan_assertions.
- Cancel a scan mid-tagging: phase shows Cancelled (not Completed), no auto-clustering fires,
  `scanComplete` arrives with totalFiles=discovered > processedFiles.
- Sidebar queue list NOW POPULATES (queueState was never emitted before this branch): start a
  scan, queue a Deep Analyze — both appear; entries clear on completion/cancel/crash.
- Cache-cold video-heavy folder scroll: extracts serialized (2 permits), no engine OOM, tiles
  fill in (C5).
- 45 MP photo library scan on the Low-tier (4 GB) profile: engine commit stays bounded
  (byte-weighted predecode budget, C6).
- A >256 KiB non-ASCII document (e.g. large Cyrillic .txt) scans without an engine panic (C14).
- Per-row model Cancel during engine cold start sticks (C9); sentinel-write failure surfaces
  an error not a stuck 99.9% slot (C10); OpenVINO-unavailable resets the Accelerator slot (C11).
- People tab: cluster counts exclude tiny/profile faces (excluded flag now populated — expect
  somewhat CLEANER clusters than before on re-cluster; re-run clustering twice → identical
  results).
- Cross-platform DB round-trip: scan a small library on macOS, open the same DB on Windows
  (and reverse) — both engines accept it (v1–v16 parity); a DB from a NEWER build is refused
  with `db_newer_than_engine`.
- TLS pin failure UX via mitmproxy (`download_tls_pin_failed` surfaces; escape hatch works);
  corrupt a model file on disk → reinstall flow recovers (checksum mismatch).

**After UAT:** merge to `main`, confirm CI green there, then the only remaining v1.0 items are
the non-campaign ones in SHIP.md (Restructure P2–P4, Windows EV signing/MSI, per-vendor matrix,
face-clustering structural fix, rename-heal duplicate fix, throughput re-baseline).

---

## 2026-06-09 (superseded by the entry above) — Verify the bug-audit sweep on hardware (branch `fix/bug-audit-sweep`)

72/73 confirmed + 3/4 uncertain audit findings are fixed on the branch, now merged with main's
2026-05-26→06-04 line (commercial-clean YuNet+SFace / OpenCLIP / RAM++ stack, butler restructure,
GPU Performance Packs, main's own audit sweeps) — fixes re-applied onto the replacement code
where a subsystem changed. **macOS `swift build` is green; everything else needs hardware
verification.** Before merging:

**Build gates**
- macOS: `cd platforms/apple && DEVELOPER_DIR=<Xcode> swift test` (needs Xcode; this env had only
  CommandLineTools so the swift-testing suite never ran). Then `bash run.sh`.
- Windows: `pwsh build/build.ps1 -RunTests`; `cargo clippy --all-targets -- -D warnings && cargo test`
  (engine); `dotnet test && dotnet format --verify-no-changes` (app); `pwsh build/iterate.ps1`.

**Add regression tests (designed but not written — this env can't compile them):**
- macOS: cancel-during-tagging emits `scanComplete(.cancelled)` and the JobQueue advances (C1);
  re-scan preserves `face_prints.person_id` + `files.id` (H2); IPCSink full-buffer coalescing
  keeps a buffered `scanComplete` (H3); FTS5 MATCH with metacharacters returns rows (M3).
- Windows: `planRestructure` SQL prepares against bundled SQLite (H10, cargo); restructure
  collision disambiguation (C2, cargo — added); `is_safe_filename` rejects `:` (M26, cargo —
  added); OCR returns text from a known-text PNG via spawn_blocking (H9 — main's six-workflow
  sweep fixed the same missing COM init; confirm the merged path keeps both the init and the
  spawn_blocking timeout).

**UAT (manual):**
- macOS: scan large folder; pause→resume→cancel (no hang, Mac can sleep); re-scan a clustered
  folder (People names survive); Restructure skip→regenerate→apply (skipped files not moved);
  Deep Analyze completes; search with `"`/`*`/`:` returns results.
- Windows: Deep Analyze completes (H8); Restructure plan renders (H10) + apply never overwrites,
  colliding names get " (2)" (C2); OCR-derived search works (H9); settings persist across
  relaunch (H12); offline launch shows the window immediately (H11/U2); a 2nd Windows user can
  launch (M16).

**Data migrations to run after deploy:** H2 (UPSERT) changes re-scan semantics — validate on a
real macOS DB. (H7's "recompute Windows `arcface_embeddings`" is MOOT post-merge: main replaced
Windows ArcFace with YuNet+SFace — `models/arcface.rs` is gone and the v12 migration already
rebuilt the embedding store. Re-verify channel order in the `sface.rs` preprocessing instead.)

**Behavioral changes to confirm:** L24 (discovery no longer prunes generic `build`/`bin`/`obj`
folders unless a build-marker is present — could now scan dev trees); the WinVerifyTrust
cache-only revocation trade-off (DECISIONS.md).

**Still open (deliberately deferred):**
- ~~L1 — IPC ID-casing drift~~ **RESOLVED by main's PR #6** (`9f1ac52` aligned the capital-`ID`
  casing across Rust+C#+schema, ~25 fields). Residual: confirm with a real Windows app↔engine
  run post-merge (per-language unit tests won't catch a cross-wire slip). See DECISIONS.md.
- **U4 — macOS IPC shares fd 2 with MLX/runtime diagnostics** (no framing recovery). Lower-risk;
  the proper fix (dup2 fd 2 to the JSONL log before MLX loads, IPC on a saved dup) is a transport
  change that must be verified on a real macOS run before shipping.

---

# Carried from main (2026-06-04) — Windows log, still-open items

## 2026-06-04 — Review + land the SIX-workflow deep audit fixes (landed as `4ff9789`)

A second, larger adversarial sweep (6 serialized workflows: engine·app·perf·security + re-audit + regression-repair re-audit) fixed **~35 bugs** in the `main` working tree (layered on the prior uncommitted sweep), and its re-audit caught + repaired **4 regressions its own fixes introduced**. All headless-green (engine clippy `-D` + tests incl. +3 new; app build 0/0 + App.Tests + IpcSchema.Tests + format). **Committed as `4ff9789`** — see STATE 2026-06-04 (latest) + DECISIONS 2026-06-04 (latest) + per-finding record `shared/docs/audit-2026-06-04c/`. Resume order:

1. ~~Review the working-tree diff (39 files) and (owner) commit + push to `main`~~ **DONE — landed as `4ff9789`**; still confirm both GitHub workflows (windows-engine, windows-app) green. The diff was the prior uncommitted sweep + this one combined.
2. **On-hardware verify (RTX 2060 / 4 GB DirectML) — the only non-headless step.** Highest-value to confirm:
   - **People-tab edit during an auto-clustering pass** (rename/merge/mark-unknown right after a scan completes): the edit must SURVIVE (was silently discarded by the phase-3 DELETE+re-INSERT).
   - **Face clustering determinism:** re-run clustering twice on a >5k-face library → identical cluster IDs / People names (HNSW fixed seed).
   - **Restructure on a large library:** the tab must populate fast (O(n) frame reader), and **Anchor/"Keep" folders must NOT be moved** when applying (they're now dropped from the plan; confirm the Keep tile count still shows). Verify the moves that DO apply are only Tidy/Reorganize.
   - **OCR actually produces text** on a screenshot/PDF (the missing COM init meant it silently returned nothing — runtime-only; can't be headless-verified).
   - **Bulk-tag a large selection** + **scan** concurrently: no writer-lock stall (COM/sidecar + face-crop writes now off the lock).
   - Settings → restart engine after a (simulated) GPU-EP crash: the CRASHING EP is disabled, not a healthy sibling (ep_guard armed-set).
   - DebugLog still synchronous (forensic tail survives a fast-fail) — confirm app.log has the last lines after a forced crash.
3. **Deferred from THIS pass (real, documented in TRIAGE.md):** VLM server-death CLI fallback; CLIP-tokenizer punctuation (ML A/B); long-path trash manifest; wipe-vs-bulk interlock; applyRestructure outbound chunking; AppSettings lost-update refactor; Sankey "Other" drill-down; startup-auth-on-UI-thread; rename-heal FTS desync; the DebugLog durable-async perf opt (a persistent flushed StreamWriter, NOT naive batching).

## 2026-06-04 (latest) — Review + land the five-workflow bug-audit fixes (folded into the entry above — its fixes landed in the same `4ff9789` diff)

A fresh exhaustive bug-audit sweep (4 audit workflows + a re-audit/critic) fixed **11 bugs** in the `main` working tree (10 files: 4 app + 6 engine), all headless-green (engine clippy `-D` + **267 tests** + fmt; app build 0/0 + **131 + 38 tests** + format). **Committed in `4ff9789`** — see STATE 2026-06-04 (later) + DECISIONS 2026-06-04 (later) + the per-finding record in `shared/docs/audit-2026-06-04b/`. Resume order:

1. **Review the working-tree diff and (owner) commit + push** to `main`; confirm both GitHub workflows (windows-engine, windows-app) green. (`git diff` shows the 10 files; nothing else touched.)
2. **On-hardware verify the user-facing fixes (RTX 2060 / 4 GB box) — the only non-headless step:**
   - People AND Cleanup tabs: open + refresh repeatedly (incl. after a scan / after trash) — must NOT intermittently crash (the off-UI-thread `IsLoading`/`ErrorMessage` fast-fail).
   - Settings → **Restart engine** (and the post-GPU-pack-install restart): the engine must come back Ready, not wedge on "Starting…" with dead IPC (the stale-`Exited` race).
   - **Wipe library** right after a scan completes (while auto-clustering may be running): the People tab must be empty afterward — no ghost person cards.
   - **Restructure** on a large library (>~3.5k proposed moves): the tab must populate (was silently empty at the 1 MiB frame cap); an oversize drop now shows an `ipc_frame_too_large` error toast.
   - Suggested-merges sheet: shows "Looking…" then the result — no "No likely merges" flash.
3. **Deferred (real, out of this pass's scope):**
   - **Single-file Deep-Analyze waiter correlation** (`EngineClient.Commands.cs` DeepAnalyzeFileAsync ~406 + engine `deep_analyze.rs`): the waiter resolves on ANY `DeepAnalyzeComplete` (batch or another single-file), so it can show a wrong/false result. Needs a request/file-id threaded through the engine's single-file `DeepAnalyzeComplete` event (IPC contract change). Low sev.
   - **IPC forward-compat / determinism hardening** (no active drift today, so deferred): an unknown enum value drops the whole event (`IpcCoder.cs`); a missing REQUIRED field is silently zero/null-filled (`Dtos.cs`); the schema documented alphabetical key ordering but the engine emits insertion order (`sink.rs`) — the schema/README side of that drift is already corrected on `fix/bug-audit-sweep` (L29: key order is now documented as platform-dependent). Tighten the rest when the contract next evolves.
   - **ML-preprocess micro-opts — DEFERRED TO ON-HARDWARE A/B** (a tag/embedding *quality* regression here is invisible to the headless gate; per CLAUDE.md "tune ML against real data"): RAM++ + MobileCLIP per-pixel scalar 4-D ndarray indexing (~442K bounds-checked ops/image — the biggest potential preprocess win, but may be GPU-bound-masked; `ram_plus.rs`/`mobileclip.rs`); the redundant `Array4::zeros` memset before a full overwrite; the RAM++ batch-path full-frame clone per file (dormant unless `FILEID_RAMPLUS_BATCH_SIZE>1`); the dbwriter 512-byte per-face embedding clone; model-pool sized from the unclamped CPU worker cap (latent/benign now).
   - **restructurePlan engine-side paging**: the robust alternative to the 32 MiB cap raise — page the plan via a new bounded IPC event the app accumulates. Only needed if a real library exceeds ~200k proposed moves.

## 2026-06-04 (later) — win-face-fix-perf: reconcile to main, then on-hardware verify

`win-face-fix-perf` (off origin/main) = the suggested-merges hang fix + over-split tuning (implements `PLAN-suggested-faces-fix.md`) AND an exhaustive 4 GB/low-mem perf audit. Headless-green (engine clippy `-D` + 266 tests; app build 0/0 + 131 App.Tests + 38 IpcSchema.Tests + format). Commits `d7b0159f` + `c07f93e8`. See STATE 2026-06-04 + DECISIONS 2026-06-04. (This resolves the prior deferred "consolidate() 12k cap" and "suggested-merges fast / HNSW" items below — now DONE.)

1. **`win-face-fix-perf` is merged into LOCAL `main` (merge commit); PUSH it** to origin/main and confirm both GitHub workflows (windows-engine, windows-app) are green. Then close/delete the superseded remote branches `windows-v16.22-v16.26` (RAM++/Qwen work already on origin/main — see DECISIONS) and `plan/win-suggested-faces-fix` (its plan is now implemented).
2. **On-hardware (RTX 2060 + the 4 GB DirectML box) — the only non-headless verification:**
   - Suggested-merges: with a clustering pass running, the sheet must return immediately (not minutes); confirm far fewer duplicate person cards after a scan (AUTOMERGE 0.75); calibrate `FILEID_FACE_AUTOMERGE_COS` (~0.72–0.80) + the `FILEID_FACE_PASS3_*` env knobs against the labeled `G:\TrueNAS` library via `build/iterate.ps1` + `scan_assertions.py`.
   - Perf hardware-sensitive knobs (applied but UNVERIFIED — confirm no regression on the 4 GB box AND that the 6 GB box is byte-identical; all are gated on `MemoryTier::Low` / `pool_size==1`): memory_tier worker/pool/predecode clamps, VRAM-None pool=1 fail-safe, the pool=1 vision-semaphore (vision_cap=1), BGE-on-CPU, downloader streaming. Run the `build/*.ps1` perf benches.
3. **Deferred (still real; see the older sections below):** RAW decode; rotated portrait-video keyframes; durable content-keyed `face_verifications`; macOS lockstep of the new face behaviors; EV code-signing; add `FileID.IpcSchema.Tests` to `FileID.sln`.

## 2026-06-04 — Face-fix branch (win-face-cluster-merge-perf): MERGED to origin/main; on-hardware calibrate

`win-face-cluster-merge-perf-2026-06-03` fixes face scanning "totally broken" (root cause: the
`clip_text` scan-gate) + over-split / slow-merges + the install-stall toast — 16 fixes across
engine+app, found via 3 adversarial workflows (audit → gap-verify → re-audit). Headless-green: engine
clippy `-D` + **264 tests**; app build 0/0 + **App.Tests 108** + **IpcSchema.Tests 34** + format. See
STATE.md 2026-06-04 + DECISIONS.md. Resume order:

1. **Merge `win-face-cluster-merge-perf-2026-06-03` → main**, confirm both GitHub workflows green.
2. **On-hardware (RTX 2060) — the only non-headless verification:** install models (let `clip_text`
   finish), run a scan → confirm faces now populate the People tab; eyeball far fewer duplicate-person
   cards (auto-consolidate @0.85) and that suggested-merges is fast with the obvious dups at top. Tune
   `FILEID_FACE_AUTOMERGE_COS` (default 0.85) + the clustering thresholds against the labeled
   `G:\TrueNAS` library; set `=1.0` to disable auto-merge if over-merge appears.
3. **Deferred (real but out-of-scope this pass; file:line in the audit task outputs):**
   - **RAW decode** (arw/cr2/nef/dng → `FileKind::Image` but the `image` crate can't decode them → those
     files get zero faces). Needs a RAW-decode dependency (ASK before adding) or a WIC/embedded-preview
     fallback in `decode_image_sync`.
   - **Rotated portrait VIDEO** keyframes: Media Foundation doesn't auto-apply the display-rotation
     matrix → a rotated keyframe can miss faces (fail-soft, video-only). `shell/video.rs`.
   - **consolidate() 12k-cluster cap** degrades to a full no-op at extreme over-split — raise the cap or
     restrict the O(C²) scan to the top-N largest clusters (`face_clustering.rs` AUTOMERGE_MAX_CLUSTERS).
   - **suggested-merges HNSW** for very large person counts (mirror cluster()'s HNSW_MIN path) —
     consolidate() reduces P, so only needed if a real library still has many thousands of persons.
   - **Orphaned-verdict residual:** a "different people" verdict on two UNNAMED clusters can be lost if a
     re-scan churns `face_prints.id`; durable fix is content-keyed (file_id+bbox) `face_verifications`
     (migration vN+1). The name-guard already covers the named-people case.
4. **macOS lockstep** (needs a Mac): mirror the new face behaviors into the apple engine for parity —
   the relaxed scan model-gate, the centroid auto-merge + verdict/name guards, the 0.55..0.97 suggestion
   band, and the load-failure abort. No new Windows-side migrations this pass.
5. **(unchanged)** add `FileID.IpcSchema.Tests` to `FileID.sln`; EV code-signing cert.

## 2026-06-03 — Post-audit: ALL deferred fixed; merge + on-hardware verify

Full-repo audit (4 workflows, 78 confirmed) → ~70 distinct bugs fixed on branch
`win-prod-hardening-2026-06-03`, then a 5-pass refute-by-default RE-AUDIT loop caught + fixed 14
fix-introduced regressions. Headless-green: engine clippy `-D` + fmt + **258 tests**; app build 0/0 +
**App.Tests 108** + **IpcSchema.Tests 34** + format. Full record + the re-audit table in
[`AUDIT-2026-06-03.md`](AUDIT-2026-06-03.md). **NOT yet merged.** Resume order:

1. ~~Review + merge `win-prod-hardening-2026-06-03` → main~~ **DONE — merged as PR #9 (`773a812`), CI green.**
2. **On-hardware confirm (RTX 2060, the only thing not headless-verifiable):** `build-all.ps1 -Run`
   and eyeball the flicker fixes — search box doesn't glitch/crash; Deep Analyze 2nd run doesn't
   flicker; pipeline strip never blanks to grey on scan completion; Cleanup/People don't rebuild
   (flash) mid-scan; per-row model Cancel stops only that row + the slot returns to NotInstalled. Then
   the GPU/EP fixes on a real NVIDIA/Intel box (forced-CPU override → multi-threaded CPU + escapes a
   TDR loop; cross-vendor override binds the matching pack runtime).
3. **Add `FileID.IpcSchema.Tests` to `FileID.sln`** so CI runs it (it's currently orphaned — that
   masked a contract-test break this pass; we now run it directly in the gate, but CI should too).
4. **macOS lockstep of THIS branch's cross-platform changes** (needs a Mac): the v14
   `(kind, scanned_at)` index migration + `created_at` capture must mirror into the apple engine to
   keep the GRDB schema byte-faithful; the schema-drift additions (`skippedStages`/`currentCaption`/
   `cancelPrewarm.modelKind`) are already in the Swift DTOs per prior lockstep — re-verify.
5. **(unchanged external blocker)** EV code-signing cert → `release.yml` goes live (see below).

Everything else from the prior "deferred" list is DONE (LibraryView trash, Cleanup/People MergeById,
FilePreview/PersonDetail await-result, TreeDiff/Sankey, theme lifecycle, per-model cancel, EP/dylib,
TOCTOU, index/created_at, schema-drift, micro-perf). Repo hygiene: gitignore `rust_out.*` +
`__pycache__/` (stray untracked artifacts) — still worth doing.

## 2026-06-02 (later 6) — Verified "what's left" audit + ship-hardening + 256 closure

A read-only audit (5-cell workflow, refute-by-default vs CURRENT main) found the entries below SIGNIFICANTLY OVERSTATE remaining work. **The only HARD external blocker to a Windows v1.0 is the EV code-signing cert.** Verified already-DONE — stop re-chasing (older entries are stale on these):
- **SHA256 model pinning** — all 29 `registry.rs` artifacts pinned + the mandatory-pin CI gate is LIVE in `windows-engine.yml`. (Download-time verify against real network artifacts is the only residual — a release step.)
- **`release.yml` CD** — fully implemented, dormant-until-cert. **`publish-bundle.ps1`** — signtool `$LASTEXITCODE` guard + per-MSI Authenticode verify done.
- **Accessibility** — 161 `AutomationProperties` across all tabs (7b2b799); keyboard-nav CODE merged (3d47a63 / 9dd7785). Remaining: WCAG-AA contrast pass + multi-monitor DPI verify + a UI-automation test.
- **Memory bounding** — L1 128 MB byte-budget + 256 MB pre-decode budget LANDED (2f0d6b9). (RAM++ source-clone + ORT arena bounds remain for the 50K RSS gate.)
- **HNSW** module built + wired into face + restructure clustering (only the semantic-SEARCH path remains). **USN** foundation (v9 + query primitives) landed. **WS7** 18-fix polish merged (abc06a9). **ARM64** native runner live.
- **WS6 macOS DB-contract lockstep** — merged (PR #5 / e3e4959, macOS-CI build-verified): epoch→Unix-1970 across writer+readers, tag `source='auto'`, `startScan` reshape + `markPersonsDifferent`/`wipeLibrary` + 8 reply events + `EngineInfo.hardware` + `EngineError.modelKind`, hyphen sanitizer, extra-tag pruning. (The cross-platform DB ROUND-TRIP that defines lockstep still needs a Mac to validate.)

**Landed THIS session (CI-green on main):**
- **PR #6 ship-hardening** (138760c): image-decode cap (deep_analyze.rs, 50 MP peek-then-cap); **IPC capital-ID casing aligned Rust+C#+schema** (~25 fields — closes the eng-ipc-1/2 latent drift + makes the wire schema-conformant); per-monitor DPI `WM_DPICHANGED` handler; WiX `RollbackBoundary` in the Burn `<Chain>`; **single-source version** (`VERSION` + `Directory.Build.props` → csproj/WiX/Cargo + drift-guard). → resolves "later 4" item 2 + the eng-ipc casing item + decode-caps.
- **`windows-app.yml`** source-URL allowlist scan (closes the app-only-PR bypass — engine workflow's scan only fires on engine-path changes).
- **RAM++ 384→256 — CLOSED, dead end.** Export DID complete (`%TEMP%\ramexp\out256`, `[1,3,256,256]`, 4585 logits); fp16-converted to 660 MB. But tag-F1 vs the 384 model = **0.76** (60-img engine-faithful A/B), far below the 0.90 gate; **fp32-256 scored IDENTICAL 0.76 → the loss is RESOLUTION-INHERENT** (lossy 384→256 position-bias interpolation), not a fp16/threshold artifact. RAM++ stays at 384; the ~6.5 f/s GPU-compute-bound ceiling stands; no quality-preserving perf lever remains short of a different tagger model.
- **On-hardware (RTX 2060, DirectML, ISOLATED state — real library verified untouched):** merged engine ran crash-free, 120 imgs/20 s ≈ 6 f/s, 1128 `auto` tags, 218 SFace 128-d embeddings, healthy clusters, peak RAM 4.2 GB @ 120 files.

**Genuine remaining work, by blocker:**
- **External (THE blocker):** EV Authenticode cert → `FILEID_EV_THUMBPRINT` secret → `release.yml` goes live + pin `PROD_EV_THUMBPRINT`. The first real WiX MSI/Burn/ARM64 build runs in `release.yml`.
- **Needs a Mac (macOS BEHAVIOR layer — the DB contract is already done):** `FaceAlign.align112` + `VNDetectFaceLandmarksRequest` wiring + face-bbox px-vs-normalized parity (the face round-trip); RAM++ CoreML tagger; `content_hash`/`file_ref` scan write-path + rename-heal; restructure-routing parity; VLM `source='vlm'` tag emission.
- **Needs Windows hardware/time:** 1 h/50K crash-free soak + RSS ≤ 1.5 GB (needs the RAM++ source-clone / ORT-arena memory work — RSS hits ~5.7 GB at 50K); per-vendor EP matrix (AMD/Intel/Snapdragon — no HW here); face-threshold calibration (hand-labeled subset); multi-monitor DPI rescale verify.
- **Doable-here (lower priority):** canonicalize telemetry/URL allowlists into `shared/ci/*.txt` (DEFERRED this pass — refactoring a green release-blocker privacy gate that can't be verified locally risks a silent pass-everything regression; do it with a negative-control test); HNSW semantic-SEARCH path; USN reader; perceived-speed UX; Restructure P2–P4; WCAG-AA contrast (needs the user's eye on the gold palette).

## 2026-06-02 (later 4) — WS-CD remaining (CI/CD + release pipeline)

Shipped this session (DECISIONS "WS-CD pt.1"): `publish-bundle.ps1` signtool `$LASTEXITCODE` check + `CI_RELEASE` `-SkipSign`/`-SkipPrivacyGate` guard + per-MSI signature verify; `release.yml` (tag-triggered, dormant until the EV cert). Remaining — each needs a build-capable session, the network, or the cert:

1. **EV cert (SOLE hard blocker)** — procure (DigiCert/SSL.com/Sectigo) → store the SHA1 as the `FILEID_EV_THUMBPRINT` GitHub secret → `release.yml` goes live. Also pin `PROD_EV_THUMBPRINT` in `WinVerifyTrustChecker.cs` post-cert.
2. **WiX (build-capable session — neither CI nor the headless gate builds the .wixproj):** add a `RollbackBoundary` as a standalone install-sequence element in `Product.wxs` (NOT a `<MajorUpgrade>` child — that's invalid WiX); single-source the version from `Cargo.toml` via `/p:Version` → .csproj `<Version Condition>` + both .wixproj `DefineConstants` → `$(var.Version)` in `Product.wxs:33` + `Bundle.wxs:26` (kills the 5 hardcoded `0.1.0` sites).
3. **SHA256 population + gate (network):** fetch the `oid sha256:` from each HF LFS pointer (`GET <repo>/raw/main/<path>`) for the 39 `registry.rs` artifacts (byte-hash the small raw + pinned GitHub/NVIDIA ones), populate `registry.rs` + `MODELS.md`, THEN add the `windows-engine.yml` gate failing on any `sha256: None`. RAM++ hash provisional until the (blocked) WS5 256 re-export.
4. **CI gate hardening (local-test the loader, then push-verify):** canonicalize the telemetry-string + source-URL-allowlist into `shared/ci/*.txt` loaded by all 3 workflows + `publish-bundle.ps1` (+ add the missing source-URL scan to `windows-app.yml`); add a Cargo.lock-freshness gate (windows-engine) + a BOM-verify-after-format gate (windows-app).
5. **Final ship gate:** `release.yml` produces a signed bundle that installs + suppresses SmartScreen on a clean Win10/11 VM; crash-free 1h/50K soak.

## 2026-06-02 (later 3) — Production-hardening pass (plan: `majestic-foraging-tome.md`)

Driving the approved v1.0 plan via workflows, verified-merge per workstream. Landed to `main` this session (all headless-gate-green — engine clippy/fmt/test + app build/format/test):
- **WS4 a11y pt.1** — 161 `AutomationProperties` across all views (28 WCAG-AA contrast flags deferred to WS7).
- **WS2 silent-failure elimination** — 20 callsites + `EngineClient.WaitForBulkActionResultAsync` + `SqliteErrorTranslator` + `LibraryViewModel` open/search-error consumer (UI-thread-marshaled).
- **WS0 download-integrity** — `check_size_plausible` (size sanity, both paths) + `.part-N` orphan guard + 3 tests. Hash VALUES + non-`None` CI gate → WS-CD (need real artifacts; RAM++ hash not final until WS5).
- **WS3 part 1** — engine `db::quick_check` at open → `db_integrity_check_failed` EngineError; RestructureView selection persistence across nav (`_deselectedFileIds` static set).

**WS3 deferred (resume here):**
1. **DeepAnalyze ProposeRenames** — `ProposeRenamesCheck` (DeepAnalyzeView.xaml:296) is bound-but-ignored. Wiring is an **IPC-contract change, not app-only**: add `proposeRenames` to `DeepAnalyzeAllCommand` in `ipc.schema.json` + C# DTO + Rust command + honor it in `deep_analyze` (today the full pass always renames when `tagsOnly=false`; the checkbox should let the user get caption+tags WITHOUT renames). One atomic Rust+C#+schema PR with a schema-key test.
2. **Resumable scan checkpoint** — schema ready (`scan_sessions.last_file_index`); `db::open_writer` currently marks crashed `running`→`failed` (the anti-resume). The checkpoint WRITE (DBWriter updates `last_file_index` per batch) is low-risk/additive; the auto-RESUME (skip already-tagged on restart) is a pipeline change that can skip/re-tag files if wrong — **ship behind a default-off `FILEID_RESUME_SCAN` flag with unit tests for the skip logic, then enable + verify on the RTX 2060 against a real interrupted scan** (project rule: no unverified pipeline regression).

Remaining plan workstreams: WS1b (out-of-proc video keyframe), WS1c sweep (one-time `Resources[]` sites), WS4 (per-monitor DPI + keyboard E2E test), WS5 (256 re-export [BLOCKED: needs Py 3.11–3.13] + memory bounding + HNSW), WS6 (macOS lockstep [BLOCKED: needs a Mac]), WS7 polish, WS-CD (all CI/CD + hash population + EV cert [BLOCKED]).

## 2026-06-02 (later 2) — Scan-crash fix follow-ups

The mid-scan native fast-fail (in-proc shell VIDEO thumbnail provider on `.mov`) is FIXED + merged (video now skips the in-proc shell, like audio — see STATE). Remaining:

1. **Restore LIVE video thumbnails safely (out-of-process).** The skip makes new video tiles show a placeholder (cached keyframes still show). The durable fix for the WHOLE in-proc-shell crash class (images still use it) is an OUT-OF-PROCESS extractor: either (a) the shell `IThumbnailCache` COM API (Explorer's thumbnail-cache service runs providers out-of-proc — needs COM interop in `ThumbnailService`), or (b) reuse the engine's scan-time video keyframe (`shell::video::keyframe_25pct`) — persist a 192px keyframe the app's `ThumbnailDiskCache` can read. (b) is cleaner cross-platform (macOS uses out-of-proc `QLThumbnailGenerator`). Verify on hardware against a `.mov`-heavy folder.
2. **Arm WER dumps before the next repro** (`! pwsh -File platforms\windows\build\enable-crash-dumps.ps1`, self-elevates) so any residual fast-fail (e.g. a flaky IMAGE codec) drops a native minidump in `%LOCALAPPDATA%\FileID\crashdumps` that names the faulting provider DLL.

## 2026-06-02 (later) — Perf + bug + lockstep sweep follow-ups (branch `perf-bug-lockstep-2026-06-02`)

Landed this pass (headless-green, engine clippy + 246 tests): RAM++ preprocess-out-of-lock + byte-budgeted read-ahead (perf hygiene, below noise floor); eng-ipc-0 JoinError terminal events. Remaining, highest-value first:

1. **Lower-res RAM++ re-export 384→256 — THE #1 throughput lever (~1.8–2.7×, works on the shipped DirectML EP, also relieves the 90 %-full VRAM).** Everything is staged: torch 2.12 + the checkpoint (`%TEMP%\ramexp\ram_plus_swin_large_14m.pth`) are downloaded, `shared/scripts/export_ram_plus_onnx.py` now takes `--image-size`, and `platforms/windows/build/ram_ab.py` does the latency + tag-set-F1 A/B vs the shipped 384. **BLOCKER:** this dev box is Python 3.14, which forces transformers 5.x; `recognize-anything`'s vendored BERT imports symbols (`apply_chunking_to_forward`, `find_pruneable_heads_and_indices`, …) that 5.x removed from `transformers.modeling_utils` (the export script has a partial shim, but 5.x dropped some entirely). **Run the export in a Python 3.11–3.13 venv with `pip install "timm<1.0" "transformers==4.25.1" fairscale` + recognize-anything**, then: `python export_ram_plus_onnx.py --checkpoint <pth> --out-dir out256 --image-size 256 --precision fp16 --no-dynamic-batch --sample-image <img>`. Validate with `ram_ab.py --base <shipped-384-dir> --cand out256 --corpus G:\TrueNAS\Users --n 150` — **ship gate: tag F1 ≥ ~0.90, no systematic high-confidence tag loss.** If it holds: derive the engine's `INPUT_SIZE` from the loaded session input shape (so any square export "just works"; no per-build coupling), host the 256 model + SHA-pin (ties to ENG-76), re-measure on the 2060. If 256 fails the F1 gate, try 320.
2. **Clean perf A/B (the 2 wins are below the noise floor today).** `perf_bench.ps1` measured ~25 % run-to-run variance (RAM++ 517↔671 ms, same code). To detect the <5 % preprocess/RSS wins: pin the GPU clock (`nvidia-smi --lock-gpu-clocks`), average ≥5 runs, use the `[STATS]` `ramplus_us`/`vision_wait_us` counters rather than wall throughput. Also **fix `perf_bench.ps1` GPU-util sampling** — `nvidia-smi -lms … -f` returned 0 samples (the `-f` file isn't flushed before `Stop-Process -Force`).
3. **IPC field-name casing alignment (bug eng-ipc-1/2 + macOS round-trip) — ONE atomic, test-guarded Rust+C# PR.** `ipc.schema.json` (the contract) + macOS Swift use capital-`ID`; the Windows Rust `#[serde(rename_all="camelCase")]` structs + C# CamelCase policy emit lowercase-`d`, so the fields silently drop on a schema/macOS round-trip. Windows↔Windows works today (both lowercase-d), so **edit Rust AND C# in the same PR or you break the live app.** Add a Rust test (serialize each → assert the JSON contains the capital-`ID` key) AND a C# test so any missed mirror fails the gate. **Full field inventory (wire name → Rust struct.field @mod.rs / C# record):**
   - `fileIDs`: `ApplyTagsPayload.file_ids` (~269) / `ApplyTagsCommand.FileIds`; `TrashFilesPayload.file_ids` (~306) / `TrashFilesCommand.FileIds`
   - `fileID`: `RenameEntry.file_id` (~297) / `RenameEntry.FileId`; `EmbedImageQueryPayload.file_id` (~328) / `EmbedImageQueryCommand.FileId`; `RestructureMove.file_id` (~248) / `RestructureMove.FileId`; `BulkActionItem.file_id` (~884) / `BulkActionItem.FileId`  (`DeepAnalyzeFile*` already correct)
   - `queryID`: `EmbedTextQueryPayload.query_id` (~322), `EmbedImageQueryPayload.query_id` (~329), `ClipTextEmbedding.query_id` (~893) / `EmbedTextQueryCommand.QueryId`, `EmbedImageQueryCommand.QueryId`, `ClipTextEmbedding.QueryId`
   - `personID`: `RenamePersonPayload.person_id` (~350) / `RenamePersonCommand.PersonId`; `personIDs`: `MarkPersonsAsUnknownPayload.person_ids` (~367) / `MarkPersonsAsUnknownCommand.PersonIds`
   - `sourcePersonID`/`destinationPersonID`: `MergeClustersPayload` (~312-313), `RevertMergePayload` (~342-343), `MarkPersonsDifferentPayload` (~376-377), `MergeSuggestion` (mod.rs ~385-386 + Dtos.cs ~59) + matching C# records
   - `sourceAnchorFaceID`/`destinationAnchorFaceID`: `MarkPersonsDifferentPayload` (~378-379), `MergeSuggestion` (~388-389) / C#
   - `batchID`: `RestoreFromTrashPayload.batch_id` (~336) / `RestoreFromTrashCommand.BatchId`; `faceIDsToRevert`: `RevertMergePayload.face_ids_to_revert` / `RevertMergeCommand.FaceIdsToRevert`
4. **macOS lockstep — see [`LOCKSTEP-2026-06-02.md`](LOCKSTEP-2026-06-02.md) (39 confirmed divergences, file:line + fix per item; needs a Mac to build/verify).** Priority order: **(CRITICAL) timestamp epoch** — macOS `DBWriter.swift:331-333` + `DeepAnalyzeRunner.swift:276` write `timeIntervalSinceReferenceDate` (2001) vs Windows UNIX; switch to `timeIntervalSince1970` AND reconcile the read sites (`Restructure.swift:135` drop the `+978_307_200` shift, `:161-162` + app `ReadStore.swift:813-815,827` → `Date(timeIntervalSince1970:)`) — macOS is internally inconsistent, so verify holistically on a Mac. **(CRITICAL)** `startScan` `rootBookmark`→schema `rootPath`. **(HIGH)** wire `FaceAlign.align112` into `VisionWorker`/`FaceClustering` + add `VNDetectFaceLandmarksRequest` (the 128-d round-trip goal); add the 9 missing reply events + `wipeLibrary`/`markPersonsDifferent`; reconcile source token `vision`→`auto`. A few `win_verifiable=true` items (e.g. round-trip test coverage ipc-8) can land on Windows.
5. **Fuller bug-hunt re-run.** The 10-cell adversarial bug-hunt under-reported (capacity blips zeroed several find-cells → only 4 raw candidates). Re-run `workflows/scripts/win-bug-hunt-*.js` (retry-hardened) when capacity is stable for true top-to-bottom coverage.

## 2026-06-01 (later) — Audit follow-ups (branch `audit-fixes-2026-06-01`)

Full report + master prioritized plan: [`AUDIT-2026-06-01.md`](AUDIT-2026-06-01.md).

**✅ DONE + headless-verified across two waves (~17 fixes; see STATE):** crash/data-loss — ENG-2 (wipe FK leak), ENG-18 (file_ref u64 abort), ENG-42 (restructure ` (2)` churn), ENG-69 (SFace dim assert), ENG-71 (decode pre-alloc), APP-1 (UndoStack lock), APP-2 (watchdog dispatcher), PAR-111 (face-cluster re-entrancy); security/correctness — ENG-59 (per-EP disable), ENG-88 (zip actual-bytes cap), ENG-91/92 (rename path_hash + false success), ENG-97 (path redaction), PAR-69/96 (restructure name sanitizer parity); queries — PAR-116 (kind-in-SQL), PAR-117 (semantic failed=0). Plus the RAM++/vision-wait perf profiling that pinned the throughput bottleneck.

**Highest-value REMAINING, grouped by what each needs:**

**Headless-fixable on Windows now (pick up anytime):**
1. **Throughput on the RTX 2060 is at the card's ceiling (~6.2 files/s fp16+pool) — RAM++ is GPU-COMPUTE+VRAM-bound, ALL three concurrency/batching fixes DISPROVEN on hardware.** Profile during a single-path scan: GPU util **mean 73% / p50 87% / p90 97%**, VRAM **90% full**. Tested + reverted/kept-off: (×) CLIP fill-window (no gain); (×) CUDA pool=3 (regressed to 3.9 f/s — thrash); (×) **batched RAM++** — built the dynamic-batch ONNX export + `RamPlusBatchCoordinator`, A/B'd it, and batched=4 was **~23% SLOWER** than the pool (no idle compute to fill, no spare VRAM). The coordinator is retained **opt-in OFF** (`FILEID_RAMPLUS_BATCH_SIZE`) for high-SM/VRAM cards only — RE-VALIDATE per card; never default-on without a measurement. **Genuine remaining levers (all bigger efforts):** (a) **TensorRT EP** — fused Tensor-Core kernels for Swin-L on Turing (~1.5-2× potential, but VRAM-tight at 90% full — needs the TRT pack + workspace tuning; part of the all-vendor HW-accel project); (b) a **lighter tagger** (Swin-B/@224 — accuracy tradeoff); (c) close the mean-73%-vs-p50-87% gap via tighter CPU-decode↔GPU overlap (~modest). Also the per-image CLIP double-copy (ENG-57). The repro tooling lives in `platforms/windows/build/{profile_gpu.ps1,measure_batch.ps1}`.
   - **NEW (from the accuracy sweep):** re-tune `SCENE_COSINE_THRESHOLD` (0.15, `scene_vocab.rs`) against the corpus now that CLIP uses bilinear resize (#1 shifted the cosine distribution); land the **CLIP tokenizer reference-regex (#16)** in the same pass (regen `scene_embeddings_precomputed.rs` via `gen_scene_matrix` + retune together — do NOT land #16 without the retune). Consider a Cleanup perceptual/near-dup **opt-in** mode (#4) as a separate review-only surface (exact-content stays the default delete key — see DECISIONS).
2. **Unbounded image decode caps (ENG-10/38/71):** `Read::take(cap)` + `image::io::Reader.limits()` in `tagging.rs` / `deep_analyze.rs` / `vlm_server.rs` — OOM/DoS + allocation-abort defense (12 decoder threads each `Vec::with_capacity(size)`).
3. **RAM-fit VLM gating (PAR-57):** disable a VLM card whose RAM need > machine RAM (`GlobalMemoryStatusEx`) + "needs N GB" badge — prevents OOM-killing the engine.
4. **ReadStore (PAR-116/117):** push the kind filter into SQL *before* LIMIT (filtered grids under-fill today); add `AND failed=0` to semantic search. **Thumbnail `size` param (PAR-124):** every surface is a 192px upscale. **Cleanup keeper rank + preview (PAR-135/136); restructure irreversibility confirm (PAR-141); Settings Restart/Stop engine (PAR-148); live-scan headline (PAR-128).**
5. **EP-guard correctness (ENG-59 multi-EP poison, ENG-61/62 override mismatch); path-redaction unification (ENG-97/98/99).**

**Needs the RTX 2060 / `G:\TrueNAS`:**
6. **Re-measure DirectML throughput (HW-1, UNVERIFIED).** The audit never completed a DirectML scan (harness bug). Disable the CUDA pack, confirm forward progress + the CUDA-vs-DirectML delta. CUDA itself is confirmed working at 4.9 files/s.
7. **RSS ≤ 1.5 GB (HW-3):** RAM++ per-file source clone (ENG-67), decode-buffer caps, ORT arena bounds. Peak was 5.7 GB.
8. **Face-clustering over-split calibration (HW-5):** sweep COS thresholds on a hand-labeled real subset (176 persons / 624 faces today).

**Needs network / release step:**
9. **SHA256 verify-or-bail (ENG-76):** every `registry.rs` entry is `sha256: None`; multi-GB weights + native runtime DLLs load with ZERO integrity check despite the wired verify path. Pin real hashes in `MODELS.md`, enforce (also closes ENG-78 part-resume corruption, ENG-82 sentinel fast-path).

**Needs a Mac (WS-MAC lockstep — the cross-platform DB round-trip is broken today):**
10. **v13 migration on macOS (PAR-1)**; **SFace 128-d + FaceAlign wiring (PAR-2 / EG2)**; **canonical `source=` / `vlm_model` / timestamp-epoch tokens (PAR-78/88/85)**; **`markPersonsDifferent` + 7 missing IPC events/2 commands (PAR-3/107)**; **INSERT-OR-REPLACE FK-cascade + `content_hash`/rename-heal (PAR-14/15/16).**

## 2026-06-01 - On-hardware verify Wipe + Restructure overhaul (branch `windows/wipe-restructure-overhaul`)

Headless-green (app build 0/0, format exit 0, App 108 + IpcSchema 34). The WinUI runtime path needs the RTX 2060 (`build\build-all.ps1 -Run`):

1. **Wipe:** click Wipe -> confirm -> the library empties, **no rescan starts**, and the sidebar returns to the empty folder-picker (first-run state); `Models/` is preserved; a "Library wiped" confirmation shows. Re-pick a folder -> a fresh scan starts. Check the `[WIPE]` log stages; on a locked WAL the "Wipe partially failed" path must still restart the engine + clear the folder.
2. **Restructure:** plan auto-loads; stat-hero numbers match the card headlines; the three Keep/Tidy/Reorganize cards render via ItemsRepeater; Review expands an inline file list (checkbox + "from X") with no native fast-fail; per-file + per-group skip keep the ApplyBar count == the applied set; "See all N files" opens the scoped DrillDownSheet; Apply-as-shortcuts applies exactly the selected set (`LastRestructureApplyResult.Applied == selected`); the Flow/Tree toggle swaps cleanly; the Staying-put expander opens; "Run Deep Analyze" starts a VLM pass and re-plans on completion; the nudge hides once >= 40% of files are captioned.
3. **Crash-watch:** rapidly switch tabs while a plan / Deep Analyze is mid-flight; confirm no native fast-fail; `[ENGINE-SUB:RestructureView]` lines present and the `_unloaded` guards hold. Check `%LOCALAPPDATA%\FileID\logs\`.

## 2026-05-31 (later) — On-hardware verify the Suggested-merges crash fix (merged to `main`)

Headless-green (build 0/0, App 102 + IpcSchema 34, format exit 0, clippy clean, engine 242 tests, `cargo fmt --check` clean) and merged to `main`. On the RTX 2060, launch `build\build-all.ps1 -Clean -Run` and confirm:
1. **No crash:** People → **Suggested merges** opens and renders pairs (side-by-side anchor crops + similarity %). Reopen repeatedly / while the engine emits events. Check `app.log` for `[ENGINE-SUB:SuggestedMergesSheet]` and the **absence** of a new `session-died-without-handler-*.txt` (the native-fast-fail breadcrumb).
2. **Merge:** click Merge → the row dims, sibling rows referencing the merged-away person also dim, and the merged person is gone from the People grid after closing the sheet (`PeopleView` refreshes on dialog close).
3. **"Different people" survives re-cluster:** click it → pair suppressed; re-run face clustering (People → refresh) → the same pair stays suppressed (proves the v13 stable anchor-face keying). A self-merge must not orphan faces.

**macOS parity:** mirror migration `v13_face_verification_anchors` (ALTER `face_verifications` ADD `face_a`/`face_b` INTEGER) for cross-platform DB parity, and add the `markPersonsDifferent` IPC arm + handler. **Deferred (flagged):** wire `revertMerge` to the UI — it needs a merge-history record written in `handle_merge_clusters` (source person id + moved `face_prints` ids); without it merges are un-undoable.

## 2026-05-31 (audit hardening) — verification-gated follow-ups (branch `phase0-critical-fixes`)

The audit-driven hardening pass (Phases 0–4, see STATE.md) landed headless-verified on `phase0-critical-fixes`. These remain, grouped by what each needs. Branch is NOT merged — review/merge first, or continue on it.

**Needs the RTX 2060 / `G:\TrueNAS` (on-hardware):**
1. **CUDA-bind 3–5× (highest perf value).** Clean NVIDIA launch → `CudaAutoInstaller` fetches cuDNN + `ort_cuda_x64` → restart → confirm `engine.jsonl` `ExecutionProvider == "cuda"` + files/s up 3–5×. Then test the **B6/ep_guard** gate: set `gpuExecutionProviderOverride="cuda"`, corrupt `Models/packs/cuda/**/onnxruntime.dll`, relaunch → must log `[EP-GUARD]`, run DirectML, no crash loop (B6 now arms the override-aware EP).
2. **P3 pool retune for CUDA.** On 6 GB, the VRAM clamp keeps the pool ~2 so P3's EP-aware caps are a no-op. Measure real CUDA per-session VRAM during a scan and add a CUDA-specific `VRAM_PER_POOL_INSTANCE_MB` (DirectML's 2000 MB is allocator-conservative) so the pool — and thus the now-EP-aware semaphore — can grow. `tagging.rs`.
3. **P17 mutual-kNN A/B.** Run a People-tab cluster with `FILEID_FACE_MUTUAL_KNN=1` vs default on a hand-labeled subset; compare largest-cluster contamination + identity recall. Promote to default only if it cuts chaining without hurting recall.
4. **P19/P22 quality sweeps.** P19: gate low-quality faces out of Pass-1 *seeding* (`face_clustering.rs` load query has no `face_quality` floor; reuse `validate_face_geometry`'s score) — measure recall. P22: sweep `FILEID_RAMPLUS_PRECISION_FLOOR` below 0.62 (the floor clamps per-class thresholds *upward*, overriding RAM++'s F1 calibration) and diff tag precision/recall.
5. **ETA sanity (live GUI).** A real scan: tagging ETA tracks ~files/s (no "13s"), "Counting files…" during discovery, the five-dot strip advances. Restructure a folder of same-basename files → both survive (B3).

**Needs network / artifact access (release step):**
6. **S2 — populate SHA256 (HIGH security).** The verify-or-bail path is wired (`downloader.rs:282-308,509-514`) but every `registry.rs` entry is `sha256: None`. Fetch each pinned artifact (esp. the *code-executing* zips: llama.cpp b9254, cudart, cuDNN, ORT-CUDA/OpenVINO packs), `sha256sum`, record in `MODELS.md`, set `sha256: Some(...)`. A mismatch then bails before extract/run.
7. **P1 — batch RAM++ (headline perf).** Re-export `ram_plus.onnx` with a dynamic batch axis (`shared/scripts/export_ram_plus_onnx.py:263` is `dynamic_axes=None`; fall back to a fixed batch of 4 + padding if the Swin dynamic export trips Concat), re-host + SHA-pin (ties to #6), then add a RAM++ batch coordinator mirroring `batch_clip.rs`. ~2–4× on the heaviest in-scan GPU op.

**Needs a Mac (`swift build`/`swift test`, written-for-Mac edits already staged + specs):**
8. **macOS B8/S5/S8** (staged): verify the ScanCoordinator rate reset, the bounded IPC buffer, and the `blobToEmbedding` guards compile + behave.
9. **EG2/B9–B11 — wire FaceAlign (highest macOS quality gap).** `FaceAlign.align112` is a faithful port but has *zero callsites*; macOS embeds unaligned 15%-padded bbox crops, diverging from the shared calibrated thresholds. Add `VNDetectFaceLandmarksRequest` to `VisionWorker` (it supersedes the rectangles request — don't run both), map the 5 ArcFace points, **convert Vision normalized bottom-left → absolute top-left pixels** (the error-prone step — verify against a known image on a Mac), persist them, then call `FaceAlign.align112(source:landmarks:)` before `ArcFaceService.embed` in `FaceClustering.extractOneFile`. Do NOT blind-ship the coordinate math — it silently degrades embeddings if wrong.
10. **EG3/B12 — SFace contract cleanup** (low risk): `ArcFaceService.swift` still documents "512-d ArcFace" though it runs 128-d SFace (`AIModels.swift` embeddingDim=128); fix the docs, drop the ArcFace-iResNet50/MobileFace user copy in `FaceClustering.swift:60-63`, and assert `output.count == 128` so a wrong model fails loudly.
11. **EG4 — content-hash rename/move rebind on macOS.** v8 columns exist but nothing writes them; add a BLAKE3 helper (same ≤16 MB-full / head+tail+size composite), capture in discovery, write + rebind in `DBWriter`, and **mirror B1's old-path-gone guard**. Decision: BLAKE3 dep (a SHA-256 fallback breaks cross-platform hash equality — prefer a BLAKE3 impl).
12. **EG1 — port RAM++ tagging to macOS** (largest): `RamPlusService.swift` (ORT + CoreML EP), 384² ImageNet norm, per-class sigmoid → top-8, gate Vision/CLIP-scene to fallback. EG5/C3 — port `doc_extract` + a BGE-small ORT service to fill the dormant `doc_text/doc_fts/text_embeddings` tables.
13. **macOS S1 — replace `/usr/bin/unzip`** with an in-process extractor mirroring Windows `util/zip.rs` (enclosed-name + canonicalize + `starts_with`, per-entry/total caps, skip symlinks). `CLIPModelInstaller.swift:367-402`.

**Headless-buildable Windows UI parity (pick up anytime):**
14. **UG2 RAM-fit gating** (safety): disable a VLM card whose RAM budget > machine RAM (`EngineInfo.physicalMemoryGB`) + "Needs N GB (you have M)" badge — prevents OOM-kill. **UG1** Deep Analyze status card (active model / total / not-analyzed / ETA / RAM badge). **UG3/4/5** Settings: general Restart/Stop-engine buttons, Open-scan-log/Open-app-log, storage stat rows. Mirror macOS `DeepAnalyzeViews.swift`/`SettingsView.swift`.
15. **P12/P13 ANN search index** (perf, big): engine-side `semanticSearch`/`similarFiles` IPC owning a cached HNSW (reuse `util/hnsw_index.rs`) with brute-force fallback < ~5 K vectors (exact below threshold = no quality loss). Replaces the per-query full `clip_embeddings` scan in `ReadStore.cs`. **P14** FTS keystroke split (recall-flagged — needs a labeled query set), **P20** Cleanup near-dup phash tier (bucket to avoid O(n²); brings Windows toward macOS), **P21** box-average dHash (do on BOTH platforms in lockstep or not at all).

## 2026-05-31 — All-vendor acceleration (branch `windows-allvendor-accel`)

Auto-install + crash-safety landed headless-green. Remaining = hosting + on-hardware:

1. **CUDA auto-install on the RTX 2060 (highest value).** Clean launch on NVIDIA → `CudaAutoInstaller` auto-fetches cuDNN + `ort_cuda_x64` → restart → `engine.jsonl` shows `ExecutionProvider == "cuda"` + files/s up 3-5×. Then test the **B1 gate**: corrupt `Models/packs/cuda/**/onnxruntime.dll`, relaunch+scan → next launch must log `[EP-GUARD]` and run DirectML (no crash loop). Re-enable via Settings → Performance → "Verify install".
2. **OpenVINO (Intel) — assembled + hosted; verify on Intel HW.** The pack
   (`ort-openvino-win-x64-1.22.0.zip`, ORT 1.22 + OpenVINO 2025.1) is uploaded to
   `huggingface.co/Web-World-Wide/OpenVINO` and the registry points at it. On an Intel GPU box:
   confirm `CudaAutoInstaller` (Intel branch) auto-fetches it, `engine.jsonl` shows
   `ExecutionProvider == "openvino"`, files/s improves vs DirectML, and the B1 fallback works
   (corrupt the pack → reverts to DirectML, no crash). Then flip the Intel Accelerator Settings card
   from pseudo-Installed to an installable manual path. If the bind fails (likely cause: the raw
   DLL-load needs `plugins.xml`/an env tweak the pip package set up differently), iterate the pack
   contents — ep_guard keeps it safe meanwhile.
3. **QNN/Snapdragon:** deliberately no hosted pack (proprietary). If a Snapdragon WoA device is available, confirm the chain uses device-provided QNN when present, else DirectML.
4. **Merge `windows-allvendor-accel` → main**, both Windows workflows green. (vLLM: decided — keep llama.cpp, no work.)

## 2026-05-30 (later 5) — On-hardware verification for branch `windows-scan-fixes`

Four fixes landed headless-green on branch `windows-scan-fixes` (crash mitigation, grid arrow
keys, duration-tag removal, CUDA Performance Pack). **These need the RTX 2060 + live WinUI app:**

1. **CUDA pack — THE 3-5x test (highest value).** Build+run from the branch
   (`build-all.ps1 -Clean -Run`). In Settings → Performance (or the Welcome GPU Acceleration Pack),
   click install — it fetches `ort_cuda_x64` (Microsoft ORT-GPU 1.22.0, ~313 MB) + cuDNN, extracts to
   `Models/packs/cuda/`. **Restart the engine.** Confirm `engine.jsonl` shows the CUDA EP bound (no
   "using DirectML" line; `hardware.ExecutionProvider == "cuda"`) and `[EP] … pinning ORT_DYLIB_PATH`.
   Then scan the 100-photo sample and compare `[STATS] total_us` / files-per-second vs the ~5 files/s
   DirectML baseline — target 3-5x. **Risk to check:** if the EP silently stays on DirectML, the
   provider DLL version didn't match — verify `Models/packs/cuda/**/onnxruntime.dll` ProductVersion ==
   the pyke base (1.22.0). If cudart/cublas missing, ensure the `llama_runtime_cuda_x64` pack is installed.
2. **Crash repro + dump.** Run `build/enable-crash-dumps.ps1` (elevates) → WER full dumps for
   FileID.exe. Do a long scan + scroll an audio folder. Expected: no crash now (audio shell-thumb
   skipped). If it still crashes, grab the newest `.dmp` from `%LOCALAPPDATA%\FileID\crashdumps` for
   the native stack to confirm the faulting provider.
3. **Arrow keys — eyeball.** In the Library grid: arrows move the selected tile, Up/Down by a row,
   Home/End, PageUp/Down, Shift+arrows extend, Enter opens preview, Space toggles. Confirm the preview
   sheet keys (9dd7785) still work.
4. **Tags — eyeball + optional tune.** Confirm no more `3 sec`/`1 min` duration chips; `iPhone`/`Year`
   still present. Optional: `tag_report.py` → if `huddle`/`floor`/`animal` still read noisy, bump
   `FILEID_RAMPLUS_PRECISION_FLOOR` (~0.70) or extend `ram_plus_suppress.txt`, rescan, diff.
5. **All-vendor follow-on (per "all platforms and hardware"):** OpenVINO (Intel, Apache-2.0 — host on
   HF) and QNN (Snapdragon — device-provided, proprietary) EP packs follow the identical `packs/<ep>`
   pattern; the EP chain already builds them. Add per-vendor install slots + host the provider DLLs.
   AMD/Intel/Snapdragon already run DirectML (functional baseline); macOS uses CoreML/MLX (accelerated).
   Then on-hardware verify each vendor.
6. **Merge `windows-scan-fixes` → main** once the RTX 2060 confirms (esp. the CUDA EP binds), and
   watch both GitHub workflows.

---

# NEXT — Windows end-to-end correctness (resume here)

Branch `windows-e2e-correctness` (P1+P2+P4 committed, building green).

**UPDATE 2026-05-30 (later 2):** items 2 (P3) and 3 (P5) below are DONE (committed). A second
Scan/Cleanup UX batch also landed headless on this branch — Processing-flicker monotonic-phase
fix; RAM++ suppress sidecar + `FILEID_RAMPLUS_PRECISION_FLOOR` (floor 0.5->0.62) + `"catch"` +
`sample_corpus.ps1`/`tag_report.py` tuning harness; gold Faces-badge removal; restructure
`GROUP_CONCAT(DISTINCT …)` SQL crash fix; Cleanup `phash`->`content_hash` exact dupes. Engine
232/232 + clippy clean; app build + 34/34 + 102/102 GREEN. Item 4 (App.Tests headless) is
RESOLVED — both test projects pass headless. **Remaining now:**
- **On-hardware headless DONE (2026-05-30, later 3):** 100-photo RTX 2060 scan verified
  content_hash 100/100, restructure SQL runs (no DISTINCT error), tags clean (no catch / no
  dog→bear), library backed-up+restored. RAM++ locked in (posture fillers + catch suppressed).
  **Remaining = live-GUI-only checks a human must eyeball in the running WinUI app** (the
  headless engine path can't drive the renderer): (a) Processing sidebar holds steady on
  "Tagging" with no flicker during a big scan; (b) Cleanup shows byte-identical groups WITH
  thumbnails rendering; (c) Restructure tab Sankey renders with no "Couldn't read files table"
  toast. Run `build-all.ps1 -Run` and look.
- **All work merged to `main`; only `main` remains** (other branches removed this pass).
- **macOS: build on a Mac to verify the lockstep edits** (`Restructure.swift` SQL fix +
  `LibraryView.swift` Faces-badge removal) compile, plus the merged `macos-lockstep` model swap
  (`bash run.sh` / `swift build` + `swift test`). All macOS Swift is unverified-until-Mac.
- **macOS RAM++ + content_hash (deferred, needs a Mac + decisions):** macOS still tags via Apple
  Vision and dedupes via phash. To reach true lockstep, port RAM++ to macOS (then the suppress
  sidecar + precision floor apply) and have the macOS engine write `content_hash` (needs a
  BLAKE3 dep decision) before switching macOS Cleanup to exact dupes.
- Optional P3 status-card polish (per-model not-yet-analyzed counts + ETA, RAM-fit badge).

--- original resume list (items 2/3/4 now superseded above) ---

1. Rebuild the engine to clear the live `ram_plus` toast:
   `platforms\windows\build\build-all.ps1 -Clean -Run` (the running FileIDEngine.exe
   is stale; the source registry already has ram_plus).
2. P3 — Deep Analyze -> macOS parity. DeepAnalyzeView.xaml(.cs): status card
   (active model / total / not-yet-analyzed-by-this-model / ETA from a per-model
   seconds-per-image estimate); RAM-fit badge (EngineClient.Instance.Info.
   PhysicalMemoryGB vs the card RAM need); two-path naming banner (Go to People OR
   "Skip - analyze without names" -> DeepAnalyzeAllAsync); "Smart names -> Review
   and apply" card. Mirror apple/.../Views/DeepAnalyzeViews.swift; counts via ReadStore.
3. P5 — Settings -> macOS parity. SettingsView.xaml(.cs): reorder to Cleanup ->
   model cards by function -> a single Advanced disclosure; move GPU/Performance/
   NVIDIA into Advanced; add Logs buttons (AppPaths.LogsDir), collection stats in
   Storage (ReadStore), engine Restart/Stop (EngineClient.RestartAsync/ShutdownAsync).
   Keep About. Mirror apple/.../Views/SettingsView.swift.
4. Triage FileID.App.Tests headless failure (EngineClient ctor needs a UI thread;
   likely environmental — confirm on hardware or exclude from the headless gate).
5. On-hardware (RTX 2060 / G:\TrueNAS) via build\iterate.ps1: wipe+rescan has no
   "Wipe partially failed"; welcome modal shows all 5 rows and reaches all-installed;
   tagging/search/faces accuracy + >=140 files/s.
6. Merge windows-e2e-correctness -> main once P3/P5 land + CI green.

--- (previous NEXT.md content) ---

# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

---

## Butler-grade restructure overhaul — status (2026-05-30)

Design in [`RESTRUCTURE.md`](RESTRUCTURE.md). **P1–P4 built + headless-verified on
Windows; macOS mirror written but unverified.** Remaining, highest-value first:

1. **On-hardware butler verification** (needs `G:\TrueNAS`). Build the WinUI app,
   open Restructure, confirm: proposed folders are content-aware (an event/trip
   group, not just `Photos/Year/Month`); files matching a good existing folder route
   there; confidence bands + reasons read sensibly; the "What to apply" tiers + the
   Okabe-Ito/"Other" Sankey render correctly. Tune the provisional confidence
   thresholds (`restructure_semantic.rs` `AUTO_*`/`*_COHESION`/`MIN_MARGIN`) and the
   fusion weights against real results.
2. **macOS butler build** (needs a Mac). `swift build` + `swift test` the ported
   `RestructureSemantic.swift`, then wire the app-side UI per
   `platforms/apple/MACOS_BUTLER_NOTES.md` (reason display, confidence→Keep/Tidy/
   Reorganize mapping, Okabe-Ito Sankey).
3. **P2 live local-VLM naming (deferred enrichment).** Replace the c-TF-IDF group
   name with a Qwen2.5-VL label-then-reason on the cluster profile (distinctive tags
   + 3-5 medoid representatives), constrained decoding, cache by cluster signature —
   run as a background pass (charging/idle), not in the interactive plan, since each
   `llama-mtmd-cli` call reloads the model.
4. **Learn-from-corrections + earned autonomy (P3 follow-on).** Update folder
   centroids + thresholds on accept/reject; per-category accuracy track record;
   promote a category a tier only after a streak. Calibrate the bands to *measured*
   accuracy (today's cosine thresholds are provisional).

---

## Post commercial-clean merge (2026-05-29) — priorities, in order

The `windows-ramplus-adopt` work (RAM++ + Apache-2.0 commercial-clean stack) is verified on
hardware and merged. Remaining, highest-value first:

1. **Rename-heal collapses coexisting exact-duplicate files** (correctness, cross-platform).
   `pipeline/dbwriter.rs` rename-heal re-binds an existing row to a new path whenever a file's
   `content_hash` (or `file_ref`) matches — **without checking the old path still exists on
   disk**. For a true move that's correct; but when two byte-identical files coexist (e.g.
   `IMG_1558.HEIC` + `IMG_1558(1).HEIC`), the second steals the first's row, so only one of the
   two appears in the library and the Cleanup tab can't surface the exact-dup group. Not data
   loss (files stay on disk). **Fix**: only heal when the prior path no longer exists (stat it)
   or the USN journal recorded a rename; otherwise insert a distinct row and let phash dedup
   handle it. *Acceptance*: scanning a folder with N byte-identical pairs yields 2N file rows;
   Cleanup shows the dup group. Mirror the fix in macOS `Database`/dbwriter for parity.

2. **WS-MAC — macOS lockstep** (Swift written here, user builds/verifies on Mac). Mirror the
   Windows swap into `platforms/apple/`: RAM++ tagger (CoreML or ORT CoreML EP), ArcFace→SFace
   (128-d) embedding with Apple Vision detection, MobileCLIP-S2→ViT-B/32 (`.mlpackage`),
   regenerate the scene-embedding table, VLM ladder (drop Qwen-3B). Must match the v12 migration
   identifier + the 5-point alignment transform exactly. *Acceptance*: macOS ≥140 files/s held;
   person clusters within tolerance; semantic search quality unchanged; **a face DB written on
   one platform round-trips on the other** (the 128-d lockstep goal).

3. **Throughput re-baseline + CUDA Pack for ORT.** DirectML on the RTX 2060 measured ~6–7
   files/s (RAM++ Swin-L-bound). Host the ORT CUDA EP DLLs (`onnxruntime_providers_cuda.dll` +
   deps) so NVIDIA users get the 3–5× path (cuDNN 9.5 is already installed; only the ORT CUDA
   provider is missing). Evaluate batched RAM++ inference (current `tag()` is one image/run) for
   GPU utilization. *Acceptance*: SHIP.md Appendix W NVIDIA row re-measured with RAM++ enabled.

4. **SFace clustering — single-linkage Pass-1 fix + labeled fine-tuning.** The
   `identity_clustering.rs` bands were calibrated on-hardware (pass1 0.66 / pass2 0.54 / margin
   0.10 / pass3_min_mean 0.60 / max_splits 7), exploiting the measured gap between genuine clusters
   (~0.85+ mean cohesion — 27 studio portraits → 1 cluster, median 0.93) and chained blobs (~0.50).
   This cut the largest cluster on a 1475-face set from 90% (1339 faces, mean 0.40) to 7% (103,
   mean 0.66) with no over-split of the known identity. Two remaining items: (a) **Pass 1 is
   single-linkage connected-components** — it still chains different people through bridge faces on
   very large libraries; the structural fix is mutual-kNN or density-gated edges, not a higher
   threshold (which would start over-splitting genuine identities). (b) **Fine-tune against labeled
   faces** — current values fail safe toward over-split (478/1475 singletons on the backup subset;
   mergeable in the UI), but the precision/recall optimum needs ground truth. *Acceptance*:
   largest-cluster contamination + identity recall on a hand-labeled subset of `G:\TrueNAS` within
   target.

5. **WS9 hardening (handoff — needs your hardware/creds)**: per-vendor verification on
   AMD/Intel/Snapdragon; Authenticode **EV-cert** procurement + signing; WiX MSI + Burn bundle
   packaging. Also: full-corpus (26k) soak for VRAM/TDR over a sustained run.

## V16.29 — SmolVLM removed, tag-quality fixes, sidebar + Deep Analyze (2026-05-27)

**Landed (clippy + test green; dotnet build/test/format clean).** Response to user-reported
issues: image/video/audio tag chips were "year only"; SmolVLM cruft to remove; navbar
toggle no-op; Deep Analyze model list missing Gemma.

**Acceptance criteria** (user-run on hardware):
- Re-scan a folder of mixed kinds. Engine log shows `[TAGGING] scene_summary` lines per
  image / video with `scene_emit_count >= 1` and `max_score >= 0.15`. Image / video cards
  in the Library show scene chips (mountain / portrait / etc.), not just year.
- Audio cards (including ID3-less voice memos) show a duration chip (`12 min`, `1 h 05 min`).
- Click the title-bar hamburger toggle — sidebar collapses to zero width and re-expands cleanly.
- Deep Analyze tab shows three cards: Qwen 2.5-VL 3B (recommended), Qwen 2.5-VL 7B, Gemma 3
  4B. Each card's "Installed" badge reflects on-disk weight presence accurately.
- No SmolVLM card anywhere; no SmolVLM auto-install at engine-ready; settings.json's
  `selectedVlmModelKind = "smolvlm"` (if any) auto-migrates to `qwen2_5_vl_3b` on launch.

**Deferred to follow-ups (not in V16.29)**:
- **Scene vocabulary expansion** (`scene_vocab.rs:54-86`): the curated 50-label set may be too
  narrow for the user's library. Expanding requires regenerating `scene_embeddings_precomputed.rs`
  offline via the CLIP text encoder (~21 s build + checked-in matrix). If the diagnostic shows
  `scene_emit_count = 0` on many images even at threshold 0.15, expand the vocab.
- **Tile drop-shadow animation** (from V16.28 plan): still pending. Per-tile
  `Composition.DropShadow` with `ItemsRepeater` recycle cleanup.
- **ReadStore FTS5 v8 migration** (from V16.28 plan): non-sargable `LIKE '%x%'` branches in
  `SearchAsync` still force full table scans on large libraries.
- **macOS smolvlm-related session-log cleanup**: historical NEXT.md / STATE.md / DECISIONS.md
  entries reference SmolVLM as the canonical tagger; left intact per append-only convention.

## V16.28 — OCR overflow defense, thumb-cache LRU index, bulk-select batching, tile hover (2026-05-26)

**Landed (engine clippy `-D warnings` + test green at 212/0; dotnet build clean; app tests 102/0).**
Hardening pass on top of V16.27. Stacks under V16.27's hardware verify — same scan-and-look
checklist applies, plus three additional checks below.

**Acceptance criteria** (also do the V16.27 set):
- Open a library that has historically had 10K+ cached thumbnails. After the first scan there
  should be **no pause every ~30 seconds** while scrolling (the old `Directory.EnumerateFiles`
  sweep is gone). Diagnostics → cache bytes still updates as before.
- Click Library → click "Select" (or press Ctrl+A) on a ~10K tile library. Selection should land
  instantly. Previously this stalled for multiple seconds on the per-tile PropertyChanged storm
  + N×N `_selected.ToList()` reallocations.
- Hover a Library tile (no click). The white border ring should brighten visibly over ~0.18s
  alongside the existing 1.012× scale spring. Matches macOS LibraryView.swift:676-680.

**Deferred to follow-ups (not in V16.28)**:
- **ReadStore FTS5 v8 migration**: `ReadStore.SearchAsync` (lines 144-166) OR-joins MATCH against
  `ocr_fts`/`doc_fts` with `LIKE '%x%'` against `f.path_text`, `f.vlm_proposed_name`,
  `f.vlm_description`, `tags.tag`, and `persons.name`. The leading-wildcard `LIKE` branches are
  non-sargable, so any branch forces a full `files` scan. Real fix: extend `doc_fts` (or add
  `text_fts`) to include those columns, route the query through MATCH only. Needs the user's
  real DB to test the migration safely; queued here so the perf payoff is captured.
- **Tile hover shadow animation** (`Views/Library/LibraryView.xaml` + `.xaml.cs`): C2 of V16.28
  shipped only the stroke part of the macOS hover spec. Shadow opacity 0.18→0.45 + blur 5→14
  needs per-tile `Microsoft.UI.Composition.DropShadow` with cleanup on `ItemsRepeater`
  recycle; non-trivial relative to a comment-and-scope session.
- **TagChip color canonicalization (decision needed from user)**: Windows
  `TagChipForegroundBrush`/`TagChipBackgroundBrush` are gold `#FFCD3C` @ 0.85 / 0.10 (Theme.xaml
  lines 71-72). macOS LibraryView.swift:734-739 uses `.foregroundStyle(.secondary)` +
  `.fill(Color.secondary.opacity(0.10))` — i.e. system gray, NOT gold. Three options:
  1. Match macOS literally (system gray, no brand color).
  2. Keep current Windows gold (`#FFCD3C` is brand-aligned; CLAUDE.md says gold reserved for
     "primary actions + the Smart name result"; gold-tinted chips technically violate that).
  3. Use palette gold `#FFCC00` instead of `#FFCD3C` (the latter is a stray near-miss).
  No code change pending decision.

## V16.27 — Scan-pipeline single-read + UI parity polish (2026-05-26)

**Landed (engine cargo check + clippy `-D warnings` + test green; awaiting hardware verify).**
Image EXIF ghost-read eliminated; doc/pdf/audio extract paths now share the decoder-thread
pre-read buffer for files ≤ 16 MB (one fewer file open per matching file). ApplyBar hover
spring wired to match macOS. TagChip Kind theme brushes defined. Stray DLLs gitignored.

**On-hardware verify** (run a scan against a mixed library):
- Image tiles still surface camera/GPS for JPEGs; PNG/GIF/screenshot tiles scan cleanly with
  no EXIF and no crash.
- Doc / PDF / Audio files still surface keyword chips / artist+album tags as before; > 16 MB
  files still scan via the composite-hash fallback.
- Restructure ApplyBar buttons scale up to 1.02× on hover when enabled, snap back on exit.
- Library Kind chips visually identical to V16.26.

**Deferred (next sessions, not in V16.27)**:
- **Video keyframe single-read**: `shell::video::keyframe_25pct` calls ffmpeg / Windows Media
  Foundation, both want a file path / IMFByteStream. Streaming bytes in needs a meaningful
  adapter and the typical video exceeds the 16 MB cap anyway; composite-hash + path-based
  decode is fine.
- **Deep Analyze cross-pass RGB cache**: re-rasterizes images for the VLM. Reusing scan-time
  decoded RGB would mean caching ~50K decoded frames — disk thrash or unbounded memory. Deep
  Analyze is user-triggered, so the second read is acceptable.

## V16.26 — No-self-host posture + hanging-feature sweep (2026-05-22)

**Landed (engine clippy `-D warnings` + test on pinned 1.90 green — 204-0; C# dotnet build 0/0).**
Hardened policy: every artifact the engine downloads must already exist on a public upstream.

**Removed**: RAM++ integration (no public ONNX), Performance-Pack registry arms,
`NotYetAvailable` variant, conversion script. **Unhung**: HNSW into face_clustering, PDF text
extraction, BGE-small text embeddings (with migration v11 + persistence).

**Tagging promise vs V16.21 — strictly better-or-equal, never worse**: images same; documents +
audio gain strictly-new tag chips; faces faster on big libraries; rename/move preserves tags.

**On-hardware verify**: rename a file mid-scan → tags preserved; deep paths get scanned; OneDrive
cloud files don't hydrate; doc files get keyword chips + show up in semantic search once BGE is
installed; audio files render artist/album/year chips.

**In-policy follow-ups** (no self-hosting needed):
- USN reader (`FSCTL_READ_USN_JOURNAL`) + scan-skip-set integration.
- Whisper.cpp subprocess (whisper.cpp binaries + GGUF Whisper models are public).
- Florence-2 inference (4 ORT sessions + generation loop + `tokenizers` dep).

## V16.25 — Research-implementation Phases 3–7 landed (2026-05-22)

**Landed (engine clippy `-D warnings` + test on pinned 1.90 green across the full suite).**
Five phases in one session on top of V16.24 (Phases 0–2 + content-hash brick):
- **Phase 3**: rename/move heal (BLAKE3 + NTFS MFT-ref, migration v8) + USN journal foundation
  (admin gate + query primitive, migration v9) + pure-Rust HNSW vector index
  (`instant-distance` — no C++ build).
- **Phase 4**: doc content pipeline — txt/md/docx/pptx/xlsx via `quick-xml`; RAKE keyword tags;
  `doc_text` + `doc_fts` FTS5 (migration v10).
- **Phase 5**: audio metadata chips (artist/album/title/genre/year via `symphonia`).
- **Phase 6**: per-vendor quantized-variant framework documented (resolver was Phase 1; variants
  ship with each model's base hosting).
- **Phase 7**: Florence-2 foundation — real registry arm for `onnx-community/Florence-2-base`
  (downloadable today) + skeleton module + docs.

**Verify on hardware:** Phase 0 robustness (long-path / OneDrive online-only / file-lock) +
CPU multi-thread inference uplift (Phase 1) + rename-heal preserves tags across a move +
doc/audio files now render content tag chips.

**Documented follow-ups** (foundation in place; full integration deferred):
- Phase 3b: USN reader + scan-skip-set integration.
- Phase 3c: HNSW into `face_clustering` above ~5 k faces.
- Phase 4b: PDF text extraction + BGE-small text embeddings + GLiNER NER.
- Phase 5b: YAMNet sound-event tagging + Whisper transcription.
- Phase 6 hosting: per-model `_int8` (OpenVINO) + `_qnn` (Qualcomm AI Hub) variants.
- Phase 7b: Florence-2 inference (4 ORT sessions + generation loop + `tokenizers` dep + Deep
  Analyze grounded-OD backend).
- **RAM++ activation**: run `shared/scripts/convert_ramplus_onnx.py` on **transformers 4.x /
  Python 3.11–3.13** to produce + host the ONNX. Until then RAM++ stays gated; the VLM tagger
  remains the default (zero regression).

## V16.24 — Phase 2 RAM++ landed (code); Phase 3 underway (2026-05-22)

**Landed (engine clippy `-D warnings` + test on pinned 1.90 green — 184-0).** RAM++ multi-label
tagger wired as the primary scan tagger *when installed* (gated; no regression). `blake3` content-hash
utility added for Phase 3 rename/move identity.

- **RAM++ activation requires an offline ONNX conversion** (`shared/scripts/convert_ramplus_onnx.py`)
  — run it on **transformers 4.x / Python 3.11–3.13** (Python 3.14 forces transformers 5.x, which the
  2023 RAM++ stack doesn't fully support), then host the outputs and flip the `"ramplus"` registry arm
  from `not_yet_available` to a real `Model`, or drop `ramplus.onnx` + tag-list files into
  `%LOCALAPPDATA%\FileID\Models\ramplus\` for local testing.
- **Continue Phase 3**: rename/move rebind (content_hash + NTFS-ref `file_ref` column, migration v8,
  dbwriter lookup-before-insert) → USN journal scanning (admin-gated, jwalk fallback) → vector index.
  ⚠️ Decision pending: `usearch` pulls a C++ build dependency — likely feature-gate it or use a
  pure-Rust HNSW to keep the default "download-and-run" build.
- Then Phases 4 (documents), 5 (audio), 6 (per-vendor NPU variants), 7 (Florence-2, optional).

## V16.23 — Phase 1 ML/hardware foundation landed (2026-05-22)

**Landed (engine clippy `-D warnings` + test on pinned 1.90 green — 177-0).** `active_provider` +
`configure_session_builder` (per-EP graph-opt + CPU multi-threading) + `models::variants`
(per-EP variant resolver, fp32 fallback) + pure-Rust `wordpiece_tokenizer` + QNN HTP backend.

- Mostly headless-verifiable. **On hardware:** a CPU-only box should now scan faster (multi-threaded
  ONNX intra-op). NPU paths (Intel OpenVINO device hint + INT8, Snapdragon QNN w8a8) finish in Phase 6.
- **Next — Phase 2 (RAM++ multi-label tagging).** ⚠️ Prereq: RAM++ has no first-party ONNX, so the
  code lands behind the existing "model not installed → stage skips" gate (no regression — SmolVLM
  stays the tagger until RAM++ is present). A one-time **offline conversion + HuggingFace hosting**
  of the RAM++ ONNX (script + `shared/docs/MODELS.md` entry to be added) is required before RAM++
  actually runs.

## V16.22 — verify: Phase 0 robustness (long-path / OneDrive / file-lock) (2026-05-22)

**Landed (engine clippy `-D warnings` + test on pinned 1.90 green — 167-0; C# build 0/0).** First
slice of the research-implementation plan (`~/.claude/plans/i-want-to-implement-radiant-sunset.md`).
Rebuild + re-scan: `pwsh -File platforms\windows\build\build-all.ps1 -Run` (`-WipeDbOnly` for fresh).

1. **Long paths.** Scan a tree with a path >260 chars (deeply nested folders). The deep files appear
   in Library and get analyzed (previously silently skipped). `SELECT path_text FROM files` shows
   normal-form paths (no `\\?\` prefix).
2. **OneDrive online-only.** Point at a folder of dehydrated (cloud-only) OneDrive files. Scanning does
   NOT trigger downloads (watch the OneDrive tray + network); they get a metadata row with no content
   tags. Hydrated files scan normally.
3. **File locks.** A file mid-write by another app is retried briefly instead of one-shot skipped.
4. **Next:** Phase 1 (shared ML/hardware foundation: per-EP variant resolver + session tuning +
   WordPiece tokenizer + NPU/QNN wiring), then Phase 2 (RAM++ multi-label tagging).

## V16.21 — verify: welcome models, discrete-GPU, tag quality, progress (2026-05-22)

**Landed (engine clippy/test on pinned 1.90 green — 163-0; C# `dotnet build` 0/0).** Rebuild +
re-scan: `pwsh -File platforms\windows\build\build-all.ps1 -Run` (`-WipeDbOnly` for a fresh scan).

1. **No silent download.** Fresh launch → nothing downloads until a button is clicked. Watch
   `%LOCALAPPDATA%\FileID\Models` + `app.log` for the absence of any `[SMOLVLM-AUTO]` line.
2. **Welcome screen.** 5 rows: CLIP · ArcFace · SmolVLM (tagging) · Qwen Deep Analyze · GPU pack.
   The Qwen row shows **3B vs 7B** matching this PC (≥16 GB RAM **or** ≥8 GB VRAM → 7B). **Install
   all** pulls every row (incl. both VLMs) to ✓. Installing the Qwen row sets
   `SelectedVlmModelKind` (Deep Analyze tab shows the same model selected).
3. **No progress flicker.** During any model download the row shows one smooth bar (indeterminate
   until first byte, then fills) — no ProgressBar↔spinner flicker. Same in Settings → AI Models.
4. **Tags are descriptive.** Re-scan a folder of geotagged phone photos → chips are **1–2 specific
   words** (e.g. "golden retriever", "mountain lake"); **no** "Has Location"/"Has Text"/"Has Faces".
   `SELECT tag FROM tags WHERE source='vlm'` shows concrete nouns; `SELECT DISTINCT tag FROM tags`
   has no "Has *".
5. **Discrete GPU (hybrid iGPU+dGPU).** Settings → Performance shows the dGPU adapter name. During
   a scan, Task Manager → Performance shows load on the **discrete** GPU, not the iGPU. For Deep
   Analyze on the **Vulkan** runtime, `engine.jsonl` shows `[VLM] pinning llama.cpp to discrete GPU`
   with the chosen `VulkanN`. (If `--list-devices` output differs on your hardware and the line is
   absent, share it — the parser in `vlm.rs::parse_best_vulkan_device` is keyed to the b9254 format.)

## V16.17 — verify: SmolVLM-only tags + CLIP semantic search kept (2026-05-21)

**Landed (all gates green: engine clippy/test/fmt on the pinned 1.90; C# build 0/0 + format +
BOM).** Rebuild + re-scan (`pwsh -File platforms\windows\build\build-all.ps1 -Run`;
`-WipeDbOnly` for a fresh scan).
1. **No CLIP tags.** Re-scan → Library chips are SmolVLM-only. `SELECT DISTINCT source FROM
   tags` returns no `auto`.
2. **Semantic search still works.** A free-text query ("a dog at the beach") returns semantic
   matches (needs MobileCLIP installed); `SELECT COUNT(*) FROM clip_embeddings` populates on
   new files. No ~21 s scene-matrix build in `engine.jsonl` (it's tags-only now).
3. **UI.** Settings/Welcome still offer the MobileCLIP install card (for search); the
   scene-tagging diagnostic reads "Tags: SmolVLM; Semantic search: MobileCLIP-S2".
4. **Kill switch:** to drop CLIP entirely (search → FTS5), flip `scene_vocab::ENABLE_CLIP = false`.

## V16.16 — verify the crash fix + get Deep Analyze running on hardware (2026-05-21)

**Landed (all gates green in-agent: C# build 0/0 + dotnet format clean; engine cargo
check / clippy `-D warnings` / test 158-0 / fmt clean).** Rebuild:
`pwsh -File platforms\windows\build\build-all.ps1 -Run`.

1. **Crash gone.** With a scan running, click into **Restructure** (and each other tab)
   repeatedly → no crash, no half-blank tab; the Sankey/Tree-diff toggle works. No new
   `crash-*.txt` under `%LOCALAPPDATA%\FileID\logs\`.
2. **Settings EP override persists.** Set Settings → Performance → execution-provider
   override to a non-"auto" value, leave + reopen Settings → it stays (was resetting to
   "auto" on every open).
3. **Deep Analyze + tagging end-to-end.** The relaunch auto-reinstalls the **b9254**
   llama.cpp runtime (replacing the stale b4475 that lacked `llama-mtmd-cli.exe`); install
   **Qwen2.5-VL-3B** from the Deep Analyze tab. Then a scan auto-tags via SmolVLM
   (`SELECT tag FROM tags WHERE source='vlm'` populated) and "Whole library" Deep Analyze
   produces captions + smart names on the resident server. A missing model now says
   "install it from the Deep Analyze tab" (not a confusing runtime error).
4. **Perf.** Run a scan with `FILEID_PERF_TRACE=1` and share the `[PERF]` lines so the
   per-stage bottleneck can be tuned toward ≥140 files/s.

**Open decision:** broad comment condensation (Workstream F) is held — this codebase's
verbose comments are load-bearing bug-prevention WHYs (CLAUDE.md says don't strip them).
Confirm if you want an aggressive purge anyway.

## V16.15 — verify on hardware: face crops + 1-2 word tags + smooth downloads (2026-05-21)

**Landed (engine clippy + 158 tests; C# format+BOM; build in VS).** Rebuild:
`pwsh -File platforms\windows\build\build-all.ps1 -Run`.
1. **Faces:** re-scan a folder with people → the People tab shows real cropped faces (not
   blank, not whole-image smears); same-person faces group; merge works. Existing DBs hold
   the OLD bad crops — use `-WipeDbOnly` (or re-scan) to regenerate. `SELECT COUNT(*) FROM
   face_prints` > 0; the `face_crops/*.jpg` look like faces.
2. **Tags:** `SELECT tag FROM tags WHERE source='vlm'` → all 1-2 words (no 3+-word phrases).
3. **Deep Analyze:** tab defaults to Qwen2.5-VL-3B; "Whole library" → full-sentence captions
   + smart names. (Qwen3-VL-4B unavailable as GGUF; 7B OOMs on 4 GB — see DECISIONS.)
4. **Downloads:** the rate/ETA rise smoothly and do NOT blink to 0 / "Stalled" at file
   boundaries in multi-file model bundles.

## V16.13 — verify on hardware: scan starts (no timeout) + SmolVLM tags / Qwen Deep Analyze (2026-05-21)

**Landed (engine clippy `-D warnings` clean; C# `dotnet format` + BOM clean — build in VS).**
Rebuild from the repo root: `pwsh -File platforms\windows\build\build-all.ps1 -Run`
(`-WipeDbOnly` for a fresh DB). Fixes the 4 GB-VRAM/DirectML model-load timeout + the
tagging/Deep-Analyze model split:

1. **Scan starts — no 30 s timeout.** First launch: scene matrix builds once
   (`engine.jsonl`: `[TAGGING] scene-label embeddings built elapsed_ms≈21000`) and the scan
   runs (no `model_load_timeout`). EVERY later launch logs `scene-label matrix loaded from
   cache` (no 21 s build) and starts <10 s. A `Models\clip_scene_cache\scene_matrix.bin`
   appears.
2. **Tagging = SmolVLM.** After a scan: `app.log` `Auto-chaining Deep Analyze (tags-only).
   model=smolvlm`; `SELECT COUNT(*) FROM tags WHERE source='vlm'` climbs.
3. **Deep Analyze = Qwen.** The Deep Analyze tab shows **Qwen 2.5-VL 3B active** by default
   (existing settings migrated off smolvlm); SmolVLM still selectable. Qwen cards show
   **Install** (not a false "Installed") until downloaded; after Install + "Whole library",
   `SELECT DISTINCT vlm_model FROM files` shows `qwen…` with captions. (On 4 GB VRAM Qwen 3B
   may be slow / spill to RAM — SmolVLM is the fast option.)
4. **(Follow-up, not this pass) faster ONNX:** ONNX runs on DirectML (perf-hint logs it).
   The CUDA ORT pack that would make ArcFace/SCRFD/CLIP ~3-5× faster is `not_yet_available`
   — needs the ORT 2.0.0-rc.10 CUDA provider DLLs sourced + hosted.

## V16.12 — verify on hardware: first-scan tagging + first-run speed + VLM fallback (2026-05-21)

**Landed (engine cargo check + clippy -D warnings clean; C# self-reviewed but
NOT compile-verified — WinUI CLI build is blocked on the dev box, build in VS).**
Rebuild from a VS Developer shell: `pwsh build/build-all.ps1 -Run` (add
`-WipeDbOnly` for a fresh DB, or `-Wipe -PreserveModels` to re-test first-run
install ordering without re-downloading multi-GB weights).

1. **First-scan tags (THE fix).** On a clean profile (`-Wipe -PreserveModels`
   keeps SmolVLM so this exercises the *installed* path; for the genuine
   first-run, use `-Wipe`): scan a folder. CLIP placeholder chips appear during
   the scan. When SmolVLM finishes installing after the scan, `app.log` shows
   `[AUTO-ADVANCE] SmolVLM finished installing after a scan — triggering
   tags-only auto-pass.` (this is the NEW path) — not just "no VLM installed;
   skipping." `SELECT COUNT(*) FROM tags WHERE source='vlm'` climbs from 0 on
   the FIRST scan's lifetime, and chips switch placeholder → VLM tags. No
   double-pass (only one `Auto-chaining Deep Analyze (tags-only)` per cycle).
2. **VLM server payload.** `engine.jsonl` shows `[VLM-SERVER] persistent server
   up; payload self-test OK`. If instead `payload self-test failed; falling back
   to per-file CLI` + a `vlm_server_payload_rejected` warning — tags still land
   (slower), and the logged probe error tells us the server's expected payload
   shape to fix. Either way the batch must produce `source='vlm'` rows.
3. **Odd formats.** A `.webp`/`.bmp` in the library gets VLM tags (transcoded),
   not a per-file failure.
4. **First-run speed.** With `-Wipe` (true first run, NVIDIA): `app.log` shows
   `[CUDA-AUTO] deferring CUDA runtime until a VLM is installed`; the CUDA
   ~650 MB pack does NOT download until after SmolVLM's sentinel lands. First
   scan `files_per_second` is materially higher than before (no triple-download
   contention). No false "No response from engine — try again" install failures.
5. **Crash-during-scan.** Click around the sidebar / switch tabs rapidly during
   a live scan — no crash (this class is already defended; the CUDA-defer
   shrinks the hang-prone window). If it still dies, grab the crash dump under
   `%LOCALAPPDATA%\FileID\logs\` — it now pinpoints the offending event.
6. **CLIP batch/pool A/B (perf tuning, optional).** Scan the same folder twice:
   once default, once with `FILEID_CLIP_USE_BATCH=0`. Compare `clip_p95_ms` +
   `files_per_second` from the sidebar/batch stats; lock in the winner (the
   default is currently batch-ON, flagged pending this measurement). Watch for
   `[FILEID_GPU_DEVICE_REMOVED]` — if it appears, the setting exceeded the TDR
   ceiling and must be lowered.

## V16.11 — verify on hardware: thumbnails + Deep Analyze runtime + SmolVLM auto-tag (2026-05-21)

**Landed (compiles + clippy -D warnings + all tests + format + BOM; see STATE V16.11).**
Three root-caused fixes + SmolVLM auto-tagging. A clean rebuild is required
(`pwsh build/build-all.ps1 -Run`, or `-WipeDbOnly -Run` for a fresh DB). These
are GUI/timing/runtime behaviors a compile cannot prove:

1. **Thumbnails render (the NOW fix).** Scan a folder. Every visible card shows
   its image immediately — square, image area NOT collapsed — during a live scan
   AND at rest. The bug was the `TileRoot` `Height="{Binding ActualWidth …}"`
   self-binding (non-observable DP → stuck at 68 → image row collapsed); now set
   via `OnTileSizeChanged`. If a card is still blank, `app.log` `[THUMB]` lines
   tell which: `TILE_SIZED w=… h=…` (layout) + `TILE_THUMBNAIL_ASSIGNED … px=WxH`
   (bitmap). px>0 with no/!square TILE_SIZED ⇒ layout; px=0 ⇒ decode.
2. **Deep Analyze: no "runtime too old" toast.** Deep Analyze a single image →
   caption succeeds, no toast (the 3 MB→20 KB `sanity_check_binary` floor fix —
   the thin 89 KB `llama-mtmd-cli.exe` now passes). `engine.jsonl` shows
   `[VLM-SERVER] ready` on a batch; no orphan `llama-server.exe` after.
3. **SmolVLM auto-tagging.** Existing settings.json (had `qwen2_5_vl_3b`) is
   migrated to `smolvlm` on first launch (`[INSTALL]`/AppSettings v2). SmolVLM
   auto-installs (`[SMOLVLM-AUTO] … installing`). Scan → CLIP placeholder chips
   appear immediately (threshold 0.18); after the scan completes + SmolVLM is
   installed, the next scan's auto-chain runs the tags-only pass
   (`tags_only:true`) and `SELECT tag,COUNT(*) FROM tags WHERE source='vlm'
   GROUP BY tag` climbs with real tags; cards switch from placeholder → VLM tags.
   Kill + relaunch mid-pass → resumes (only untagged files). The single Settings
   → Cleanup "Tag automatically with AI after scans" switch toggles it.

**Known follow-ups (non-blocking):**
- **First-scan auto-tag latency.** On the very first scan SmolVLM may still be
  downloading when the auto-chain checks `Vlm.Status`, so auto-tagging starts
  from the *second* scan. If we want first-scan coverage, trigger the auto-tag
  pass on SmolVLM install-complete (listen for the smolvlm sentinel/slot →
  Installed transition) rather than only on the scan→cluster→caption chain.
- **"Remove CLIP" switch** is still `ENABLE_CLIP_SCENE_TAGS=false` (engine) once
  VLM tagging is validated as strictly better; left on as the placeholder.

## V16.8 — VLM activated (runtime b9254) + persistent server + Settings declutter (2026-05-20)

**Landed (compiles + clippy + tests; closes the V16.7 activation prerequisite):**
- ✅ **Runtime bumped to b9254** (`registry.rs` `llama_runtime_x64`), verified to
  ship `llama-mtmd-cli.exe` + `llama-server.exe` + `mtmd.dll`. The auto-installer
  re-fetches when the stale b4404 runtime is detected (sentinel present but
  mtmd-cli missing), so it self-activates on next launch. Fixes the toast.
- ✅ **Persistent `VlmServer`** (`models/vlm_server.rs`) — `run_deep_analyze_batch`
  loads the model once via `llama-server.exe` and serves all files (~1-3 s/file),
  CLI fallback retained.
- ✅ **Settings decluttered** — removed the pure-doc "Models" card + the disabled
  "Performance profile" placeholder.

**Blocking hardware verification (a compile can't prove these):**
1. **Runtime auto-activation.** Rebuild + relaunch on the user's box (which has
   the stale b4404). Confirm the auto-installer logs `[VULKAN-AUTO] … stale … —
   reinstalling`, downloads b9254, and `Models\llama.cpp\llama-mtmd-cli.exe`
   appears. Then Deep Analyze a single image → caption succeeds (no toast).
2. **Persistent-server multimodal.** Run "Analyze all" on a small folder. Confirm
   `[VLM-SERVER] persistent server up` in `engine.jsonl`, the server answers
   `/v1/chat/completions` with an image for Qwen2.5-VL, and `SELECT COUNT(*) FROM
   tags WHERE source='vlm'` climbs. If the server 400s on the image payload,
   check the `image_url` data-URI format against b9254's server API (the one
   unknown I couldn't test from the build host).
3. **No orphan `llama-server.exe`** after the job completes / is cancelled /
   the engine exits (kill_on_drop should handle it — verify in Task Manager).

**Optional follow-ups (NOT done — flagged for a decision):**
- **CUDA runtime bump.** Left `llama_runtime_cuda_x64` at its old pin: the VLM
  uses the Vulkan dir (`VlmRunner`/`VlmServer` probe `Models\llama.cpp\`), and the
  current b9254 CUDA build splits `cudart` into a separate zip, so bumping it
  needs the cudart handled too. Vulkan runs on the RTX 2060 fine. Only worth it
  if CUDA-accelerated VLM is wanted.
- **Settings: fuller macOS parity.** A bigger pass could collapse the Windows
  diagnostics (CPU/Mem/GPU/Power/thumbnail) under an "Advanced" disclosure like
  macOS, and trim the 3 extra Behavior toggles macOS lacks (Hide-unknown,
  Restructure-tree-diff, Auto-chain-Deep-Analyze). NOT done this round — those
  are *functional* controls; deleting them needs user confirmation, and the
  WinUI render can't be visually verified from the build host.

## V16.7 — VLM tagging implemented; runtime bump is the activation step (2026-05-20)

**Landed (compiles + tests; reuses the existing Deep Analyze pipeline):**
- ✅ VLM scene/content tags written as `source='vlm'` during Deep Analyze
  `Both` mode (`pipeline/deep_analyze.rs` `analyze_file` + `parse_vlm_tags` +
  `models/vlm.rs::TAG_PROMPT`). ReadStore surfaces + prefers them. CLIP
  (`source='auto'`) and VLM tags coexist; VLM leads the chip slice.
- ✅ One-line CLIP kill switch: `scene_vocab::ENABLE_CLIP_SCENE_TAGS` (set
  `false` to drop CLIP scan-time tagging entirely — VLM tags then lead
  unchallenged; no other code change needed).
- ✅ `VlmRunner::find()` now emits an accurate "runtime too old — update it"
  error when a stale-but-present runtime lacks `llama-mtmd-cli.exe`.

**ACTIVATION PREREQUISITE — VLM cannot run until the llama runtime is bumped.**
The runtime is pinned to **b4404** (`registry.rs` `llama_runtime_x64` /
`llama_runtime_cuda_x64`), which ships `llama-server.exe` + the per-model CLIs
but NOT the unified `llama-mtmd-cli.exe` this code drives, and predates
Qwen2.5-VL. So Deep Analyze AND VLM tagging both fail until the runtime is
current. To activate (do this with the ability to verify a download — I did NOT
blind-guess a URL):
1. Find a current llama.cpp release that ships `llama-mtmd-cli.exe` in its
   `*-bin-win-vulkan-x64.zip` (and a CUDA `*-bin-win-cuda-*-x64.zip`). Verify
   by downloading + listing the zip.
2. Bump both `url:`s in `registry.rs` (vulkan: `llama_runtime_x64`; cuda:
   `llama_runtime_cuda_x64`). Note the vulkan entry still uses the
   `ggerganov/llama.cpp` org (redirects); the cuda entry uses `ggml-org`.
3. Force re-install: the auto-installer skips when the `.installed` sentinel
   exists, so delete `%LOCALAPPDATA%\FileID\Models\.sentinels\llama_runtime_x64.installed`
   (+ the cuda one) and `Models\llama.cpp\` (+ `llama.cpp-cuda\`), then relaunch
   (auto-install re-fires) or click Settings → Performance → "Install llama.cpp
   runtime". Confirm `Models\llama.cpp\llama-mtmd-cli.exe` now exists.
4. Verify a Qwen2.5-VL caption succeeds (Deep Analyze a single image), then run
   "Analyze all" and confirm `source='vlm'` rows land
   (`SELECT COUNT(*) FROM tags WHERE source='vlm'`).

**Perf follow-up (the original Track-3 design — optional optimization):** the
current path spawns `llama-mtmd-cli.exe` per file (model reload each time) +
adds one tag call per file, so a full-library pass is many hours. A persistent
`llama-server.exe` (`/v1/chat/completions` multimodal, load once) would cut that
to ~1–3 s/file. `llama-server.exe` ships in the runtime; build a `VlmServer`
wrapper (HTTP via the existing `reqwest` dep) and route `analyze_file` through
it. Deferred — correctness first; this is a speed optimization.

**To "simply remove CLIP" once VLM is validated:** set
`ENABLE_CLIP_SCENE_TAGS=false` (engine), optionally delete the gated scene block
in `pipeline/tagging.rs` and `models/scene_vocab.rs`. VLM tags already lead in
ReadStore, so nothing else changes.

---

## Older follow-ups (archived)

Verification queues for V16.5c and earlier (all marked landed), plus the V15.3 N1-N10 backlog and the Phase 9-11 robustness/a11y/release-engineering scope, were trimmed to keep this file to the active priorities. The full text lives in `git log shared/docs/NEXT.md`.
