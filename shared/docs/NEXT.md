# NEXT — Windows (resume here)

## 2026-06-04 (newest) — Review + land the SIX-workflow deep audit fixes (UNCOMMITTED on `main`) (RESUME HERE)

A second, larger adversarial sweep (6 serialized workflows: engine·app·perf·security + re-audit + regression-repair re-audit) fixed **~35 bugs** in the `main` working tree (layered on the prior uncommitted sweep), and its re-audit caught + repaired **4 regressions its own fixes introduced**. All headless-green (engine clippy `-D` + tests incl. +3 new; app build 0/0 + App.Tests + IpcSchema.Tests + format). **Uncommitted** — see STATE 2026-06-04 (latest) + DECISIONS 2026-06-04 (latest) + per-finding record `shared/docs/audit-2026-06-04c/`. Resume order:

1. **Review the working-tree diff (39 files) and (owner) commit + push** to `main`; confirm both GitHub workflows (windows-engine, windows-app) green. The diff is the prior uncommitted sweep + this one combined.
2. **On-hardware verify (RTX 2060 / 4 GB DirectML) — the only non-headless step.** Highest-value to confirm:
   - **People-tab edit during an auto-clustering pass** (rename/merge/mark-unknown right after a scan completes): the edit must SURVIVE (was silently discarded by the phase-3 DELETE+re-INSERT).
   - **Face clustering determinism:** re-run clustering twice on a >5k-face library → identical cluster IDs / People names (HNSW fixed seed).
   - **Restructure on a large library:** the tab must populate fast (O(n) frame reader), and **Anchor/"Keep" folders must NOT be moved** when applying (they're now dropped from the plan; confirm the Keep tile count still shows). Verify the moves that DO apply are only Tidy/Reorganize.
   - **OCR actually produces text** on a screenshot/PDF (the missing COM init meant it silently returned nothing — runtime-only; can't be headless-verified).
   - **Bulk-tag a large selection** + **scan** concurrently: no writer-lock stall (COM/sidecar + face-crop writes now off the lock).
   - Settings → restart engine after a (simulated) GPU-EP crash: the CRASHING EP is disabled, not a healthy sibling (ep_guard armed-set).
   - DebugLog still synchronous (forensic tail survives a fast-fail) — confirm app.log has the last lines after a forced crash.
3. **Deferred from THIS pass (real, documented in TRIAGE.md):** VLM server-death CLI fallback; CLIP-tokenizer punctuation (ML A/B); long-path trash manifest; wipe-vs-bulk interlock; applyRestructure outbound chunking; AppSettings lost-update refactor; Sankey "Other" drill-down; startup-auth-on-UI-thread; rename-heal FTS desync; the DebugLog durable-async perf opt (a persistent flushed StreamWriter, NOT naive batching).

## 2026-06-04 (latest) — Review + land the five-workflow bug-audit fixes (UNCOMMITTED on `main`) (folded into the newest entry above — its fixes are part of the combined working-tree diff)

A fresh exhaustive bug-audit sweep (4 audit workflows + a re-audit/critic) fixed **11 bugs** in the `main` working tree (10 files: 4 app + 6 engine), all headless-green (engine clippy `-D` + **267 tests** + fmt; app build 0/0 + **131 + 38 tests** + format). **Uncommitted** — see STATE 2026-06-04 (later) + DECISIONS 2026-06-04 (later) + the per-finding record in `shared/docs/audit-2026-06-04b/`. Resume order:

1. **Review the working-tree diff and (owner) commit + push** to `main`; confirm both GitHub workflows (windows-engine, windows-app) green. (`git diff` shows the 10 files; nothing else touched.)
2. **On-hardware verify the user-facing fixes (RTX 2060 / 4 GB box) — the only non-headless step:**
   - People AND Cleanup tabs: open + refresh repeatedly (incl. after a scan / after trash) — must NOT intermittently crash (the off-UI-thread `IsLoading`/`ErrorMessage` fast-fail).
   - Settings → **Restart engine** (and the post-GPU-pack-install restart): the engine must come back Ready, not wedge on "Starting…" with dead IPC (the stale-`Exited` race).
   - **Wipe library** right after a scan completes (while auto-clustering may be running): the People tab must be empty afterward — no ghost person cards.
   - **Restructure** on a large library (>~3.5k proposed moves): the tab must populate (was silently empty at the 1 MiB frame cap); an oversize drop now shows an `ipc_frame_too_large` error toast.
   - Suggested-merges sheet: shows "Looking…" then the result — no "No likely merges" flash.
3. **Deferred (real, out of this pass's scope):**
   - **Single-file Deep-Analyze waiter correlation** (`EngineClient.Commands.cs` DeepAnalyzeFileAsync ~406 + engine `deep_analyze.rs`): the waiter resolves on ANY `DeepAnalyzeComplete` (batch or another single-file), so it can show a wrong/false result. Needs a request/file-id threaded through the engine's single-file `DeepAnalyzeComplete` event (IPC contract change). Low sev.
   - **IPC forward-compat / determinism hardening** (no active drift today, so deferred): an unknown enum value drops the whole event (`IpcCoder.cs`); a missing REQUIRED field is silently zero/null-filled (`Dtos.cs`); the schema documents alphabetical key ordering but the engine emits insertion order (`sink.rs`). Tighten when the contract next evolves.
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
