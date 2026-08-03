# FileID — State

> Snapshot of what's working and where we left off. Update at the end of every working session.
>
> **How to read this file:** newest entry at the top. Each entry is a one-day-or-one-release summary of what landed. For *why* a decision was made, see [`DECISIONS.md`](DECISIONS.md). For *what's next*, see [`NEXT.md`](NEXT.md). For *user-visible release notes*, see [`/CHANGELOG.md`](../../CHANGELOG.md).
>
> Older entries below V15.0 are historical context — load-bearing for archaeology, not for current state. Skim if you want the journey; skip if you want the destination.
>
> **Trimmed to a lean baseline (2026-05-21).** Only the most-recent entries are kept here; everything older lives in `git log`.

## 2026-08-03 — macOS scan ETA and background-only engine release candidate

The macOS sidebar now keeps scan timing visible through discovery, tagging, and post-scan work. It
reports the live discovered-file count while the total is open-ended, changes to an explicit
estimating state when work is measurable but throughput is not yet stable, and then presents the
engine's rolling ETA. Four focused presentation tests cover counting, estimating, active ETA, and
post-scan ETA behavior.

`FileIDEngine` now embeds a dedicated `com.fileid.app.engine` Info.plist with `LSUIElement` and also
sets AppKit's activation policy to `.prohibited` before model initialization. This prevents AppKit,
ML, and document frameworks from promoting the child engine into a separate Dock application.
Local assembly and hosted macOS packaging fail if the helper metadata is absent. Runtime inspection
confirmed that FileID remains foreground while its engine remains background-only during model
prewarm, and the assembled release helper contains the expected `__TEXT,__info_plist` section.

The strict Swift validation passes 385 tests across 74 suites. Debug and release build-and-launch
paths pass, including release bundle assembly with the required `mlx.metallib`.
This is the unsigned v0.1.4 prerelease candidate; no Developer ID signing or notarization is claimed.

## 2026-08-03 — macOS release packaging now fails closed on Deep Analyze

Post-merge inspection found that the workflow-dispatched macOS artifact omitted the 96 MB
`mlx.metallib`: SwiftPM builds the app and engine but does not compile MLX's Metal kernels, and the
bundle assembler only warned when its local cache was absent. That downloaded artifact was never
published. Metal-library compilation now lives in one shared helper used by both `run.sh` and the
unsigned DMG rebuild; workflow dispatch prepares the separately downloadable Xcode Metal Toolchain;
and app assembly refuses to create a bundle without the library. Both unsigned and production DMG
paths mount the completed image and explicitly require `Contents/MacOS/mlx.metallib`.

A clean-cache local build reproduced `mlx.metallib` with SHA-256
`cf80bc0944705308a955f5537d8834512e0d0016fece1d8133491da2f346b2fb`, byte-identical to the
established cache. The rebuilt unsigned DMG has SHA-256
`ba4de72f3fbafb025ef44629095321a79b476de9e8953c1169929965f6af337f`; its image checksum and strict
bundle signature verify from a read-only mount, and its engine emits `ready` without
`deep_analyze_unavailable` against an isolated database. The separate security review remains
skipped at the owner's direction.

## 2026-08-03 — Final production artifact and real-data acceptance

The final macOS strict build passes 381 tests across 73 suites with complete concurrency checking
and warnings as errors. The Windows/shared Rust engine passes 684 normal tests plus both explicit
million-file scale tests; CLI passes 65 unit tests, 14 smoke tests, and both scale tests; TUI passes
111 normal tests and its million-row test. Strict Clippy, formatting, `git diff --check`, all four
offline lockfile audits, and the existing non-security policy and packaging gates pass.

The versioned `FileID-v0.1.3.dmg` was rebuilt from release binaries and is checksum-valid. The app
and nested engine verify from a read-only mounted DMG, and launching that mounted app starts both
`FileID` and its bundled `FileIDEngine` with the single colocated `mlx.metallib`. The release wrapper
is executable again. The artifact remains deliberately ad-hoc signed and unnotarized because no
Developer ID Application identity or notarization profile is installed; it is suitable only for a
clearly labeled unsigned prerelease.

A fresh read-only Adlon acceptance folder contained 15 DOCX, nine HTML, and one PPTX source file.
All 16 supported Office documents scanned successfully, persisted extracted document text and text
embeddings, and representative DOCX/PPTX files completed packaged Qwen3-VL 8B Deep Analyze with
descriptions, grounded proposed names, and VLM tags. The exact source metadata fingerprint remained
`4f1bdb2a5d40ad8fc8667faa0fdf3dbe2177817ef294127769999ddc16631bb8`; no Restructure apply or other
source mutation was issued. The separate security review was skipped at the owner's direction and
is not claimed by this entry.

## 2026-08-02 — Production polish: all-file macOS analysis, terminal UX, and release packaging

macOS scan tagging now derives up to eight deterministic RAKE-style keywords from bounded PDF and
document text, matching the Rust engine's limits and deterministic tie ordering. Deep Analyze uses
persisted or freshly extracted bounded text for PDFs, Word, PowerPoint, Excel, and plain documents;
combines it with a bounded native preview when available; quotes file text as untrusted JSON data;
and falls back to grounded text-only descriptions, names, and tags without inventing visual claims.
MTS/M2TS route through video keyframes. Audio and 3D files remain first-class targets, and an empty
metadata-only result can no longer establish full-analysis completion.

The macOS security follow-up contains OBJ material references to regular files beneath the model
directory, redacts database-open paths from user-facing IPC, aligns Deep Analyze counts with every
engine-supported kind, bounds SoundAnalysis by wall-clock time, cancels timed-out Quick Look work,
and removes the unreliable claim that Finder's Undo owns FileID trash operations. Corrupt PDF and
presentation inputs remain retryable. Focused traversal, timeout, completion, counting, prompt-
injection, extraction, and file-matrix regressions accompany the changes.

The CLI rejects ambiguous or previously ignored flags, bounds query result counts, deduplicates
model selections, sanitizes terminal control characters, and waits for authoritative face
clustering after a full scan. The TUI now keeps all six tabs reachable at 80 columns, meets the 4.5:1
normal-text contrast floor for muted labels, adds page navigation and truly modal help, displays
persisted Deep Analyze results read-only, uses an isolated scratch library by default, and also
waits for face clustering before reload. Both platform READMEs and the root README now document the
actual release artifacts, database precedence, compatibility model IDs, key map, safety gates, and
Swift/Rust engine split.

Release packaging now ships only the colocated `mlx.metallib` that MLX loads first instead of a
duplicate 96 MB copy. Both unsigned and production scripts stage outside Desktop/FileProvider,
verify the app seal before imaging, validate the DMG checksum, mount the completed image, and verify
the app inside it. The ad-hoc v0.1.3 dry run passed; its mounted app copied to `/tmp` launched both
FileID and FileIDEngine. The local test copy was moved recoverably to Trash. This does not claim
Developer ID signing or notarization; no such identity/profile is configured.

Final local evidence: strict Swift concurrency/warnings-as-errors passes 376 tests in 73 suites;
the shared Rust engine passes strict Clippy plus 683 library tests and two manifest tests (two
explicit scale tests ignored in the normal run); CLI passes 65 unit + 14 smoke tests and both
explicit scale suites; TUI passes 111 normal tests and its million-row suite; all four shipped
lockfiles resolve patched `event-listener` 5.4.2. Rust formatting, `git diff --check`, binary privacy,
shell syntax, DMG checksum/signature/launch, model-license, supply-chain, workflow-pin/permission,
and current-document gates pass. .NET and native Linux GTK builds remain hosted/native-platform
gates because those SDKs are not installed on this Mac. Adlon stayed read-only throughout; its live
library remains visible at 180 healthy rows out of 181 indexed files.

## 2026-08-02 — macOS UI, tagging, and Qwen3-VL 8B quality follow-up

Cleanup is pinned to the top of its tab in the production app. The five-stage sidebar track now uses
one geometry model for both dots and line segments, so its animated fill ends exactly at each dot
center without overshoot; regression tests cover all five endpoints. File-preview tagging exposes a
visible gold **Apply tag** button, disables it for blank input, and maps Return to the same action.

RAM++ now advertises the static Core ML input shapes used by the shipped model. Four bounded workers
remain the measured optimum: the copied-Adlon benchmark improved from 27.73 s to 22.59 s, and every
one of the 222 emitted tags and scores remained identical. Higher concurrency was rejected because
it increased memory pressure without improving the stable four-worker result.

The Apache-2.0 `lmstudio-community/Qwen3-VL-8B-Instruct-MLX-4bit` revision
`a0afc48efd9308fb14b4d58bbd49d382f7d4f845` is now the 16 GB recommendation; its exact download is
5,776,636,403 bytes. Across six copied Adlon images, 4B completed in 30.92 s at a 4.8 GiB peak
footprint and 8B completed in 47.49 s at 7.2 GiB. The 8B answers were generally more concise and
grounded, while 4B remains the right 8 GB choice. Deterministic generation and output repair remove
unsupported identities and uncertain OCR; the final guard case produced the factual caption “Two
boys sit side by side on a bench inside a building with large windows, smiling at the camera.” and
filename `boys-sitting-bench-windows`, with no invented name.

The final strict Swift run passes 354 tests in 70 suites with complete concurrency and warnings as
errors. The production 0.1.3 bundle rebuild passes with `mlx.metallib`, and live screenshots confirm
the Cleanup alignment and active-dot endpoint. The user database remains healthy: 181 files, 180
tagged files, 1,484 automatic tags, zero face-verification rows, and `PRAGMA quick_check = ok`. Every
indexed Adlon path remains present with its cataloged size and modification time unchanged; all
mutation-capable benchmarks ran only against explicit local copies.

## 2026-08-02 — Native macOS parity hardening and Adlon acceptance

The latest GitHub state was fetched before work began; `origin/main` remained at `f96a6e1`, with no
newer Windows changes to integrate. macOS now uses one bounded full-source ImageIO decode for scan
tagging, face crops, and Deep Analyze instead of accepting low-resolution embedded JPEG previews.
Discovery reports useful early progress, completed scans stop emitting terminal progress, and both
ordinary and disk-backed Restructure planners omit case-insensitive source-equals-destination no-ops.

The native Adlon pass detected 226 faces, retained and SFace-embedded 174, assigned 171, and produced
29 visible people. Representative cards display real face crops. Merge suggestions reject clusters
whose members co-occur in one file, and “Different people” verdicts persist through re-clustering by
stable face anchors. macOS now matches Windows by resolving the suggestion only after the engine
confirms that write; stale anchors and engine failures remain visible with their returned error. The
two verdict rows created by accidental UI clicks during validation were identified and removed; the
final `face_verifications` count is zero and SQLite `quick_check` is `ok`.

Deep Analyze now defaults 8–16 GB Macs to the Apache-2.0 Qwen3-VL 4B MLX quant after a native Adlon
A/B showed stronger grounding than Qwen2.5-VL 7B with a smaller memory footprint. Both the Swift MLX
and Rust llama.cpp paths enforce trusted-year grounding and a 3–5-word filename contract, retry once
with a stricter image-grounded prompt, then decline to invent a name. A live Qwen3 rerun of
`PC230007.JPG` persisted a factual makeup/face-paint caption and
`makeup-artist-boy-ram-sweatshirt`; the Library preview refreshes immediately after terminal analysis.
The live preview now case-insensitively deduplicates VLM and RAM++ labels while preserving their
separate database provenance; the matching Windows presentation source applies the same rule.

Restructure produced a read-only 27-action Adlon plan (16 tidying, 11 reorganizing, zero staying-put
no-ops). macOS now binds that plan to the active scanned folder, matching Windows and the engine
contract, rather than offering an unrelated destination picker. Apply was not invoked. The exact
corpus path/size/mtime fingerprint remained
`e1e52c67d0d93e45704284aa17868fab9bd3885c84b2e9207adc5c79ac44e58f`, so no Adlon file was renamed,
moved, resized, or retimestamped.

The process-spawning cancellation test is now database-isolated. Before that guard, local test runs
had inserted 334 temporary corrupt-JPEG rows and eight `FileIDCancelTest-*` sessions into the live
catalog. A SQLite backup was taken, only those exact test artifacts were removed, and the restored
library contains 181 Adlon rows (180 healthy, one explicitly corrupt) with `quick_check = ok`.

Native verification passes the strict macOS suite (348 tests, 68 suites), production build and
launch, Windows Rust format and strict Clippy, both complete engine test targets (670 and 682 tests;
only explicit performance benchmarks ignored), and `git diff --check`. The small WinUI tag-parity
mirror could not be compiled on this Mac because the .NET SDK is unavailable; its source regression
is ready for the normal Windows/hosted gate. Signing/notarization,
clean-machine installation, Windows GPU runtime, and ARM64/other-hardware matrices remain external
gates and are not inferred from this Mac.

## 2026-08-02 — Identity-safe People reduction and final real-data quality pass

People no longer hides any active cluster by size on Windows, macOS, or Linux. The full 164,518-file
Adlon catalog contains 193,133 quality-eligible faces: 166,266 are assigned across 2,215 active
clusters and 26,867 remain unmatched. Controlled recovery runs at `0.75`, `0.70`, and `0.60` yielded
the identical partition and membership digest. Exact-capture and exact-embedding analysis found no
further cluster that could be removed without inferring identity, so production remains at the
measured-safe `0.75` threshold instead of hiding evidence or spending identity precision.

Merge suggestions are now explicitly review-only. Every platform suppresses people who co-occur in
the same source file, excludes rejected faces, bounds the review list to 50, displays neutral numeric
similarity, and removes bulk “likely/all” merge actions. Linux now consumes the shared engine result
instead of retaining and comparing up to 100,000 face vectors in GTK. macOS reads one persisted
centroid per person, reducing peak memory, and Windows invalidates every stale pair involving either
endpoint after a merge.

Deep Analyze disambiguates repeated proposed names with the sanitized source stem on the shared Rust
engine and macOS. Repeated-document prompts ask for a visible date, name, or reference. The final
real Mistral report `.ralph/adlon-final-quality-20260802m-deep-audit/summary.json` is GREEN with zero
failed checks across image, video, PDF, typed-error, cancellation, skip-existing, partial/full
upgrade, content, persistence, fingerprint, and SQLite-integrity oracles.

Restructure's final read-only report
`.ralph/adlon-final-quality-20260802i-restructure-audit/summary.json` is GREEN with zero failed
checks. Cancellation is typed and bounded; two runs produced the identical 15-move review-only plan
with collision-safe destinations, exact source/file identity, stable ordering, preserved drive
fingerprints, and an intact database. No apply command or file mutation was issued.

Windows ONNX Runtime sessions now skip absent accelerator packs and go directly to the first provider
that can bind; routine optimizer output is held to error level. The native staged Release scan of
`F:\Kyle\File Cabinet` discovered 1,173 files, processed 938 supported items, produced 1,186 faces
and 48 person clusters, exited cleanly, and had no WER dump or unmatched `[APPLY:N]` scope. The
automation now waits for authoritative face-clustering completion and can target an explicit
published `FileID.exe`. The app also waits for the engine's `Ready` event before requesting status,
closing the deterministic cold-start write-before-ready race; the rebuilt Release scan completed
with zero premature-status warnings, provider failures, optimizer warnings, or engine errors.

Local final gates pass on Rust 1.90: Windows strict Clippy and the complete engine suite; Windows
format plus 453 App and 53 IPC tests; the 71-test shared repository policy suite; and WSL engine
format/Clippy/release/lib tests (690 passed, two ignored), CLI (61 passed, two ignored), TUI
(104 passed, one ignored), and GTK build/strict Clippy. Engine and GTK Linux binaries are
privacy-clean. Native macOS compilation, hosted matrices, signing/notarization, and clean-machine
install behavior remain external gates and are not inferred from Windows or WSL.

## 2026-08-01 — Final face containment, Adlon acceptance, and native release polish

The People overload fix is now mirrored across Windows, macOS, and Linux. Unnamed clusters with
fewer than 13 active faces are omitted from the primary grid while named and explicit Unknown
groups remain visible at every size. Each UI discloses the withheld count and keeps those groups
available through its Show/Hide control without deleting clustering evidence. Name detection uses
the trimmed legacy name plus every structured name component.

Automatic clustering treats separate detections from the same physical file as cannot-link
evidence through both identity passes, consolidation, and fragment recovery. Strongest edges are
processed deterministically, constraints propagate transitively, and unprotected automatic
clusters enforce a conservative `0.15` centroid-similarity outlier floor. Named, manually merged,
and verdict-backed identities bypass that suppression so automation cannot silently reverse user
evidence. Deterministic similarity/index tie breaks remain in face and semantic-neighbor selection.

A proposed anti-correlation split was rejected by a controlled full-catalog A/B. With that split,
the unchanged validator went RED: raw groups rose to 2,426, visible People cards to 1,220, the
largest cluster to 19,411, and top-cluster minimum median cohesion fell to 0.5595. Removing only that
split restored every oracle. The final pipeline therefore keeps its calibrated mean/variance
validation rather than adding label-free pair probes that fragment real identities.

The exact final Rust 1.90 engine at
`.ralph/target-rust190-consolidated-final/release/FileIDEngine.exe` has SHA-256
`8a4c96991fb476cc30b10fdd2744569bcd9e8e4ae3b96fd722bb344a34a55540`. The authoritative isolated
face report `.ralph/adlon-face-validation-20260801-consolidated-final-audit3/summary.json` is GREEN
with zero failed checks and has SHA-256
`8db30aaaed19c55fc15bbde4d6776d2aeae44fe739f94c3169a9b26d1f15d969`. Two clustering passes
completed in 441.29s and 432.89s with identical partition digest
`63cbf668d47ef35186f96924b97d22d14fc821666a7f51fcee29a5c1e1c1e4fa`.

The 164,518-file Adlon catalog contains 193,133 eligible face prints. The final partition has 2,215
raw groups, 1,074 visible People cards, 166,266 assigned faces, 26,867 unmatched faces, and a
14,645-face largest cluster (8.81% of assignments). High-similarity fragment risk is 433, below its
438 baseline. DirectML ran on the RTX 5080 with ONNX Runtime 1.22.0; provider selection, runtime
hashes, face-crop membership, the seed catalog, the read-only `F:\Music` fingerprint, and SQLite
integrity all passed their post-run checks.

The separate Restructure report
`.ralph/adlon-restructure-validation-20260801-consolidated-final-audit2/summary.json` is GREEN with
zero failed checks and SHA-256
`5f645ed1fbf0c6454cb0de6dc35a8973a1ed61369127c98986e3c4054221a116`. Cancellation emits the
typed `plan_restructure_cancelled` terminal, then two normal runs produce the same safe nine-move
inline plan (`planID = null`) with canonical digest
`6e9001d40fe1267822a62b988ff7f2fe5a5abeae1244308020ec2ba932166127`. No moves were applied.

Final Windows Rust gates pass 709 tests with only the expected ignored destructive/performance
cases; the explicit million-row reconciliation gate also passes. The Windows app builds with zero
warnings/errors, format is clean, App tests pass 453/453, and IPC tests pass 53/53. Native WSL Rust
1.90 gates pass for the engine (687/689), GTK app (57/57), CLI (61 total, two intentional ignores),
and TUI (104/105); every shipped Linux binary is privacy-clean. Repository policy, runtime egress,
model licensing, Cargo deny/audit, Flatpak offline-source generation, package-tool tests, all 11 TLS
roots, Python/JSON/YAML parsing, and PowerShell 5.1/7.6 parsing are green. The full hosted macOS,
Flatpak, Windows matrix, installers, release assets, signing, and notarization remain remote or
external gates and must not be inferred from these local checks.
## 2026-08-01 — macOS presentation correctness and cross-platform verification

- **Tag chips now rank correctly.** Generic labels are excluded before the per-file top-two window rank, preserving useful tags such as `sunset`; the preview applies the same source priority, trimming, and suppression policy. SQL orders its ranked result explicitly, so tag chip order is deterministic.
- **Named small people are never hidden.** The six-face display floor recognizes every structured name field, including title, middle name, last name, and suffix. A cluster named only `Doe` now remains visible and is excluded from the hidden-cluster count.
- **Factory Reset promises its real scope.** It clears FileID's Application Support data and FileID-managed models, while intentionally retaining shared Deep Analyze Hugging Face downloads rather than deleting a shared user cache.
- **Regression coverage.** Added `ReadStorePresentationTests` for both tag-ranking and structured-name scenarios. With Xcode 26.6 installed, `swift test --no-parallel -Xswiftc -strict-concurrency=complete -Xswiftc -warnings-as-errors` passes all 336 tests in 68 suites. The Windows Rust engine remains clean: Clippy passes and `cargo test` passes 660 tests (two intentional benchmarks ignored).
- **Native launch.** `bash run.sh --no-wipe` built the production `FileID.app`, compiled and cached the 96 MB MLX Metal library, and launched both the app and child engine successfully. The welcome sheet was inspected through macOS accessibility plus a screenshot; no model download or preference mutation was performed.

## 2026-07-30 (Part 2) — Global Cancel Fix & Native Uninstaller (Factory Reset)

- **Native Uninstaller / Factory Reset**: Instead of providing a script for complete removal, a native "Factory Reset & Quit" button was added to the `Settings → Advanced` Danger Zone (`SettingsView.swift`). It executes `engine.factoryResetAndQuit()` (`EngineClient.swift`) to kill the backend, erase the entire `~/Library/Application Support/FileID` directory (destroying SQLite data, caches, logs, and FileID-managed ONNX weights), purge `com.adamnolle.FileID` UserDefaults, and gracefully terminate the process via `NSApplication`. Shared Deep Analyze Hugging Face downloads are deliberately retained.
- **Global Cancel Dispatch Fix**: The main UI `Cancel` button (in `Sidebar.swift`) was updated. The previous implementation exclusively emitted `.cancelScan` — wedging the app if a user attempted cancellation mid-Restructure, Deep Analyze, or clustering phase. The fixed `EngineClient.cancel()` now universally broadcasts `.cancelScan`, `.cancelRestructure`, `.deepAnalyzeCancel`, and local `cancelAutoPilot()` together. The FileID backend naturally ignores events invalid for the current state.

## 2026-07-30 (Part 1) — macOS Tag Parity, Anti-Correlation Face Partitioning, Hardware-Adaptive ANE Scaling, & Zero-Warning Production Build

- **macOS Parity & Tag Chips**: Added `tags: [String]?` to `FileRow` in `DBTypes.swift`. Updated `ReadStore.swift` (`toFileRow`, `tags(forFileID:)`, `topVisionTagsBulk`) to query and prioritize user (`source='user'`), VLM (`source='vlm'`), and auto (`source='auto'`) tags, filtering generic suppressed noise tags and displaying tag chips across Library grid tiles and preview detail views.
- **macOS People Grid Noise Floor**: Implemented `minFaces = 6` filtering query in `ReadStore.swift` and `PeopleView.swift`, filtering out small 1-5 photo burst fragments while retaining user-named people and disclosing hidden count with a toggle.
- **Strict Anti-Correlation Face Partitioning**: Enforced negative pairwise cosine splitting ($\text{cosine} < 0.0$) in both Swift (`IdentityClustering.swift`) and Rust (`identity_clustering.rs`) to eliminate over-merged mega-clusters while preserving 1:1 identity precision.
- **Engine Quickselect Determinism**: Fixed `select_nth_unstable_by` comparators in `face_clustering.rs` and `restructure_semantic.rs` to include a secondary index tie-breaker (`.then_with(|| a.idx.cmp(&b.idx))`), guaranteeing cross-platform clustering determinism.
- **Hardware-Adaptive ANE Concurrency**: Added `Hardware.defaultInferenceConcurrency` (`Hardware.swift`), dynamically scaling model inference concurrency (4 to 12 parallel slots) based on Apple Silicon performance cores across `MobileCLIPService`, `RamPlusService`, `ArcFaceService`, and `BGETextService`.
- **UI Layout & Motion Polish**: Stabilized grid tile heights in `LibraryView.swift` (`.frame(height: 18)`) and added signature spring motion (`response: 0.35, dampingFraction: 0.78`) to sidebar navigation tabs in `Sidebar.swift`.
- **Zero Compiler Warnings**: Resolved all unused variable and struct field warnings across `FaceClustering.swift` and `trash.rs`.
- **CI Strict Concurrency**: Updated `.github/workflows/macos.yml` to include `-Xswiftc -strict-concurrency=complete -Xswiftc -warnings-as-errors`.
- **v0.1.2 Release & macOS DMG**: Built and verified clean `FileID.dmg` locally (32 MB), verified `cargo test` (660 passed / 0 failed, 100% green), and updated the `v0.1.2` GitHub Release asset.

## 2026-07-29 — Face over-detection, Restructure apply trust, and Deep Analyze folder exclusion: audit + fix session

A full audit (adversarial-verified, source + real-catalog measurement against the 2026-07-29 Adlon
scan: 135,740 files, 183,230 detected faces, 3,108 person clusters) drove a same-day fix pass across
the shared Rust engine, all three GUIs, and macOS. The owner's four complaints — too many/invisible
leftover faces, unclear apply affordances, no way to exclude folders from Deep Analyze, and
Restructure/Deep Analyze correctness — are addressed as follows. See `DECISIONS.md` for the *why*
behind each; `RESTRUCTURE.md` §8 for the now-documented IPC 1.2 apply/undo contract.

**Faces.** The only face-size gate was relative-to-image-area (scale-invariant, so uncorrelated with
actual visibility) — it kept unrecognizable crowd blobs from low-res video while discarding
well-resolved 100px+ faces on high-megapixel photos. Added an absolute `FACE_MIN_BBOX_MIN_DIM_PX =
40.0` floor. The first pass picked 64px off a POOLED sweep; the follow-up audit caught that as a
regression and a per-source-kind re-measurement corrected it — decode resolution is not uniform
(video keyframes cap at 1280px, stills decode to [2048,4096), so video-face min-dim p50 is 52px vs
159px for images), and 64px would have silently destroyed clustering in 30% of face-bearing videos
while looking like a +18,018 win on aggregate. 40px is +28,051 image faces, +134 video faces, and
wipes only 29 persons entirely (each holding <=8 faces, i.e. noise). Mirrored to
macOS (new `faceMinDimPx` threaded from `VisionWorker` through `DBWriter`), with the cross-platform
decode-resolution caveat documented (Windows decodes near-original resolution; macOS's
`FILEID_SCAN_MAX_PIXELS` default caps at 1536px, so 64.0 is a stricter relative gate there — flagged
for the owner to re-tune on real Mac hardware, not silently "fixed"). Deleted the dead
`FACE_QUALITY_FLOOR` check on Windows (a macOS-Vision-scale constant applied to SCRFD's product
score, analytically provable to never fire — 0 of 183,230 real rows tripped it); kept intact on
macOS where it's real. Also wired `face_prints.excluded` into Restructure's `face_count` query
(previously read by exactly one query in the whole engine, silently under-filing 19.2% of files
that had an assigned face). **Not fixed, deliberately deferred:** the separate mega-cluster problem
(16 clusters absorbing 83,381 faces via kNN connected-components chaining) needs labelled ground
truth to calibrate against, not a blind threshold change; the People-tab UI still has no
minimum-cluster-size filter to make 3,108 raw clusters reviewable — both are in `NEXT.md`.

**Restructure apply trust.** Fixed real correctness bugs, not just cosmetics, alongside moving each
platform's apply bar to the top of the tab (prominence, the owner's explicit ask):
- Engine (shared Rust): a stale row (file deleted/renamed since planning, or a destination that
  collided in the meantime) used to abort the ENTIRE apply via `?` — zero files moved, reproducible
  forever since the spool wasn't cleared. Split preflight into structural-fatal vs per-row-advisory
  outcomes; `apply_iter_with`'s existing graceful skip already handled the identical conditions.
  Also fixed a DB-reconciliation-failure double-count (`applied += 1; failed += 1` for one row,
  diverging from macOS's documented single-count convention) that left `undo_last`'s
  journal-removal gate permanently unsatisfiable — "you can put them back" offered forever, always
  re-failing. The paired fix (stop counting it `failed` *and* make `reconcile_pending_path_updates`
  retry instead of discarding its recovery record after one pass) closes both halves; a genuinely
  unfixable UNIQUE conflict between two live rows is still a known, correctly-recorded gap.
- Windows: cancelled-undo now correctly keeps the Undo button live (was gated on `Failed > 0` only,
  ignoring `Cancelled`).
- Linux (agent-verified via WSL: clippy/test/fmt clean): same cancelled-undo fix; shortcut-mode
  apply now has a real undo path (token was hardcoded `None`); the "all N moves selected" label no
  longer lies about a truncated plan applying only its Auto tier.
- macOS (agent-implemented, unverified — no Xcode here): `applyStoredPlan` was applying EVERY
  confidence tier for a truncated plan (the most severe of the three platforms' bugs — real
  Review/Ask moves left on disk with no review), fixed to Auto-only with a stored-plan-version bump
  for clean migration; added the previously-entirely-missing `cancelRestructure` button (engine
  plumbing existed, UI didn't); `useSymlinks`/`shortcutUndoToken` now fail closed instead of
  silently performing/undoing real moves (macOS has no symlink-apply mode).

**Deep Analyze folder exclusion.** Didn't exist in the IPC contract at all. Added
`deepAnalyzeAll.excludedFolders` (schema 1.2.0 -> 1.3.0) as a list separate from scan exclusions
(different question: "catalog/search this but skip the VLM pass" vs "never touch this folder"),
applied only to whole-library runs (an explicit file selection is never filtered), matched with the
same separator-terminated prefix-range technique as scan exclusions (not a bare `LIKE prefix%`, so
excluding `/Photos` doesn't also exclude `/PhotosBackup` — proven by a dedicated boundary test).
Windows and Linux fully implemented with Settings UI (add/remove folder, live persistence) and
verified; macOS delegated to a background agent (protocol field, GRDB-side filter, Settings UI,
call sites) — check its report before relying on it, unverified pending a real build.

**Second audit round (same day).** A full top-to-bottom adversarial audit of the entire uncommitted
change set ran after the work above and produced 27 confirmed findings, all triaged. Fixed: the face
floor per-kind regression (above); macOS's cancelled/partially-failed undo permanently hiding "Undo
last run" (`app/.../EngineClient.swift` — the same bug Windows/Linux fixed earlier, made REACHABLE on
macOS for the first time by the new Cancel button, and unrecoverable there because the flag is never
re-seeded from disk); a leaked `DestinationClaims` reservation letting one raced destination fail a
second legitimate row (`restructure_apply.rs`); `reconcile_pending_path_updates` deleting every
pending recovery record when the library volume is simply unmounted; the Linux apply confirmation
dialog authorizing the FULL plan total when only the Auto tier moves; Linux status feedback rendering
below a multi-thousand-row list after the apply bar moved up; an inert-but-visible Linux Cancel button
after an engine error; Linux exclusion dedupe folding case on a case-sensitive filesystem (and the SQL
side's `COLLATE NOCASE`, now Windows-only); missing engine-side enforcement of the schema's
`excludedFolders` 256 cap; and several comments that misstated when `reconcile_pending_path_updates`
runs (engine start, not "next scan") — the stated safety argument for the counter change. Also fixed a
new macOS IPC test that asserted nil optionals are omitted from the wire: `IPCCommand.Payload` uses
synthesized Codable with no custom `encode(to:)`, so it very likely writes explicit nulls, and the
assertion would have failed on the owner's first Mac build for a reason unrelated to the feature.
Three CI policy gates that were RED are now green: `check_current_docs.py` (ARCHITECTURE.md/SHIP.md
still said migrations v19, now v20) and `check_runtime_egress.py --known-blockers` plus its 23-test
self-test (14 digests refreshed deliberately after verifying zero added network capability in every
drifted file, and two false-positive boundary files reviewed line-by-line and allowlisted — see
DECISIONS.md, which records what that refresh does and does NOT assert).

**Verification.** Rust engine: `cargo clippy --all-targets -D warnings` clean, `cargo test` 692
passed / 0 failed / 3 ignored, `cargo fmt --check` clean. Windows: `dotnet build` 0 warnings/errors,
446 App.Tests + 53 IpcSchema.Tests passed, `dotnet format --verify-no-changes` clean for every
touched file (103 pre-existing ENDOFLINE errors in 3 untouched files are unrelated repo drift, not
introduced this session). Linux: `cargo clippy`/`cargo test` (57 passed)/`cargo fmt --check` all
clean via WSL Ubuntu. macOS: source-reviewed by hand (types/call-sites cross-checked, brace-balance
verified) but not compiled — genuinely unverified until the owner runs a real Xcode build.

Resume in this order:

1. **Get a real macOS build.** This is the one platform nothing here compiled. Open Xcode, build,
   run the Restructure and Deep Analyze flows by hand, run `swift test`. Fix whatever the compiler
   finds — two agents worked carefully but blind, and Swift 6 strict concurrency plus GRDB idioms
   are easy to get subtly wrong without a compiler in the loop.
2. **Re-scan and validate the face-size fix on real hardware.** Everything here was measured
   against the frozen 2026-07-29 Adlon catalog by direct SQL query — no actual re-scan with the new
   64px gate has run yet. Re-scan (or re-cluster against the existing embeddings if a cheaper path
   exists) and confirm the predicted +18,018/-6,429/+24,447 numbers hold, and that People-tab
   clustering quality genuinely improves for the owner, not just on paper.
3. **The mega-cluster / fragmentation clustering problem is still open.** 16 clusters absorb 83,381
   faces (kNN connected-components chaining — my own cosine-similarity measurement showed
   mega-cluster intra-similarity ~0.39, barely above inter-mega-cluster ~0.21-0.29, i.e. these
   aren't held together by real similarity). This needs labelled ground truth to safely retune
   `pass1_cosine`/`k_nn`/`AUTOMERGE_COS_DEFAULT` — guessing new numbers without it risks repeating
   the exact regression `identity_clustering.rs`'s own comments warn about.
4. **People tab still dumps all 3,108 clusters unfiltered.** No minimum-cluster-size hide, no bulk
   dismiss for junk. Out of this session's explicit scope (the owner's three directives were the
   face floor, apply-bar prominence, and Deep Analyze exclusion) but is the other half of "too many
   leftover faces" and should be next.
5. **The kNN tie-break nondeterminism flagged by the audit is unfixed.** Both
   `face_clustering.rs` and `restructure_semantic.rs`'s comparators don't consult index as a
   tiebreak, so ties can resolve differently across stdlib/arch (the newly-added single-thread HNSW
   pool, documented in `DECISIONS.md`, only fixes the graph-build nondeterminism, not this).
6. External release gates (signing, hosted CI native builds, ARM64/AMD/Intel/QNN hardware, clean-VM
   installer) remain unproven locally, unchanged from prior sessions.

Evidence: `.ralph/adlon-faces-20260729/candidate-current/FileID/fileid.sqlite` (the frozen catalog
every measurement in this entry was computed against), full `cargo test`/`dotnet test`/WSL
`cargo test` output captured in-session, and the two macOS agent reports (Restructure apply-bar,
Deep Analyze exclusion) for exactly what was changed file-by-file.

## 2026-07-28 — macOS / CLI / TUI audit: EXIF orientation parity restored, five more defects closed

A second pass covered the macOS Swift engine and app, the `fileid` CLI, and `fileid-tui`, combining source audit with adversarial verification and hands-on runtime exercise. Six real defects landed.

**The headline is a cross-platform correctness bug.** The Rust engine read EXIF only for camera model and GPS — there was no rotate or flip anywhere in its pipeline — while macOS decodes through `kCGImageSourceCreateThumbnailWithTransform: true`. Phone and camera photos are stored in sensor order with an orientation tag rather than physically rotated, so on Windows and Linux every portrait photo was perceptually hashed, CLIP-embedded, RAM++-tagged and face-detected **on its side**. The same file produced a different phash and CLIP embedding depending on which engine wrote the row, so Cleanup grouping and semantic search disagreed across platforms, and YuNet — not rotation-invariant — lost real face recall. Tag 0x0112 is now read once from the bytes both decode paths already hold and applied through the image crate's own `Orientation` table, in `decode_image_sync_imagecrate` so the DCT-scaled JPEG path is covered too. Proven on real corpus photos: a file carrying orientation=6 and an ImageMagick `-auto-orient` copy of it now yield **byte-identical perceptual hashes (hamming distance 0)**, and a two-image full-ML scan finds faces in both files where it previously found one. Incremental rescan skips on (size, mtime) with no model-version column; adding one would need a schema migration that must stay byte-faithful with the Swift engine, which would break the parity being restored — so existing libraries need one force-retag pass, while a fresh scan is correct from the start.

**macOS.** `handleEngineExit(for:)` guards on `proc === self.process` so a late EOF cannot tear down the live engine — correct, but it means a *restart* never reaches the reset block it protects, because `start()` → `terminateRunningEngine()` → `spawn()` reassigns `self.process` first. Nothing else clears that state, so Settings ▸ Restart Engine during Deep Analyze left `deepAnalyzeInFlight` latched true against a jobless engine: Analyze stayed disabled and its Cancel inert until relaunch, with `undoRestructureInFlight`, `queueState`, `isPaused` and both App-Nap tokens equally stranded. The reset is now shared by both legs, and the restart path also clears `lastProgress` (a non-idle phase otherwise kept `SidebarProcessingControl` rendering a dead Pause/Cancel pair with no reachable Start). Separately, the excluded-folder purge delegated case folding to SQLite's ASCII-only `lower()` while the needle was Swift-folded over full Unicode, so excluding `Fotos/Ärchiv` or `Документы` deleted zero rows and reported success — discovery stopped walking the folder but its already-indexed files, tags, faces and OCR stayed in the library forever; a Swift-side pass now runs for non-ASCII needles only. And `ruleClassify` truncated the whole basename through `componentSafe`, eating the extension past 200 scalars so the file stopped opening by double-click and dropped out of the library on the next scan (`isTaggable("")` is false); stem and extension are now sanitized separately, as the VLM branch already did.

**CLI and TUI.** `--json` is documented as machine-readable but both failure exits printed prose, so a failed run emitted nothing a parser could read; errors and the partial-scan warning now serialize as `{level, kind, message}` on stderr. `restructure --apply --symlinks` gated its bail on `failed > 0` alone while a privilege refusal reports through `privilege_error` and abandons the remaining moves without counting them, so on stock Windows with Developer Mode off it printed "apply complete" and exited 0 — `… --json && next-step` marched straight past a mutation that did nothing. In the TUI, `on_key` dropped the modifiers when delegating to its modal handlers, so every control chord reached their `Char(c)` arm as a bare letter: Ctrl+C in the search box appended "c" instead of quitting (it worked from every other context) and Ctrl+U appended "u". Chords are now resolved before the modal dispatch, requiring CONTROL *without* ALT so Windows AltGr text still types. The empty-results panel also advertised `Esc  clear the search` while Esc fell through to the quit arm and closed the app — following the on-screen instruction lost the session.

A second full uncapped `F:\Adlon Drive` run re-validated the changed decode path at scale and passed all 13 assertions (`ITERATE_EXIT=0`): 135,740 supported files in 4,951 s (**27 files/s** against the 25 floor) with **peak RSS 6,535 MB — lower than the 6,972 MB pre-fix peak**, so the transient rotation buffer costs no headroom against the 8,500 MB cap. Throughput is ~7% below the 4,610 s pre-fix run, but the two are not a controlled comparison: this scan shared the machine with a release build and CI. The corpus fingerprint is byte-identical to the preserved baseline for the second time, so both full ML scans left real data untouched.

Verification: engine 571, CLI 50+10, TUI 104, app 304, IPC 49, all under the CI-pinned 1.90; Linux engine/GTK/CLI/TUI green; 71 policy regressions plus every checker. The TUI was driven in a real pty — terminal state restored byte-identically, alternate screen entered and left, no panic at a 2×2 terminal, and quit reachable from every context. IPC conformance is machine-checked against the canonical schema in all three languages (Swift 2, Rust 8, C# 49). macOS work is CI-verified only (320 tests / 66 suites on Xcode 26); there is no Swift toolchain on this host. `dedupe --similar --apply` was confirmed to refuse without `--yes`, previewing 54,289 files rather than deleting. Exact dedupe is slow by design — it re-reads candidate bytes rather than trusting a stored hash — but gives no progress output for minutes on a large library.

## 2026-07-28 — Production-readiness pass: off-thread crash class closed, full Adlon scan green, docs/site truth restored

Two real defects landed. `ReducedMotion` re-raised `PropertyChanged` with a bare multicast `Invoke` directly from the WinRT `UISettings.AnimationsEnabledChanged` callback, which runs on a threadpool thread with no handler above it: the first subscriber to throw would have escaped unhandled and killed the process, and every later subscriber in the list would have been skipped, silently freezing its motion. The raise now walks `GetInvocationList()` and guards each subscriber. Separately, nine view/service handlers plus `UndoStack`'s one-shot `EngineClient` subscription had drifted out of the `DebugLog.SafeRun` convention, leaving the stowed-exception crash class open on those paths; all are wrapped, and `EventHandlerSafetyContractTests` now re-derives the rule from source (scoped to subscriptions with a receiver expression, with a floor on the number inspected so it cannot pass vacuously). The reviewed network-capable source digest for `SidebarProcessingControl` was refreshed for the SafeRun wrap only.

A full uncapped `F:\Adlon Drive` run passed every harness assertion (`ITERATE_EXIT=0`). Discovery enumerated 163,741 files; the engine processed **135,740/135,740 supported files in 4,605.6 s (~29 files/s** against the 25 floor, improving on the prior 4,813 s baseline) with **195 failed files — identical to the preserved baseline count**, so no new failure class. Peak RSS was 6,972 MB against the 8,500 MB cap and oscillated without upward drift; models released after the scan (RSS fell to 751 MB). Face clustering completed in 51.3 s over 104,958 faces into 2,728 persons with 6,373 unmatched; 156,012 face crops were written; the catalog settled at 966.8 MB with the WAL checkpointed to zero; the engine exited code 0. Every failure is benign and correctly classified — absent HEIC codec (with the user-actionable Store message), panoramas above the 50 MP decompression-bomb cap, and one undeterminable format — and log path redaction was confirmed live.

`F:\Adlon Drive` remained strictly read-only: the post-scan fingerprint is byte-identical to the preserved baseline — 163,787 files, 12,804 directories, 2,687,450,463,370 bytes, metadata SHA-256 `529f3a9f9c0cd54e5842bbd347d83f4edb6a62ac28aea5810db712a9c079725b`, the same two expected `scandir:2` observations, and 1,000 deterministic 64 KiB-bounded content samples.

Both GUIs were exercised, not just compiled. The WinUI app launched against the live catalog, spawned the engine, correctly skipped Welcome, fanned `[APPLY:1] enter/exit ReadyEvent` through every `[ENGINE-SUB:*]` subscriber with no ERROR or `threw:` line, and rendered People over the real library. The GTK app launched under WSLg, rendered all six tabs with the brand palette and reported `Engine: ready`, with no panic, `BorrowMutError`, or GTK critical. The `fileid` CLI ran full-text content search (docx/pdf/xlsx/pptx) against the 966 MB catalog while the engine was still writing it, exercising WAL reader concurrency.

Gates are green on both platforms at the CI-pinned toolchain. Windows: engine clippy/test/fmt under **both 1.90 and 1.96** (570 library + 560 binary), CLI 60, TUI 100, .NET build/format clean, App **304** (302 + 2 new contract tests), IPC 49. Linux under 1.90: engine 551, GTK app 40, CLI 51, TUI 100. All 71 shared policy regressions and every checker pass, and the shipped release engine scans privacy-clean. **Note a dev-environment hazard:** a standalone Rust 1.96 install precedes the rustup shims on this host's `PATH`, so a plain `cargo` invocation bypasses the repo's 1.90 pin that CI uses — local green is not CI green unless the shim is invoked explicitly.

Documentation and the marketing site were corrected against source rather than restyled. The site advertised the face stack as "ArcFace · SCRFD" — the non-commercial stack that was deliberately replaced — while FileID ships YuNet + SFace; it also understated the release-blocking runtime archives as six (ten), named a superseded Deep Analyze lineup, presented CUDA/QNN as product Performance Packs, claimed no installers exist, and carried a dead hero anchor (`#what`) whose scroll control did nothing. The README's Website link pointed at a `github.io` host that no longer serves the site. A Front-ends section now covers the `fileid` CLI and `fileid-tui`, which the site had never mentioned. Root `SECURITY.md` and `CONTRIBUTING.md` were added because GitHub does not read `shared/docs/`, leaving a public project with no vulnerability-reporting channel; private vulnerability reporting is now enabled on the repository.

Publication posture is unchanged: strict runtime egress still rejects the same ten reviewed GitHub/NVIDIA archives, so this remains an unsigned-prerelease state, not release approval. Native macOS, ARM64/vendor hardware, clean-VM installer lifecycle, accessibility, and public-trust signing remain external.

## 2026-07-27 — Final pre-release whole-codebase audit closed locally; strict publication blocker preserved

A final repository-wide pass audited Windows, Linux, CLI/TUI, Apple interactions, packaging, policy, and current documentation. Linux OCR/video/HEIC and all long-lived engine helpers now have bounded output, deadlines, parent-death/process-group ownership, and tree kill/reap; non-Windows Restructure `EXDEV` fails without touching the source. Scan cancellation wakes discovery/decoder stages, bounds decoder joins, propagates decoder panics, and waits for mutation quiescence before checkpoint. Windows engine spawn pins the signed image before trust verification through `Process.Start`; app commands are FIFO and generation-bound so queued destructive intent cannot cross a respawn; scan optimism/rollback and first-crash auto-scan terminals are generation-owned; explicit engine shutdown exits the stdio loop promptly and releases queue sink listeners. Restricted model terms cannot be bypassed by `--yes`, JSON, or noninteractive CLI execution. Dead UI controls/converters and the unused native-VLM feature were removed only after call-graph/compiler confirmation.

The complete clean WSL gate passes: shared engine 551 library + 560 binary active tests (plus manifest/doc targets), GTK app 40, CLI 51 unit + 10 smoke, TUI 100 active; all locked format/clippy checks and the Linux release build pass. Windows full engine format/clippy/tests pass; x64 Release app build, App 302/302, IPC 49/49, and format pass. All 71 shared Python policy/fingerprint regressions pass (one Windows symlink skip), along with model-license, bootstrap, workflow pin/permission, current-doc, and runtime-egress known-blocker checks. A shutdown frame with stdin held open exits the engine in under 0.5 seconds. Independent final validation found no remaining locally actionable blocker/high/medium code issue.

`F:\Adlon Drive` remained strictly read-only. The current 1,000-sample fingerprint exactly matches the preserved prior result: 163,787 files, 12,804 directories, 2,687,450,463,370 bytes, metadata SHA-256 `529f3a9f9c0cd54e5842bbd347d83f4edb6a62ac28aea5810db712a9c079725b`, identical extension counts/errors, and identical sampled content. The exFAT volume currently reports `Warning` / `Full Repair Needed`, reinforcing the no-mutation rule.

This is an audit-complete state, **not release approval**. Strict runtime egress still rejects the exact reviewed ten GitHub/NVIDIA archives and non-Hugging-Face redirect hosts; current artifacts must not publish until those paths are removed or mirrored and the strict gate passes. Native macOS, ARM64/vendor hardware, distro installs, clean-VM lifecycle/accessibility, public-trust signing/notarization, and hosted CI remain external. Blocking filesystem/image/document decodes are cooperatively bounded at pipeline shutdown but cannot be forcibly cancelled inside the process. No commit, push, or publication occurred. Evidence is `.ralph/baseline/final-pass-linux-gate-complete.log`, `.ralph/baseline/fileid-adlon-current-64k.json`, `.ralph/baseline/strict-runtime-egress-final.log`, and final reviews under `.pi-subagents/artifacts/outputs/{5c249c54,c4027978}/`.

## 2026-07-27 — Full Adlon throughput recovered; final mutation, framing, and parser review closed

Profiling identified Windows video decoding as the dominant full-scan cost. Media Foundation now requests an even, maximum-1280px RGB32 frame with advanced video processing, uses a bounded native fallback only when BGRA + retained RGB fit the shared 64 MiB reservation, and rejects missing dimensions before decode. The predecode budget is memory-tier-aware (192 MiB Low, 768 MiB otherwise), while OBJ parsing takes the full budget exclusively. On the same 2,006-video workload, runtime improved from 483 s to 153 s with identical tags/faces and zero failures. A clean uncapped `F:\Adlon Drive` scan indexed 135,740 supported files in 4,813 s (~28.2 files/s), above the 25 files/s floor, with the same 195 classified corrupt/codec/safety failures. Metadata, 163,787-file/12,804-directory totals, 2,687,450,463,370 bytes, extension counts, and all 1,000 deterministic sampled hashes remained identical to the pre-scan fingerprint.

Scan IPC now emits approximately one batch summary per 100 processed files instead of tens of thousands. Fresh catalogs skip irrelevant legacy rename-heal work for the entire run. Cleanup exact-duplicate loading uses one SQLite CTE/window query rather than up to 201 queries; semantic search defers metadata/tag lookup until after top-k scoring. Durable forensic logging remains synchronous by design rather than trading crash evidence for unsafe batching.

Final adversarial review closed additional edge cases. Bulk tag changes use one savepoint per file, People merge SQL and undo-snapshot failures propagate, and both bulk rename and Windows Restructure retain a verified source handle through a no-replace, destination-parent-bound rename. Restructure fails closed when indexed identity is missing, rejects symbolic-link sources, and repairs the DB on retry when an Undo move succeeded on disk but its path update failed. Office/EPUB ZIP extraction now applies entry, central-directory, ZIP64, member, and cumulative-decompression bounds to every format and rejects fake EOCD records in comments. Windows inbound IPC enforces 64 MiB of UTF-8 bytes for terminated and unterminated frames; Apple `LineBuffer` now rejects both forms as well.

Hardware/user-flow evidence remains green: People re-cluster/navigation/408-scroll stress exited cleanly with no WER or dump; two-image Mistral Deep Analyze and one-second `skipExisting` completed; and the post-review two-file Restructure sandbox applied and undid 2/2 moves, restored original bytes and DB paths, cleared the journal, and left integrity `ok`. Windows release-polish built the release-debuginfo engine, self-contained app, MSI, and Burn bundle, scanned 518 shipped binaries with zero privacy markers, and produced checksums. Rust fmt/clippy and tests, Release .NET build/format, App tests, IPC schema tests, Python policy/fingerprint tests, runtime-egress known-blocker audit, and `git diff --check` pass. Native macOS/Linux, ARM64/vendor hardware, signing, hosted CI, and replacement of the ten reviewed GitHub/NVIDIA runtime URLs with Hugging Face mirrors remain external gates.

## 2026-07-26 — Windows People crash path hardened; Deep Analyze/Restructure real-data flows and final CUDA scan green

Investigated the reported People/Faces crash against the preserved 135,740-file Adlon catalog. Historical Windows events and the retained dump identify a native WinUI stowed exception (`0xc000027b`, `CoreMessagingXP.dll` / `Microsoft.UI.Xaml.dll`), not SQLite corruption or a Rust panic. The current source build did not reproduce it. The high-risk flow was then hardened: People observable reconciliation and XAML callbacks run through `DebugLog.SafeRun`, unloaded views reject deferred/selection callbacks, deferred visual-tree work is guarded, and the banner refresh is coalesced instead of issuing roughly one SQLite query per cluster. Engine INFO diagnostics remain in `engine.jsonl`, while stderr/app-log mirroring is WARN+ only so a full scan no longer duplicates a per-file engine stream through synchronous app logging.

Final on-hardware People acceptance used the freshly published Release app: invoke Re-cluster, navigate People → Library, wait for the authoritative completion to auto-route Library → People, then drive 408 vertical-scroll mutations across the ~2.7k-card surface. Clustering took 54.92 s; the app remained responsive, peaked at 338 MB (engine 874.8 MB), closed normally with exit 0, and produced no new Application Error/WER event or crash dump. The original catalog remained SQLite-integrity `ok`.

Deep Analyze gained an approved optional, bounded `fileIDs` scope on the existing `deepAnalyzeAll` command across JSON Schema, Rust, C#, Swift, and Linux. Analyze Selected now resolves/prefilters the batch before model startup and uses one persistent authenticated loopback `llama-server`, rather than loading up to three model processes per file. Two real Adlon images completed with zero errors and persisted captions/tags/names; the same selection with `skipExisting` completed in ~1 s without launching a VLM. macOS now also resolves empty/already-analyzed selections before loading/downloading weights, and oversized selections emit the same error + completion lifecycle as Windows.

Restructure recovery now retains Undo after partial/cancelled work, keeps DB-update-failed physical moves recoverable, refuses stale/wrong-root plans, and does not expose an older journal after a failed-only attempt. Journal v2 records one exact canonical library-root header; nested-parent mismatches and legacy rootless journals fail closed by explicit owner choice. A real read-only Adlon plan produced 70,151 proposed moves in ~35 s. A final isolated two-file apply/Undo with the Release engine restored both bytes and DB paths, cleared the journal, and left SQLite integrity `ok`; original Adlon files were never used for destructive apply.

Final isolated Release scan on `F:\Adlon Drive`: CUDA on RTX 5080, worker cap 18, 1,000 files in 31 s harness wall time / 25.94 s engine scan time (~33 files/s), 3.1 GB peak RSS, 8,539 tags, 945 face prints, 26 persons. The sole failed row was an intentional safety rejection of a 10,000×6,000 PNG above the 50 MP decode cap. All 12 harness assertions passed (throughput, memory, crash/WER/dump, DB/WAL, face clustering, privacy). A preceding 3,000-file run also sustained ~33 files/s with CUDA and passed all assertions.

Verification: `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`; full Rust tests (0 failed); Release solution build 0 warnings/errors; App tests 292/292; IPC schema tests 48/48; `dotnet format --verify-no-changes`; runtime-egress/model-license/current-doc policies plus 34 policy regressions; full `build-all.ps1 -Release -Fast`; final independent review clean. Native Swift compilation was unavailable on this Windows host, and Linux/macOS native gates plus hosted CI remain required. The worktree still contains extensive pre-existing cross-platform uncommitted work and was deliberately not reset, committed, or pushed.

## 2026-07-22 — Hostile Restructure + Deep Analyze audit: 18 defects fixed incl. 2 data-loss; destructive core proven sound

After the perf + crash + install work landed, a deep adversarial audit of the two highest-stakes areas that on-hardware use hadn't exercised — Restructure (destructive file moves) and Deep Analyze (VLM subprocess). 8 hostile finder lenses → adversarial verifiers → a completeness critic re-reading the destructive paths no finding touched. 14 confirmed + 4 critic findings, all fixed on branch `fix-library-stowed-crash` (PR #136). The critic's headline negative is the confidence signal: **no proven-to-disk data-loss defect remained** — it verified no-clobber (`MoveFileExW` without REPLACE_EXISTING / `rename_no_replace`), mutation-gate FIFO serialization of scan/apply/deep-analyze (no interleaved mutation, no deadlock), killed-during-apply/undo recovery via the write-ahead journal + identity arms, wrong-root/stale spooled-plan guards, and that Deep Analyze writes proposed names to the DB only (never renames files on disk).

Fixed (see DECISIONS 2026-07-22 ×3): cross-volume moves now `MOVEFILE_WRITE_THROUGH` (was a crash-mid-move data-loss window on the default NAS/USB→local path); EXDEV path fsyncs the destination parent dir before unlinking the source; undo restores a moved-but-DB-update-failed file from journal evidence and no longer silently no-ops on a library_root mismatch; the recovery record is now actually consumed at startup; the feedback learner can't upgrade an Ask-tier move to auto-apply and the inline apply path gained the server-side ask-tier exclusion; drive-root plans compute the correct path range; symlink runs clear a stale real-move journal. App: Apply sends exactly the reviewed rows (stale-plan guard — a background re-plan can't land approvals on unseen destinations), undo terminal no longer mislabeled, "originals unchanged" softened. Deep Analyze: three unbounded hangs bounded (mid-batch reprobe 15s + cancel-aware, --version probe 20s + kill, process-wide SetErrorMode), weights-missing terminal, and a Win32 Job Object so a hard engine death can't orphan a 9-14GB-VRAM llama-server.

Verified: engine clippy clean + 1088 tests (Windows) / 1067 (WSL); app build 0/0, 287 tests, format clean; egress audit + license policy + flatpak green. 6 new regression tests (WRITE_THROUGH-adjacent undo recovery, ask-tier consent, drive-root bounds, …).

## 2026-07-21 (later) — Adversarial closure: 8 review findings fixed, engine abort eliminated, full 71k soak green

A 6-lens adversarial review (each finding independently refuted before acceptance) confirmed 8 defects — all fixed same-session: the Settings CUDA button installed NVIDIA CUDA Toolkit redists without surfacing the CUDA EULA (ModelLicenseGate now maps ort_cuda_x64 → NVIDIA-CUDA on every path); Deep Analyze wave progress no longer ticks backwards or flips the current-file display between concurrent files; wave terminals drain sibling outcomes; server death retries on the CLI; Settings no longer runs sentinel tree-walks on the UI thread at 50×/s mid-install nor clobbers a live VLM download row; no-pack NVIDIA startups no longer log five bogus "reinstall" ERRORs; Linux GTK renders the discovery-incomplete state honestly.

Separately, a real crash was caught and killed: the engine aborted (0xC0000409) 25 min into a 71k soak — ort's `tracing` feature routes EVERY native ORT log line (VERBOSE env + CUDA arena spam) through an `extern "system"` callback whose CStr asserts abort the process on a null pointer. Fixed by eager `ort::init()` clamping native severity to Warning at the source (+ `ort=warn` subscriber filter), wrapped in catch_unwind so a missing/mismatched dylib degrades to lazy init instead of killing startup (arm64 CI's stale system ORT 1.17 proved that path). Bonus: the eager init also **explicitly disables ORT's ETW telemetry**, which the pinned Microsoft gpu build ships and ort's builder leaves on by default — a silent pre-existing "no telemetry" violation.

**Final acceptance: the full uncapped Family Shared soak (71,361 files → 63,742 indexed) completed cleanly in 44 m 45 s (~23.7 files/s end-to-end incl. model load, faces, CLIP, post-scan) with 0 aborts and a clean shutdown** — vs the 2026-07-20 overnight run that projected 17+ hours and died silently on CPU. CUDA pool A/B: 1→20.4, 2→23.4 (default now, `CUDA_MODEL_POOL_MAX`), 3→17.2, 4→4.6 f/s. All gates green: engine clippy+1078 tests (Windows+WSL), app 287/287+format, egress audit, license policy, full PR CI matrix.

## 2026-07-21 — Tagging ~40× recovered: CUDA stack was unloadable (cuFFT) + non-Blackwell (cuDNN 9.5/CUDA 12.4); discovery/ETA truth + install-flicker fixed

The 2026-07-20 overnight Adlon scan (163k files, RTX 5080) ran at **0.55 img/s with a 17-hour ETA that barely moved**. Root-caused on hardware and fixed end to end; the same 60-image sample that took 106-120 s now scans in **11-14 s**, and a live 71k-file "Family Shared" soak sustains **~21 img/s through RAM++ (single-slot; pool-4 higher)** with the GPU actually loaded.

**Root causes (all verified by measurement, see DECISIONS 2026-07-21):**
1. `onnxruntime_providers_cuda.dll` could never load — its hard import `cufft64_11.dll` was shipped by no pack (measured LoadLibrary err 126). ORT registered EPs permissively, the pinned gpu runtime has no DirectML, so every session silently landed on the **CPU** EP while all logs/IPC said `ep=cuda`.
2. Even loadable, the CUDA 12.4-line cuBLAS + cuDNN 9.5.1 have no consumer-Blackwell (sm_120) kernels: 99-100 % GPU utilization at ~1.8 s/image through arch-fallback kernels. CUDA 12.9 redists + cuDNN 9.8.0 → ~10× on identical output.
3. CPU-resident sessions ate RAM → the live memory-tier probe flipped Low **between model load and cap computation** → a 4-session pool ran under 1 vision permit.
4. Discovery walk shared a 32,768-slot bounded channel with tagging: the "discovered" counter, `done`, and the app's ETA denominator advanced at ML speed (17.0 h = 32,768 ÷ observed rate — exactly).

**Engine (Rust):** CUDA pack now ships the full math closure (5 pinned archives: ORT gpu + cudart/cublas/cuFFT/NVRTC, CUDA 12.9) + cuDNN 9.8.0; `preload_cuda_math_stack` loads the exact set by full path before any session and its verdict gates `cuda_pack_present` + the ORT_DYLIB_PATH pin (fail → loud log + DirectML, never silent CPU); ort crate `tracing` feature bridges native EP-registration errors into engine.jsonl; concurrency caps derive from `ModelStack::pool_size` (the pool actually loaded); CUDA VRAM/slot estimate 3500 MB (measured); Low-tier pool clamp no longer applies to CUDA/TensorRT; `ScanProgress.total` finally honors the schema (0 until discovery completes) so ETAs are real or absent; discovery channel cap memory-tier-scaled (32 K/256 K/1 M). Deep Analyze: llama-server gets `-np 2` on ≥12 GB VRAM + wave-parallel batch driver, VLM inputs downscaled to 1568 px long edge. Registry/manifest/egress digests updated; `python check_model_license_policy.py` + both egress modes green.
**App (C#):** ModelSlot.Apply no longer knocks Installed→Downloading on stale sub-100 % events (the Welcome/Settings flicker); Settings NVIDIA buttons are Accelerator-slot-reactive and guarded against double dispatch; install watchdog re-arms instead of one-shot; clean-shutdown Reset message neutral; SentinelProbe mirrors the 5-DLL pack (+tests); sidebar shows "Counting files — N found" until the real total exists; EngineClient merges Discovered/Total forward from late Discovering events.
**Verified:** engine clippy+tests green Windows (531/531) & WSL (530/530); app build 0W/0E, 287/287 + 48/48 tests, format clean; packs installed on this box through the engine's own installer (77 s) and the scan re-verified through the shipped path with zero staging.
**Dev-box hazard:** the launch terminal exports `ORT_DYLIB_PATH` → Python 3.14's CPU-only ORT 1.27; engines started from that shell bypass the pack pin (now at least visibly warned in logs). Unset it.

## 2026-07-19 — CUDA pack install indicator fixed (cudart/cudnn size floor)

`SentinelProbe.RequiredArtifactsPresent` required `cudart64_12.dll` and `cudnn64_9.dll` to be ≥ 1 MB, but both are small CUDA/cuDNN dispatch shims (~540 KB / ~260 KB). The probe therefore reported a fully-installed CUDA/cuDNN pack as missing, so the app re-dispatched its prewarm on every Settings load / engine Ready (the 0%→100% `already_installed` spam the file header describes) and eventually surfaced "the model installer stopped reporting progress." The engine was always correct (its own sentinel + hash check passed); only the app-side size floor was wrong. Lowered both floors to 100 KB (still rejects truncated/stub files) and made the SentinelProbe test use realistic sizes (the old test used a fake 1 MB `cudart`, which is why it never caught this). App build 0W/0E; SentinelProbe tests pass.

## 2026-07-19 — Whole-diff audit closed one blocker; iterations 15–30 land to main

Independent adversarial review of the entire in-flight audit diff (iterations 15–30, ~13k lines across engine/app/apple/linux/cli + security scripts + IPC schema + CI workflows), fanned out per subsystem with every finding separately verified. One **blocker** was found and fixed: in the Rust face-clustering command, a "different people" (`same_person=0`) verdict whose endpoint face is owned by a *preserved* identity (unknown-marked, or holding any sub-quality-gate / out-of-pool face) was dropped from the partitioned pool but still iterated when building the cannot-link set and when calling `validate_protected_clusters`, so the run hit `anyhow::bail!("…left the protected partition")` and emitted `face_clustering_failed` instead of a completion — and because clustering re-fires after every scan, the People tab could never rebuild for that library. Fix: pre-filter verdict pairs touching an excluded face (a non-excluded endpoint is always re-assigned by partition), restoring the tolerant behavior the macOS mirror already had; a pipeline regression test locks the invariant. One **low** was fixed: dead `_download_exec_contract_valid` / `APPIMAGE_SCRIPT_SHA256` left by the download-exec refactor in `check_bootstrap_supply_chain.py`. Every other subsystem — the destructive Recycle Bin/restore path, zero-byte dormancy, decoder/face resource admission, GPU-reset lifecycle, VLM/runtime egress, C# generation ownership, Linux People lifecycle, and the CI/policy YAML — passed adversarial review with no confirmed defect.

Windows verified green on this machine: engine `cargo clippy --all-targets -D warnings` clean + full `cargo test` (0 failed), app Release build 0 warnings/0 errors, `dotnet format --verify-no-changes` clean, App tests 284/284, IPC tests 48/48; all shared security-script suites pass. Native macOS Swift, on-hardware GPU/codec/TDR, signing, and external mirror swaps remain the standing hosted-CI / hardware gates.

## 2026-07-19 — Linux People lifecycle and edit retention are source-closed

Linux face grouping no longer treats elapsed time or observed database rows as engine completion. The People tab retains engine readiness and one monotonically generated local attempt, rejects overlap, distinguishes a busy rejection from failure, and ends state only for exact send rollback or authoritative complete/failure/busy/process-unavailable events. Engine exit disables starts until replacement Ready. The shared engine now releases face-clustering single-flight after persistence finishes but before publishing its terminal, so terminal-driven retries cannot bounce in the prior run's post-terminal guard window; the mutation permit remains held through publication.

Person detail edits are no longer captured after the dialog has already disappeared. Done, Escape, and titlebar close all enter a `can_close=false` save barrier. The normalized rename remains in the live entry widgets through gate contention, send failure, failed or unrelated terminals, engine exit, and receiver closure; controls are re-enabled for retry. Only a matching action/person success enables and closes the dialog. Detail Mark Unknown follows the same lifecycle, and global per-action ownership safely handles ID-less worker failures.

Native WSL GTK fmt/clippy and all 38 tests pass. The engine release-before-terminal regression passes on Windows and WSL, runtime capability digests remain reviewed, and final independent reviewers report no blocker/high/medium in either scope. Evidence is `.ralph/baseline/iteration30-*`. Graphical Done/Escape/titlebar interaction remains a native UI confidence gate. Next are raw VLM GPU-removal classification, codec deadline kill/reap, macOS/docs/restructure truth, whole-diff closure, and the final repository gate.

## 2026-07-19 — Binary privacy and Windows operation ownership are source-closed

The shipping privacy gate now has one raw-byte implementation for all 23 forbidden markers. It checks ASCII, UTF-16LE, and UTF-16BE case-insensitively at any alignment, reports each marker once per file, recursively expands Windows EXE/DLL publish trees, and fails on missing or empty inputs. Windows engine/app/tool/publish paths, staged Windows App Runtime prerequisites, MSI/Burn outputs, signed macOS release binaries, direct DMG construction, Linux CI/dist/Flatpak payloads, and release/package workflows all use it. Changed shell consumers remain whole-file digest reviewed. A current Windows publish/prerequisite/MSI/Burn payload passes across 518 binaries; native signed rebuild/notarization remains external.

WinUI scan and Deep Analyze presentation is now owned by exact process generations and attempts. Scan rejects locally active starts and conditionally rolls back send/busy failures only while its generation and presentation revision still own the optimistic phase. Deep Analyze File/Folder/All share one observable owner reserved before send; cleanup terminates pre-start waiters, stale terminals cannot release a replacement, and busy or post-write-uncertain outcomes atomically fence the owner while a process restart resolves ambiguity. Successful terminal presentation is published before releasing that owner, under the same lock used by generation retirement. Deep Analyze, Library, and Restructure controls derive from ownership, and warmup warnings carry the exact attempt ID.

Windows/WSL policy suites pass, Release x64 builds with 0 warnings/errors, app tests pass 284/284, IPC tests 48/48, format and syntax gates pass, and repeated final reviewers report no blocker/high/medium in these scopes. Evidence is `.ralph/baseline/iteration29-*`. Strict egress still fails only on the six reviewed mirrors and widened host set. Next are Linux People lifecycle/edit ownership, raw VLM/codec child-process termination, macOS/docs/restructure truth, further whole-diff rounds, and the final repository-wide monitor gate.

## 2026-07-19 — Bootstrap and production network capability boundaries are source-closed

Every tracked or unignored shell/package-bootstrap source now requires an exact reviewed whole-file SHA-256; this includes shebangless `PKGBUILD`/`APKBUILD`, standard package script extensions, attached `env -S` forms, and extensionless shell shebangs. Git discovery is NUL-safe, tracked symlink mode is authoritative and forbidden, ignored build outputs stay excluded, and the policy workflow runs unconditionally. Download/source/eval/nested-shell, indirection, path execution, Unicode/newline-name, symlink, package-source, and digest-drift regressions fail closed.

The runtime release gate now inventories every current production raw-network, Foundation URL-loader, process-spawn, native FFI, and dynamic-loader module by normalized whole-file digest while scanning all tracked/unignored Rust, Swift, and C# production sources for new capability sites. Rust and Apple external transports reject disallowed initial URLs and redirects; Apple redirect text now matches the Hugging Face-only policy. The llama.cpp loopback client disables proxies and redirects, with real local proxy and redirect regressions. Policy and publication steps reject path/branch filtering, conditional/advisory execution, and `continue-on-error` masking.

Windows and WSL each pass 55 policy regressions and all enforcement/docs/compilation checks. Windows and WSL fmt/clippy plus both loopback transport tests pass; evidence is `.ralph/baseline/iteration28-{policy,vlm}-{windows,wsl}.log`. Strict egress still fails exactly on six reviewed GitHub/NVIDIA mirror URLs plus the widened host set, preserved at `.ralph/baseline/iteration28-strict-egress-blocker.log`; replacing those artifacts with byte-identical vetted Hugging Face mirrors remains an external release gate. The whole audit continues with UTF-16 binary-privacy enforcement, lifecycle/resource/UI/docs work, further whole-diff rounds, and the final repository-wide monitor gate.

## 2026-07-19 — Protected face identities survive clustering and persistence

Rust and macOS no longer let a raw geometric cluster erase user identity before later guards run. Fresh named owners and stable `same_person=0` anchors are resolved under the persistence writer snapshot, partitioned deterministically before suppression, and kept transitively separate through sparse forbidden-component union. Unknown identities and protected owners with any out-of-pool or zero-current-face membership remain in place. A prior identity can be inherited by only one rebuilt cluster, protected micro-clusters bypass junk suppression, verdict work fails closed above 100,000 rows, and the complete live face/anchor plan is validated before destructive SQL.

The Rust persistence transaction uses one temporary preserve set and rolls back atomically; pool-only result counts cannot be inflated by out-of-pool verdict endpoints. macOS applies preserved rows to the hard person cap, streams eligible faces before enforcing its 200,000-row window, and reports the unchanged current person count on no-work, cancellation, model/query, and persistence failures rather than a false zero. Numeric clustering thresholds were deliberately unchanged.

Windows full engine gates pass with 526 library/536 binary active tests; WSL passes 516/525. Evidence is `.ralph/baseline/iteration27-engine-{windows,wsl}.log`; final Rust and Swift source reviewers report no blocker/high/medium issue. Native `swift build && swift test` remains a macOS gate. The whole audit remains open for release-policy, lifecycle/resource/UI/docs/restructure findings, further whole-diff rounds, and the final repository-wide monitor gate.

## 2026-07-19 — Windows Exact Cleanup and immutable workflow enforcement are closed

Windows Exact Cleanup now constructs live destructive proof instead of sending IDs selected from persisted sampled-hash groups. A bounded, cancellable background pass full-SHA-verifies one unselected keeper plus every selected victim (5,000 victims/64 GiB), surfaces verification rejection, and sends complete keeper-bound `exactIdentities`; Similar mode retains its explicit perceptual bare-ID behavior. At execution, the keeper stays open without write/delete sharing and the atomically claimed victim stays open without write sharing until the Trash backend returns, blocking content changes while permitting the victim's Recycle Bin rename.

Bulk terminal ownership now rejects same-action overlap promptly. A confirmed-send timeout retains the waiter and its infinite Undo capture until the late terminal or an engine transition, while a timeout thrown by the send itself releases normally. The workflow action-pin checker is now a dependency-free fail-closed canonical YAML-subset validator: external actions/reusable workflows require lowercase 40-hex commits and Docker/job/service containers require SHA-256 digests; unsupported alternate YAML, traversal, dynamic, Unicode-separator, flow, property, and continuation forms are rejected by adversarial tests.

Windows full engine gates pass with 511 library/521 binary active tests; WSL passes 501/510. Release x64 builds with 0 warnings/errors, app tests pass 280/280, IPC tests pass 48/48, and format/policy/docs/schema/diff checks pass. Evidence is `.ralph/baseline/iteration26-*`; final focused reviews report no blocker/high/medium in either changed scope. The whole audit remains open for protected face clustering, remaining policy/lifecycle/resource/docs work, further whole-diff rounds, and the final repository-wide monitor gate.

## 2026-07-19 — Whole-diff reconciliation closed two destructive proof gaps

The first whole-branch adversarial pass reconciled the old audit queue into current source work, fixed/source-closed items, roadmap, and external-only gates. It found that canonical exact evidence authenticated victim and keeper independently without requiring them to be the same bytes, and that path-opened hashing could accept a rename/hash/rename-back ABA. IPC normalization and execution now independently require equal size and decoded SHA-256; SHA-256 returns the identity of the same open handle used for the read and must match the pathname before and after hashing.

Linux Exact Cleanup now generation-binds each destructive preflight. Engine exit, local failure, no-valid-files, and terminal completion invalidate the generation, so an old hashing callback cannot send during a replacement operation or overwrite its pending result state. Unequal-proof, ABA, and stale-generation regressions pass. Full engine gates pass on Windows (509 library/519 binary active tests) and WSL (500/509), and native WSL GTK passes 33/33. Evidence is `.ralph/baseline/iteration25-*`; final focused reviews report no blocker/high/medium in the changed scopes.

The audit is not complete. The reconciled ledger retains concrete Windows Exact/lifecycle, cross-platform face-identity, policy-enforcement, codec-lifecycle, docs, and UI work. Native Mac, graphical/package, real TDR/codec RSS, signing, mirrors, clean-VM, and model-export gates remain external.

## 2026-07-19 — Zero-byte rescans are dormant, truthful, and recoverable

A previously indexed file truncated to zero no longer remains active with stale searchable content. Shared Rust discovery routes empty-file observations through a bounded non-model path; exact-path existing rows are revalidated, retain their ID and user tags, then become inactive with all auto/VLM tags, faces, OCR/document FTS, embeddings, hashes, EXIF, captions, and crops cleared transactionally. New empty files still create no row. Files that grow or are replaced before mutation are preserved and reported as a nonterminal partial scan instead of being falsely soft-hidden.

The model-free CLI uses the same transition inside its existing batch transaction. Same-path restoration always reprocesses the dormant row even when the original mtime is restored, and successful engine restoration now repopulates the aesthetic score through both conflict-upsert variants. Face-crop deletion occurs only after SQLite commit and after releasing the writer guard.

Windows full engine gates pass with 507 library/517 binary active tests; native WSL passes 498/507. Windows CLI passes 47 active unit plus 9 smoke tests, and WSL passes 48 plus 9. Final adversarial closure found no blocker/high/medium issue. Evidence is `.ralph/baseline/iteration24-zero-*` and `.pi-subagents/artifacts/outputs/a5c2cd34/.ralph/subagents/iteration24-zero-final-clean.md`. The narrow final path-revalidation-to-SQL interval remains an explicit handle-authority residual.

## 2026-07-18 — Command terminal parity is closed for current Windows/Linux clients

The full command inventory now terminates every current awaited or optimistic client state truthfully. Scan and Deep Analyze lifecycle rejection is idle-gated so an invalid duplicate cannot terminate healthy existing work; Windows serializes File/Folder/All Deep Analyze behind one process-wide reservation. Bulk semantic/database/paused/worker failures emit failed action terminals, and Windows awaits Apply Tags, Mark Different, and merge suggestions before claiming success.

Linux forwards terminal scan and face-clustering events, keeps partial/empty/no-change scan notices nonterminal, awaits and serializes person rename plus detail/bulk Mark Unknown, and removes closed one-shot subscriber channels after fanout. The canonical bulk-action discriminator now includes `markPersonsDifferent`, with Rust/C# exemplars.

Windows Rust fmt/clippy/focused regressions pass; Release x64 builds with 0 warnings/errors; app tests pass 272/272 and IPC tests 48/48. Native WSL GTK fmt/clippy and 32/32 tests pass. Final independent closure found no blocker/high/medium issue. Evidence is `.ralph/baseline/iteration23-terminal-*.log` and `.pi-subagents/artifacts/outputs/46ab5aaf/.ralph/subagents/iteration23-terminal-final-closure.md`.

Next source priority is deliberate engine/CLI zero-byte parity: a previously indexed file truncated to empty currently retains stale active metadata. The reviewed dormant-row design is preserved in the iteration-23 zero-byte artifact; it is not falsely recorded as fixed.

## 2026-07-18 — Awaited command failures terminate; Linux Cleanup preserves last-good data

`mergeClusters` now returns the awaited failed bulk result on semantic, database, and paused-scan rejection. Restructure plan/apply/undo emit command-specific terminal errors across those admission paths, and rejected queued apply/undo operations release their single-flight reservations. WinUI clears prior restructure errors before retry so value-identical failures still notify; an undo error clears process-global in-flight state even while the view is unloaded.

Linux Cleanup no longer maps DB path/open/count/prepare/query/row or worker-channel failures to a successful empty result. Failed refreshes retain the last verified groups, selection, candidate count, and partial warning while displaying an explicit error; only a genuinely absent first-run DB is empty. Row decoding is fail-visible and database metadata access uses `try_exists`.

Fresh post-Trash full engine gates pass on Windows (501 library/510 binary) and WSL (492 library/500 binary), Release x64 builds with 0 warnings/errors, app tests pass 270/270, and native WSL Cleanup tests pass 10/10. Final focused reviewers found no blocker/high/medium in either changed scope. Evidence is `.ralph/baseline/iteration22-*` and `.pi-subagents/artifacts/outputs/19e772bb/.ralph/subagents/iteration22-*-followup.md`.

## 2026-07-18 — Windows Trash and retry cleanup require a durable physical receipt

IFileOperation success and path absence no longer authorize catalog deletion. After Shell deletion, FileID boundedly resolves each claimed path to the physical `$R` object, verifies the exact volume/file identity, and appends a signed full-batch receipt. Missing/mismatched lookups or receipt persistence failures remain per-item failures and retain the catalog row. Recycle lookup is split below a conservative Windows environment-block bound.

The newest valid signed receipt now wins. Restore persists receipts for older identity-bound journals before moving `$R`, revalidates and refreshes stale locators, and carries the physical path through recovered-identity reconciliation. A filesystem-success/SQLite-commit-failure retry therefore restores the exact ID/size/file reference, commits, and only then removes `$I`; failed commits never clean metadata. Windows fmt/clippy, focused bulk/restore/journal tests, and the real Recycle Bin round trip pass; WSL mirrors compile and pass. Evidence is `.ralph/baseline/iteration21-windows-trash-receipt*.log`.

## 2026-07-18 — Linux Trash preserves durable identity or fails before mutation

Current Trash requests now require a captured volume-qualified identity before journaling, claiming, or dispatch. File IDs are stable-deduplicated at both IPC normalization and the handler boundary; duplicate exact evidence fails closed. Every current journal record therefore carries restore authority rather than creating a new identity-less manual-recovery case.

Linux no longer performs an identity-changing home-Trash copy on `EXDEV`: it leaves both filesystems unchanged, removes the uncommitted `.trashinfo`, and lets the outer no-replace claim restore the original path. Same-filesystem mutation rechecks identity immediately before and after rename. Mismatches are never propagated back through staging into the user path, and a cleanup guard retains `.trashinfo` only when the Trash object is proven to be the expected inode. Native WSL fmt/clippy and 22 focused Trash tests per target pass, including real `/dev/shm`↔`/tmp` EXDEV and dangling-target cases; focused Windows fmt/clippy/bulk tests pass. Independent follow-up found no blocker/high/medium issue. Evidence is `.ralph/baseline/iteration20-trash-input-{windows,wsl}.log` and `.pi-subagents/artifacts/outputs/bbd13397/.ralph/subagents/iteration20-linux-trash-review.md`.

The remaining destructive boundary is Windows-specific: bind IFileOperation completion to the expected claimed object and preserve or rediscover `$I` metadata across catalog-commit failure/retry.

## 2026-07-18 — Restore is authorized, identity-proven, and retryable; Trash submission closure remains

Windows Undo no longer delegates authority to Shell Undelete: PowerShell only locates the physical Recycle Bin object, Rust verifies its volume-qualified identity, and a native no-replace rename targets a sentinel-pinned authorized parent handle. Linux resolves authorized destinations through no-follow directory handles, claims and verifies Trash sources, selects duplicate same-path generations by identity, and uses deterministic quarantine names that survive interruption. Late destination occupants are conflicts.

Recovered identity now gates exact catalog reconciliation. Undo restores the authenticated file ID, actual size, and file reference; uniqueness conflicts fail visibly; matching already-restored objects are idempotent after process/SQLite failure; and `$I`/`.trashinfo` cleanup occurs only after the transaction commits. Windows IFileOperation rejects aborted or no-op deletes. Windows full fmt/clippy/tests pass (501 library, 509 binary), the real Recycle Bin round trip passes explicitly twice, and WSL full gates pass (489 library, 496 binary). Logs are `.ralph/baseline/iteration19-engine-{windows,wsl}.log`.

Final adversarial review keeps the group open. Linux's home-Trash EXDEV copy creates a new `(dev,inode)` while the journal authorizes the original identity; fail closed until a same-mount Trash or durable receipt is designed. The last revalidation-to-path-backend Trash race also remains, along with duplicate-ID exact-proof consumption, identity-less current journals, and stale Windows `$I` metadata after commit-failure retry. Reports are under `.pi-subagents/artifacts/outputs/ff733a57/.ralph/subagents/`.

## 2026-07-18 — Identity-bound Trash and byte-exact Linux Cleanup implemented; restore closure remains

Shared Rust `trashFiles` now durably journals before mutation, atomically claims each indexed object to a unique sibling, revalidates the claimed identity, and conditionally removes only the unchanged database row. Linux freedesktop Trash preserves the original location, uses identity-bound cross-filesystem finalization and ordered durability, while Windows Undo maps its claimed Recycle-Bin source back to the user-facing path. Admission failures and bulk-worker failures now receive terminal results.

Linux Exact Cleanup no longer trusts persisted or sampled hashes as byte-equality proof. It boundedly full-SHA-groups every active same-size candidate off the GTK thread, coalesces stale reloads, discloses partial limits, and requires an unselected keeper. The approved optional `exactIdentities` IPC evidence completely binds each victim and keeper path/size/SHA-256; the engine caps aggregate reads and rehashes both after atomically claiming the victim. Canonical Rust/C#/Swift mirrors and parity tests were updated.

Windows engine fmt/clippy/full tests pass (496 library, 504 binary); WSL engine passes (481 library, 488 binary); native WSL GTK passes 26 tests; Windows Release x64 builds with 0 warnings/errors, app tests pass 269/269, IPC 48/48, and format/policy/schema gates pass. Logs are under `.ralph/baseline/iteration18-*`.

Final authorized-root review found two actionable restore defects, so this change group is not closed: a parent symlink/junction replacement can redirect a later path-based recovery beyond the root authorized earlier, and a destination appearing after the initial occupancy snapshot can be mistaken for successful recovery while the real item remains stranded. The review is preserved at `.pi-subagents/artifacts/outputs/f6bfe5dc-c042-487e-83bc-4276f73801f5/.ralph/subagents/iteration18-final-authorized-root-review.md`; both fixes lead the next pass.

## 2026-07-18 — Successful wipe is an ordered barrier; queued scans cannot resurrect the library

Closed a destructive command-order race in the Rust engine. Mutation tasks previously discarded and recreated their Tokio mutex waiter every two seconds while checking for a paused scan. That lost FIFO position, allowing a later uninterrupted `wipeLibrary` waiter to overtake an earlier accepted command, truncate successfully, and then let a queued scan repopulate the empty database. Mutation admission now first-polls one pinned lock future in its final child task and completes an explicit registration handshake before IPC dispatch continues. Periodic status checks borrow that same future, so a later wipe cannot overtake accepted work.

Scan admission is now reserved before gate waiting. A duplicate queued/running StartScan rejects without resetting the first scan's cancellation; CancelScan and wipe publish a pending cancel even when the coordinator has not been installed yet. Once an earlier queued scan reaches the gate, it publishes the real coordinator and observes that cancel before model loading, then releases before wipe runs. Pre-cancelled Deep Analyze file/folder/library commands likewise emit only terminal cancelled completion before any query, runner setup, or starting/progress event.

Independent review also found the scan coordinator cleanup guard was installed only near `ScanSession::run`, leaving preflight/model-load panic paths able to wedge the slot. The guard now begins immediately after successful coordinator publication, while the outer scan reservation prevents replacement admission until cleanup finishes. Deterministic regressions cover FIFO retention across repeated status timeouts, duplicate reservation/cancel preservation, pre-model cancellation, all Deep Analyze variants, and injected preflight panic cleanup. Final reviews found no blocker/high/medium issue in this change group. Windows fmt/clippy plus 489 library/496 binary tests pass; WSL fmt/clippy plus 473 library/479 binary tests pass. Evidence is under `.ralph/baseline/iteration17-*`.

## 2026-07-18 — Decoder admission and face ownership are bounded before expensive work

Closed the resource defects identified after the GPU-lifecycle pass. Every image, video, HEIC, and OBJ full-frame producer now acquires a process-wide predecode reservation before allocation. Immutable byte-backed images reserve a conservative decoder-output-plus-RGB allowance; path-backed, unprobeable, HEIC, and video decoders take the full 256 MiB capacity exclusively so worker count cannot multiply an uncertain native-codec peak. Successful reservations only shrink to retained RGB bytes, cancellation is checked before admission even when capacity is free, and mismatches fail rather than growing a live guard. Linux ffmpeg PPM is consumed through a bounded pipe while the child runs and overflow kills/reaps it; libheif writes into a private RAII directory whose aggregate bytes/files are monitored and every multi-image member is removed. Immutable pixel/raster validation closes the remaining decode boundary.

Crowd images now validate and deterministically rank lightweight face detections before crop alignment or SFace inference. Ordinary images preserve detector order; over-cap images process quality-descending candidates with stable earlier ties until 32 embeddings succeed, continuing past ordinary failures. This bounds expensive crowd work as well as retained crop memory. DBWriter records stable crop positions in the transaction, commits first, then moves the original `Vec<u8>` allocations into JPEG persistence without cloning; rollback leaves both allocations and prior JPEGs untouched.

Independent review found a Linux metadata-then-read race and incomplete post-embedding crowd pruning; both were fixed, along with strict PPM delimiter validation and conservative reservation wording. Final focused follow-ups found no remaining blocker/high/medium issue. Windows engine fmt/clippy plus 486 library/491 binary tests pass; native WSL fmt/clippy plus 470 library/474 binary tests and manifest tests pass. Evidence is in `.ralph/baseline/iteration16-windows-engine-full.log`, `.ralph/baseline/iteration16-engine-wsl.log`, and the iteration-16 subagent artifacts. Real Windows codec RSS/cancellation and video/large-photo throughput remain hardware gates.

## 2026-07-18 — Process-lifetime GPU-reset safety and generation-safe Windows recovery

Hardened the Rust engine and WinUI client against DirectML/CUDA device removal. One irreversible process-wide latch now wakes paused scans, rejects later scans and AI queries, classifies failures during both ORT session creation and inference, and guards queued/batched work immediately before submission. Deep Analyze requests accepted before the failure are rechecked after mutation-gate waits; active llama.cpp CLI/server work is cooperatively terminated and receives a canonical `gpu_device_removed` error plus terminal completion.

Windows engine supervision now carries the spawning generation through stdout dispatch and rejects stale events before publication. Expected exits are bound to the exact `Process`, an exited predecessor is cleaned before replacement spawn, and dead-process scan state is reset without clearing the sticky GPU flag until the replacement reaches `Ready`. The sidebar exposes an accessible **Restart Engine** action; scan/Deep Analyze/text search remain blocked until recovery, and per-file Deep Analyze rejection restores optimistic controls instead of wedging the UI.

Independent final review found and drove fixes for model-load classification, queued/in-flight Deep Analyze cancellation, ordinary-restart scan-state leakage, and per-file UI rejection; focused Rust and app follow-up reviews then found no remaining blocker/high/medium issue. Native Windows and WSL engine fmt/clippy/full tests pass; Windows Release x64 builds with 0 warnings/errors; app tests pass 269/269, IPC 48/48, and dotnet format is clean. Logs are under `.ralph/baseline/iteration15-*`. A real TDR is still an on-hardware gate. Next resource fixes are full-frame HEIC/video allocation before decoded-byte admission and face top-K truncation after crop allocation.

## 2026-07-17 (night) — Linux GUI production pass: People-tab data loss fixed (invalid SQL shipped in #106), first-run Welcome sheet, video thumbnails, persistence, scan cancel — all verified live under WSLg on Adlon data

Drove the GTK app end-to-end under WSLg against a 958-file / 3.5 GB Adlon subset (fresh full-ML CPU scan: 945 processed, 0 failed, 1041 faces, 8032 tags) and fixed everything found, branch `linux-gui-polish`.

**CRITICAL fix — People tab was completely blank since #106.** The rewritten person-snapshot SQL put a correlated outer reference (`p.representative_face_id`) inside a scalar subquery's ORDER BY — SQLite rejects that with "no such column", the swallowed `anyhow` error defaulted the whole snapshot, and the tab permanently showed "No people yet" even with 1041 faces in the DB. Rewrote as two legal WHERE-correlated lookups under COALESCE (representative-if-active, else lowest-id active face), made the read failure log loudly, and added the regression test that was missing: prepare the exact SQL against a real migrated schema (`person_snapshot_sql_prepares_against_current_schema`). Verified live: 1041 faces → "ready to group" → GUI clustering → 17 person cards with face crops, merge/unknown flows armed.

**First-run + persistence parity (new `app_settings.rs` + `welcome.rs`):** the app now reads/writes the same `app-settings.json` the Windows app uses (camelCase keys, unknown keys preserved, atomic temp+rename) — restores lastFolderPath (only if it still exists — Windows root-recovery semantics), activeTab, sidebarVisible, welcomeSheetSeen; writes them on pick/nav/toggle. New Welcome sheet mirrors Windows: five core models with live install state off the engine registry, one-click "Install everything" driving `prewarmModel` with per-row progress from `modelDownloadProgress` + a bounded disk poll, the machine-sized VLM recommendation line, re-shows while any core model is missing. Verified live on both the all-installed and fresh-launch paths.

**Library/Cleanup/preview upgrades:** video tiles now render real ffmpeg keyframes (engine `shell::video::keyframe_25pct` on the thumbnail worker pool — no new deps) with a centered ▶ badge, in Library, Cleanup groups, and the preview dialog (all 14 corpus videos verified); Library gained a real empty state (no-scan vs no-matches variants) instead of a black void, and an honest count ("showing 1,000 of N" via a LIMIT-gated COUNT that the common small-library path skips); `format_bytes` gained GB.

**Scan lifecycle in the sidebar:** Start scan ↔ Stop scan toggle (sends `cancelScan` — was previously impossible to stop a scan without quitting), slim gold progress bar under the status line, verified through a live start→complete cycle including the models-busy Remove-button interlock.

**Robustness/perf:** every data tab reloads on `connect_map` (tab switch) so a startup read that races the engine's DB open — or work finished while on another tab — can never leave a stale view; LavaLamp capped at ~30 fps (frame-clock throttle; full-window Cairo at display rate measured >1.5 cores on llvmpipe/WSLg, and the drift is imperceptible at 30); `emblem-ok-symbolic` (pruned from modern adwaita-icon-theme, rendered as missing-image) replaced with `object-select-symbolic` at all 9 sites — a live icon-theme audit of all 31 icon names found no other gaps; sidebar nav heading LIBRARY→NAVIGATE; welcome/people status-line truthfulness fixes ("clustering…" only while clustering; "Grouped into N people." lands with results).

**Verification:** WSL Linux 1.90 `cargo clippy --all-targets -D warnings` + `cargo fmt --check` + 20 app tests green; six-tab live walk on the scanned corpus (screenshots in session scratchpad): Library grid/search-pills/preview-nav, People cluster→cards, Cleanup Exact+Similar (14 perceptual groups with KEEPER ranking), Deep Analyze cards + VLM picker, Restructure semantic plan (441 moves, Sankey, apply bar), Settings all cards. Remaining Linux polish (NEXT): in-preview video playback needs a GStreamer decision; WSLg synthetic-keyboard limits kept search-typing untested (code path unchanged from prior on-hardware passes).

## 2026-07-17 (late) — Audit fixes landed (#130+#131), v0.1.1 refreshed + verified; full WinUI app audit: zero defects

Merged #130 (audit hardening) CI-green; first tag run then FAILED on ICE38/43/57 — the HKLM shortcut-keypath "fix" was a false positive (WiX requires HKCU for ProgramMenuFolder shortcut components); reverted via #131 with the constraint documented in Product.wxs. Re-moved v0.1.1 to `f491271`, release run 29617526554 green (first ICE-clean pass over the new Bundle.wxs ARM64 guard), replaced all 10 release assets via API and verified each: server digest == local SHA-256, fresh timestamps, SHA256SUMS.txt consistent, both MSIs embedded in the 342 MB bundle (size arithmetic). Also completed a top-to-bottom audit of the entire WinUI app (~29k lines, every service/VM/view + structural sweeps): zero new defects; six candidates dismissed on context; tracked "CUDA/cuDNN buttons flip early" item appears already fixed (WaitForModelSentinelsAsync) — retire after on-hardware confirm. Lesson recorded: installer ICE validation runs ONLY on tag pipelines, not PR CI.

## 2026-07-17 — Windows prod-readiness audit: clean bill on code; 5 hardening fixes (installer ARM64 guard, MSI keypath, CI smoke gate, stray file, test-matrix case)

Deep solo audit of `platforms/windows` (two 14-agent fleet attempts died on session usage limits first). Verified green: `cargo clippy -D warnings`, ~450 engine tests, downloader SHA-256-before-rename, trash/restructure kill-tolerant journals + reparse TOCTOU re-checks, OneDrive placeholders never hydrated, EngineClient bounded backoff + 3-strike. No new blockers — the six tracked release gates remain the path to prod. Fixed (working tree, needs branch/PR/CI): stray `main 2.rs` deleted; `Bundle.wxs` x64-only bundles now refuse ARM64 hosts (was: silent empty "successful" install); `Product.wxs` shortcut keypath HKCU→HKLM (perMachine ICE38/57) + upgrade-while-running documented; `windows-app.yml` smoke fails on ANY <5 s exit (crash-with-exit-0 previously passed); clean-VM matrix gains the mid-scan-upgrade case. Details: NEXT.md top entry.

## 2026-07-16 — Landed the mid-flight cross-platform safety + macOS Deep Analyze tree: 3-round adversarial audit, 11 defects fixed, Adlon-validated, quinn-proto CVE patched

Picked up a large uncommitted working tree (103 modified + 16 new files) — the macOS Deep Analyze / VLM / CLIP port, cross-platform model-license acceptance gating, archive (zip-slip) path-safety, engine-lifecycle safety, and read-only corpus fingerprinting — and drove it to landable.

**Baseline (ground truth, not assumption):** the full local audit gate (`shared/scripts/run_local_audit_gate.sh`) was already green first-pass — Swift 6 strict-concurrency `swift test` (311 tests/66 suites, `-warnings-as-errors`), engine 447 lib + 450 bin + CLI 51+11 + TUI 100 cargo test, clippy `-D warnings` on all four crates, fmt, and all 11 Python policy/egress/TLS suites. The "never compiled on a Mac" Swift work compiles + passes under the strictest settings.

**Three-round multi-agent adversarial audit** (subsystem finders → default-reject verification → fix-review of every applied fix; ~50 agents, converged with a clean final round). 11 confirmed defects fixed:

- **[C# app, BLOCKER — every gated download broken] `ModelLicenseGate`** persisted acceptance via `Windows.Storage.ApplicationData.Current`, which throws in the UNPACKAGED app (WiX MSI, no package identity) — so on every shipped build the NVIDIA CUDA/cuDNN accelerator pack and the Gemma VLM could never be installed. Rewrote to file-based persistence (`%LOCALAPPDATA%\FileID\model-licenses.json`, atomic tmp+move), and it now honors the user's explicit acceptance for the session even if the durable write fails. (Found only by the C# read — CI compiles but can't exercise the unpackaged runtime.)
- **[macOS engine, HIGH — runtime starvation] `Tagging.boundedVideoKeyframe`** ran `extractVideoKeyframe`'s blocking `DispatchSemaphore.wait` on the fixed-width Swift cooperative pool (via `Task.detached`), so a wave of hanging NAS video extracts could pin every cooperative thread and stall the DB writer / IPC drain / command loop. Rewrote `generate()` as async `withTaskCancellationHandler` + `withCheckedContinuation` (+ a `SendableGeneratorRef` box for the `@Sendable` cancel path); no cooperative thread is parked.
- **[CLI, HIGH — user-data loss] scan re-index** deleted ALL tags (incl. user-authored) for a file whose inode changed at the same path (trivially triggered by atomic-save editors), vs the engine reference (`dbwriter.rs:298` / `DBWriter.swift:680`) which deletes only `source='auto'`. Removed the `replacement`/`delete_all_tags` path; re-index always clears only auto tags now. Test that enshrined the buggy behavior updated to assert the user tag survives.
- **[Linux app, HIGH — license bypass] Deep Analyze** triggered a restricted-model (Gemma) download from all three entry points (Analyze-All / -Folder / re-analyze) without the license gate, unlike macOS which gates every DA path. All three now route through `model_license::ensure_or_prompt`.
- **[installer, HIGH — release build break] WiX** `AllowSameVersionUpgrades="yes"` trips ICE61, absent from the wixproj suppress list → with `TreatWarningsAsErrors` the per-arch MSI build fails at release time. Added `ICE61` to `SuppressIces` (the documented WiX remedy).
- **[engine, HIGH — indefinite hang] `spawn_mutation`** acquired the shared mutation gate with no timeout; because `StartScan` holds that gate for its whole life (by design, so `WipeLibrary` can wait a scan out before truncating), a *paused* scan hung every other mutation forever. Now polls the gate and emits a retriable `library_busy` error ONLY when the holder is a paused scan — a running scan / long mutation is still waited out (no spurious failure). The `exclusive_gate_times_out_while_a_mutation_is_active` test confirms the wipe interlock is intact.
- **[engine, MEDIUM] CLI `install_model_blocking`** wrote only `model.id` into the install sentinel, but `installation_complete()` now requires the full v2 attestation for zip runtime packs → every CLI-installed pack read as never-installed and re-downloaded. Now writes `registry::installation_attestation` like prewarm.
- **[macOS engine, MEDIUM] `VLMDownloader`** left `sentinelValid` true after deleting an invalid `.fileid-verified` manifest, so the size-only skip kept trusting files revalidation just proved corrupt. `let`→`var` + reset on deletion.
- **[CLI, MEDIUM/LOW] 0-byte files** — a present empty file was soft-hidden as "no longer present" (fixed: record it "seen", matching the engine's seen-but-not-catalogued behavior), and the seen-push now respects the batch-flush bound so a tree of empty files can't grow the batch unboundedly.

**Security (self-inflicted by the diff's own new gate):** the diff adds a `cargo audit` release gate, but the lockfiles pinned `quinn-proto 0.11.14` (RUSTSEC-2026-0185, high/7.5 remote DoS). Patch-bumped to 0.11.15 in all four lockfiles; all now audit-clean, no cascade.

**Read-only Adlon validation** (real corpus, isolated HOME, user DB untouched): scanned 1,331 real files (`Family Shared/Photos/Adam's Stuff`, 2.99 GB) headless — 0 failures, ~42 f/s, clean exit; the new per-file content-hash continuation produced a valid 32-byte SHA-256 for all 1,331; 2,643 faces detected via Vision. **Corpus fingerprint (path/size/mtime_ns → sha256) byte-identical before/after** — proven read-only. (Full ML — CLIP tags / face embedding / Deep Analyze VLM — not exercised: models aren't installed on this Mac and downloads route through app Settings; the shared engine already had the full 71k Adlon ML soak.)

**Gates after all fixes:** Swift 311, engine 447+450 + clippy, CLI 51+11 + clippy, TUI 100 + clippy, fmt/whitespace clean, cargo-audit clean on all four lockfiles, all Python policy suites green. C# (WinUI) and GTK/Linux fixes are compile-validated by CI only (no local runtime) — they were adversarially read-reviewed instead. Owner runtime follow-ups: the Windows mutation-gate concurrency change (paused-scan `library_busy`) and the wipe-vs-queued-scan edge (fail-safe: wipe times out "nothing wiped" + succeeds on retry; NEXT.md).

## 2026-07-15 — Fresh-install feedback round: Deep Analyze was 100% broken on the new runtime; preview keys, install UI, cancel affordance

The owner's true first-run (post-wipe install of the refreshed v0.1.1) surfaced four defects, all root-caused on their real log/DB and fixed + live-verified via UIA on the dev box:

- **Deep Analyze broken outright (showstopper):** the updated llama.cpp runtime (b9254) removed `--no-display-prompt` from `llama-mtmd-cli`; the engine passed it unconditionally → instant exit(1) on EVERY single-file analyze, surfaced as a misleading "model isn't installed". Flag removed (mtmd-cli never echoes the prompt on any build). A redacted 8-line stderr tail is now attached to nonzero-exit errors so the next runtime break is diagnosable from app.log. Verified: Mistral-24B captions a real Adlon photo end-to-end ("MADISON SQUARE GARDEN" read off the scoreboard, proposed name `madison-square-garden-empty`) with llama.cpp's CPU spill handling the 13.6 GB model on the 16 GB 5080.
- **VLM selection drift:** onboarding installed Mistral but `SelectedVlmModelKind` stayed at the qwen default (`WasUserChosen=false`) — the Welcome combo + Install-all path never persisted the pick, so every preview Analyze targeted uninstalled weights. `SeedDeepVlmFromSentinels` now re-points Settings at the installed VLM when the persisted pick has no weights on disk (an explicit user choice still wins). Verified self-heal on launch.
- **Accidental-Analyze guard:** the preview sheet's Analyze button is now a two-state control — while a run this sheet started is in flight it reads "Cancel analysis" and sends `DeepAnalyzeCancel`; auto-resets on `DeepAnalyzeComplete` (new `[ENGINE-SUB:FilePreviewSheet]` subscription, SafeRun + dispatcher-marshaled) and on navigation. All transitions live-verified, including the cancel IPC.
- **Preview arrow keys dead / video work:** added Left/Right `KeyboardAccelerator`s on the sheet (input-preprocessing path — fires even when the ContentDialog's key handling starves the routed tunnel) with a same-keystroke tick dedupe against the routed handler, and eager `MediaPlayer` creation so `MediaFailed` always attaches (codec failures were a silent black surface). Live-verified: Right 3→4 exactly one step, Left×2 4→2. Video playback path compile-verified only (owner's fresh scan has no video rows yet).
- **Model-install UI glitch/flash + fit:** owner's log showed ~20 progress events/s (19k+ for the 15 GB Mistral) — engine `PROGRESS_THROTTLE_MS` 50→250 plus a time floor on the simple-download path; label churn killed (Consolas progress/rate lines single-line + rate row reserved for the whole download); `BytesDone` can no longer exceed the displayed total ("15.21 GB of 15.18 GB"); Welcome sheet Width=600 → adaptive Min/MaxWidth and the overlay Border fits small windows (verified at 1000×640: footer visible, internal scroll). 

Gates: engine 449/451 + clippy 0 (Windows + WSL 1.90), app 244/244 + IpcSchema 48/48, format clean.

## 2026-07-14 — Audit iteration 14: Adlon soak GREEN, measured face/restructure recalibration, undo-journal crash-safety port

Independent re-review of the whole uncommitted audit tree (multi-agent adversarial pass + three subsystem deep audits), then measured fixes against the real corpus. The pending uncapped Adlon soak (NEXT item 4) is GREEN: 71,333 files at 43 f/s, peak 5,441 MB, all 12 assertions, WAL checkpointed, privacy gate clean, corpus fingerprint byte-identical before/after.

**Restructure (the owner's top pain) root-caused and fixed with measurements.** Libraries over 50k files bypassed the entire semantic butler — the real corpus always got the legacy date cascade (coordinate `Places/` folders, albums shredded into `Photos/<Year>/<Month>`). The semantic planner measures 16 s / 668 MB at 71k, so the threshold moved to 150k (env-overridable). Unlocking it exposed over-eagerness, fixed with three guards: junk/numeric-prototype filtering in the image pass, a per-file stay-put rule (well-placed files are recorded as `settled` and claimed away from the cascade instead of reshuffled), and month-year naming for tagless dated clusters. Real-corpus plan: 56,110 → 38,345 moves, 144 "Unsorted N" folders → 0, junk-numeric targets gone. Paged (stored-plan) bulk apply now excludes `ask`-tier moves the truncated UI never showed (design §6).

**Undo journal ported to macOS crash-safety semantics** (it had silently diverged): write-ahead per-entry fsync before each move with phantom-entry rollback, fail-closed lazy open (nothing moves without undo protection; a non-journaling apply preserves the prior journal), NEWEST-FIRST replay (forward order corrupted dependent moves A→X/B→A into "A (2)"), and torn-trailing-line tolerance. Five regressions cover all of it.

**Faces recalibrated from full-corpus evidence, not the 185-face subset.** The suspected SFace BGR/RGB channel-order bug (deep-audit rank #1) was REFUTED by experiment — engine embeddings match cv2.FaceRecognizerSF on identical crops at cosine 0.969 vs 0.846 swapped, so no re-embed migration. The real levers: the 0.35 quality gate (top of the geometry-capped 0.23–0.42 scale, discarding 67% of detected faces) and hardcoded k_nn=10. A six-config re-cluster sweep on the 84,582-face DB moved defaults to quality 0.25 + `FILEID_FACE_KNN`=32: assigned faces 27,921 → 53,955, clusters 2,272 → 1,189, with the top clusters getting TIGHTER (mean cos-to-centroid 0.606 → 0.642) — healed fragmentation, not contamination.

**Also fixed:** reconciliation seen-set keeps present-but-skipped (zero-byte/unsupported-kind) files so a clean walk can't soft-hide them as "no longer present" (the companion "directory errors uncounted" BLOCKER claim was refuted — jwalk Err entries do disable reconciliation); CLI partial-scan exit-code inversion (dedicated rsync-style exit 3, committed results, honest header, cross-platform smoke); the one genuinely failing app test (InstallerContractTests asserting pre-pinning `@v4`); Linux sidebar lost model-download failures (ModelDownloadFailed arm); stray `nul` file; platform guide now documents that `dotnet test FileID.sln` runs zero tests (Tests projects are deliberately outside the solution).

**Second/third wave (same session).** Stability: per-image face cap (top-K by quality, env-tunable), DBWriter crop double-copy released post-commit, predecode budget reserved BEFORE decode via header probe (validated with a live 300-photo fresh scan: 15 s / 3.7 GB, all files landed), wipe now interlocks Deep Analyze, and restructure apply/undo gained an engine-side single-flight gate. After the full 40-agent adversarial verification finished, every confirmed finding was fixed: macOS Cleanup candidate starvation (recipe-agnostic group-ranked selection — the SQL was executed against the real 71k DB) + the per-batch exact-rehash storm + App-Nap tokens (all Mac-compile-gated, statically reviewed); TUI first paint decoupled from the 64-GiB duplicate verify pass (`LoadMsg::Dupes`) and the browser early-break restored; Linux model-removal re-checks authoritative `models_busy()` at click and confirm time; Windows thumbnail channel keeps its 256 bound but drops OLDEST with placeholder completion so visible tiles can't hang; CLI `dedupe --exact` listing verifies twin-groups-first and reports an explicit partial instead of hard-bailing (apply stays fail-closed).

**Hands-on UI validation (2026-07-15, dev build on the real Adlon library, read-only).** Drove the WinUI app via UI Automation + screenshots across all six tabs. Library CLIP search returns real family photos with real thumbnails (thumbnail-channel fix confirmed) and the preview close→reopen ContentDialog regression is fixed. People shows the 1,189 clusters. Cleanup Exact finds 200 real duplicate groups / 1.65 GB reclaimable; Similar shows the correct conservative safety banner. Restructure generated the semantic plan on the live 71k library — **38,352 moves across 2260 categories plus a first-class "Staying put · 240 folders kept intact" card** — the stay-put + semantic-planner fixes visible in the product, matching the headless measurement. Settings truthfully reports all five core models Installed and RTX 5080 + CUDA active. **Found one new bug by looking** — face-alignment edge-clamp *smear* crops (an off-frame landmark fit replicates the border into the 112×112 grid, corrupting ~1.1% of embeddings and visibly smearing the People card) — and fixed it: `align_112` now rejects >20%-out-of-bounds warps and falls back to the bounds-clamped bbox crop (2 regressions). **Correction to an earlier finding:** this machine has the CUDA pack installed (engine log confirms the `cuda,tensorrt,directml,cpu` EP chain), so the 43 f/s soak is USB-SSD I/O-bound (~440 MB/s), not GPU-bound — CUDA is already active.

**Verification:** engine 449 lib + 451 bin tests, clippy `-D warnings`, fmt clean, release build clean; CLI 32 unit + 9 smoke; TUI 96; WSL 1.90 full pass (engine/CLI/TUI/GTK app); app 244/244 + IPC 48/48 + format clean; all 11 Python policy suites + egress known-blockers; **uncapped 71k Adlon soak against the final engine green** (12 assertions, 5.4 GB peak, corpus fingerprint identical; the A3 harness false-fail on incremental rescans was fixed to gate on walk rate). Evidence: `.ralph/baseline/iteration14-adlon-experiments.log`. Remaining (ledger + NEXT): the Mac pass (compile the reviewed Swift work, face threshold ordering, 250k auto-merge cap), gpu_dead process latch, dynamic-batch RAM++ export (compute lever for smaller/faster-disk libraries), restructure post-P1 design items, and the faces name-guard/stable-identity design work.

## 2026-07-12 — Whole-codebase audit iterations 1–13: resource bounds, supply-chain policy, offline Flatpak, exact dedupe, and macOS hardening

The active adversarial audit has fixed and regression-covered the accepted Rust/CLI/TUI destructive and partial-scan defects, Windows thumbnail and exact similar-grouping scale bounds, immutable GitHub Action inputs, exact staged-payload privacy scanning, and Linux thumbnail/cache/Settings lifecycle defects. Native Windows, isolated Rust 1.90, and WSL GTK/libadwaita gates remain green; `.ralph/whole-codebase-findings.md` is the parent-triaged source of finding status and evidence.

Flatpak no longer downloads or executes rustup or gives the build sandbox network access. It targets GNOME 49/freedesktop 25.08, uses generated SHA-pinned Cargo sources plus pinned x64/aarch64 ONNX Runtime 1.22 archives, and forces Cargo/ort offline. A native WSL build and a second forced `--disable-download --disable-cache` rebuild both passed, AppStream composed, the 12 MB bundle checksum verified, both binaries passed privacy scanning, and ORT has no dynamic dependency. Packaging CI is required. The remaining Flathub release-cut step is replacing local source directories with the immutable audited-release archive; graphical launch, HEIC-in-sandbox, aarch64, and AppImage runtime remain external gates.

Canonical root/architecture/packaging/contributing/ship docs now match the hybrid CLI/TUI execution path, schema v19, semantic IPC ordering, Linux HEIC backend, current release status, and required GNOME 49 packaging gate. `shared/scripts/check_current_docs.py` binds those high-drift claims to source in policy CI.

CLI/TUI exact dedupe now reads only same-size candidates and verifies full-file SHA-256, so legacy BLAKE3/current SHA-256 rows group together and the engine's sampled large-file rename identity is never treated as byte-exact deletion proof. CLI caps verification at 100,000 candidates/1 TiB, preflights each group, and re-opens the keeper plus current victim immediately before each path operation; a stale member found in preflight removes nothing. TUI caps full hashing to 5,000 candidates/64 GiB and visibly labels partial previews. Default radius-8 similar grouping moved from nine narrow blocks to exact 21/21/22-bit radius-two indexes: brute-force equivalence and 100k bounds pass; the explicit release 250k gate completed in 4.1 s with 8,805,593 comparisons.

macOS Restructure now treats its undo journal as a write-ahead safety boundary: open failures move nothing, partial writes roll back to the prior valid offset, each inverse is durable before its move, partial failure emits terminal counts/error, undo replays backward, and identity-confirmed move-before-DB crash recovery is covered. Tokenized coordinator reservation serializes plan/apply/undo and retains cancellation before task attachment. Welcome's VLM choice is now a Menu with genuinely disabled unsupported Buttons and selected accessibility semantics.

macOS Spotlight now uses one actor-owned operation drain for index/deindex/wipe, coalesces repeated full-index requests, retains failed work for bounded-backoff retries, and keyset-pages 500 rows/items at a time instead of materializing the entire library twice. CLI `runtime install --json` now emits pure JSON on user abort and exits nonzero with `no_source_configured` when installation cannot occur; 31 active CLI unit tests and eight cross-platform smokes pass, with the two macOS process-level contracts queued in Mac CI.

macOS no longer prevents sleep for its whole process lifetime. Sleep assertions are scoped to scans, clustering, Deep Analyze, model installs/prewarm, restructure, and bulk mutations. Bulk rename/trash prefetch path+inode state in 500-ID chunks, reject detected replacements, and keep immediate per-item DB writes so read optimization does not enlarge the baseline crash window; rename also refreshes `path_hash`, and macOS no longer advertises a fake trash-undo batch. Static evidence is `.ralph/baseline/apple-iteration10-static.log`; IPC remains 48/48.

macOS Cleanup Exact no longer treats sampled/legacy stored hashes as byte proof. It selects all-kind same-size candidates, streams full SHA-256 with handle/path identity checks, caps previews at 5,000 candidates/64 GiB with explicit partial/skipped state, and bypasses its short-lived preview cache to rehash each victim against an unselected keeper before Trash. Empty and non-image files now participate without changing the DB recipe. Shared tests cover unsampled differences, stale sizes, changed victims, and same-path replacement identities; static evidence is `.ralph/baseline/apple-iteration11-static.log`. A narrow final path-trash race and native Swift/scale/UI validation remain external risks.

macOS face auto-merge now preflights 20,000-person/250,000-row/768-MiB aggregate/16-KiB per-row embedding ceilings before reading embeddings, accumulates centroids through a cursor, and replaces the large scalar all-pairs edge materialization with a deterministic exact VP-tree threshold join. Candidates are conservatively metric-pruned, scalar-rechecked, and globally sorted exactly as before; 12.5-million evaluation and one-million edge ceilings reject the whole plan before mutation. Direct-vs-indexed, boundary, dense-overflow, and work-overflow regressions were added. Pages build work is read-only, while Pages deploy and signed-release publication receive only their isolated job-level writes; repository policy enforces the allowlist and release output gate. Static evidence is `.ralph/baseline/iteration12-static.log` and `workflow-permissions-iteration12.log`.

Developer setup no longer executes Homebrew/rustup responses: it requires separately installed trusted tools, enforces Rust >=1.90, exact-pins direct RAM++ export inputs, and pins recognize-anything to reviewed Apache-2.0 commit `7cb804a8609e9f4b1a50b7f31436d2df40bb9481`. Repository policy runs on every shell change and rejects remote pipelines/substitutions, unpinned git inputs, and every downloaded executable except the whole-file-digest-pinned AppImage builder. Runtime egress broadening was considered but not applied because Hugging Face-only is binding. CI locks the exact six-URL/four-host GitHub/NVIDIA development blocker baseline; strict publication parses all registry constructors and exact initial/redirect/download guards, then intentionally blocks before staging until approved mirrors replace it. Native bootstrap smoke, provider-backed publication, and Hugging Face mirrors remain external. The whole-repository final gate and later adversarial rounds are still pending.

## 2026-07-12 — Windows release polish: preview fixed, native installers rebuilt, signing fail-closed, stale catalog rows reconciled

Fixed the installed-build Library preview regression against real images on `F:\Adlon Drive`: `ContentDialog` transiently unloads/reparents content while opening, and `FilePreviewSheet` had treated that as terminal forever. Preview loading now starts on `Opened`, teardown runs after `ShowAsync` returns, shell thumbnails require complete reads, and direct decode streams through a dispatcher-owned, 1024-pixel/256 MiB-bounded source. The UIA smoke now proves render → close while app stays alive → reopen → render; the real Adlon JPEG passed with no placeholder.

Rebuilt both native installer surfaces. Burn uses the branded native sidebar license UI, FileID icon/copy, an embedded local Apache-2.0 RTF (no license-network egress), and named controls. MSI uses native `WixUI_Minimal`, deterministic banner/dialog artwork, and the full license. Current-source unsigned x64 package pipeline produced a 167.1 MB bundle + 100.4 MB MSI, privacy scan found zero telemetry markers, WiX ICE validation passed, and both UI smokes reached their real license surfaces. Clean-VM upgrade/repair/uninstall and ARM64 runtime UI remain external gates.

Windows signing is now provider-neutral and fail-closed: local certificate-store or managed-adapter backends, PE → MSI → detached Burn engine → reattach → final bundle order, final embedded-engine re-verification, trusted timestamp presence, subject audit, and an independently configured public-key identity embedded into signed app builds. Runtime verifies both the app assembly and engine against that key pin. Tagged/public publishing remains intentionally impossible until a real provider, protected identity, and separate write-capable publication job are configured; unsigned Windows tool archives are excluded. No real signed artifact was possible without external credentials.

A release audit found ordinary Explorer deletions stayed in the catalog forever. Per owner decision, completed error-free uncapped walks now **soft-hide** unseen rows (`failed=1`) instead of deleting them: user tags/ML metadata survive, reappearance clears the state, and cancelled/GPU-failed/partial/test-capped walks never reconcile. Discovery retains only 8-byte stable hashes, and one set-based SQLite update handles scale; the explicit 1M-row gate passed twice in 1.37/1.43 s. Remaining app summary queries now exclude failed rows. Also fixed the Settings privacy-policy 404 and two real-data harness bugs (long-path pre-count and `-SkipWipe` face-crop assertion).

**Verification:** Rust 1.90 clippy `-D warnings` + all-target tests green (429 lib / 431 bin passed; only explicit perf tests ignored); app tests 237 green and x64 builds 0/0; 1M reconciliation gate green; preview close/reopen smoke green on Adlon; capped non-destructive Adlon scan green at 10 files/s, peak 2,868 MB, 4,238 tags, 199 valid 128-d SFace embeddings; current unsigned package/privacy/checksum/UI/ICE gates green. Residual risks: provider-backed signing + clean-VM matrix, ARM64 installer, full uncapped scan on this exact tree, and media-handle inspection remain unverified.

## 2026-07-11 (cont. 3) — codex's cross-platform working tree finished, verified, versioned to 0.1.1

Owner reinstalled the shipped v0.0.1 `.exe` and hit two concrete regressions plus a broad "make it FAANG-perfect across Windows/macOS/Linux/CLI/TUI" pass. Picked up a large uncommitted working tree (99 files, ~3.2k lines) written by codex, reviewed it end-to-end (parallel per-area reviews), fixed the gaps, and verified every buildable surface.

**The two owner-reported regressions — both already fixed in the working tree, now verified:**
1. **File picker "not working" on the installed build.** The shipped v0.0.1 used the old WinRT `FolderPicker.PickSingleFolderAsync`, which throws an empty `COMException` in an **unpackaged** WinUI 3 app (confirmed in the installed app.log: three `PickSingleFolderAsync threw:` with blank messages). The working tree replaces it with a native **`IFileDialog`** (`FolderPickerService.cs`). Verified the full COM interop by hand: CLSID/IIDs correct, the 24-method IFileDialog vtable in exact SDK order (Show…Close, SetClientGuid, ClearClientData, SetFilter), IShellItem's 5 methods, FOS flags, SIGDN path — all correct; STA-safe (called from UI-thread click handlers).
2. **Welcome modal didn't list everything / didn't pick the right LLM for the machine.** The sheet now renders **all seven installables** (CLIP, RAM++, ArcFace, Deep VLM, BGE, Whisper, GPU pack) and a **machine-sized VLM recommendation** (`VlmRecommendation.cs`) that auto-selects the download target by RAM + dedicated VRAM (DXGI) + arch + free disk. Chain verified live: engine `hardware.rs` probes VRAM/vendor → `HardwareInfo` IPC → `EngineInfo.Hardware` → `UpdateDeepVlmRecommendation` (fires on the engine `Info` event) → `SelectDeepVlmModel(recommended)`. On this box (RTX 5080 16 GB, `highEndGpu` → Mistral tier) it recommends Mistral-Small 3.2 24B. macOS/Linux mirror it (unified-memory / CPU-only tiers respectively).

**Engine (cross-platform, high quality):** disk-preflight before any model download (shared `required_install_free_bytes`, both async prewarm + CLI blocking paths); cross-platform `available_disk_bytes` (Win `GetDiskFreeSpaceExW` / Unix `statvfs`); real Linux `watch_parent` parent-death detection (was a no-op TODO); VLM runner gains Unix PATH lookup + per-OS binary header checks (PE/ELF/Mach-O); `FILEID_DB` override + `hf_cache_dir` now tracks `FILEID_MODELS_DIR`; manifest adds Whisper runtime + base model (both **whisper.cpp, MIT**, SHA-pinned, HF/GitHub hosts). Windows arm64 runtime-dep pins (onnxruntime/DirectML/pdfium) added — **all 8 arm64 + x64 DLL hashes verified locally by running `fetch-runtime-deps.ps1` for both arches**.

**CI / packaging / versioning:** coherent **0.1.0 → 0.1.1** bump across the whole tree, self-enforced by `package-tools.py`'s cross-manifest assertion; `release.yml` now enforces `tag == VERSION` (so the next cut is **v0.1.1**, resolving the old tag/version drift — ProductVersion 0.1.1 > the installed 0.1.0, upgrades in place). New `tools.yml` builds/tests/packages the CLI+TUI+engine "FileID-tools" bundle across 6 targets (`macos-15-intel` confirmed a valid runner label); Linux GTK-app clippy + Flatpak promoted from advisory to **required** gates; new `check_binary_privacy.py` is byte-for-byte additive to the inline telemetry-string gates (not weaker). Consolidated Rust toolchain pinning: root + linux `rust-toolchain.toml` added, all five files agree on **1.90**.

**Fixes I made on top of codex:** relocated a `#[cfg(test)]` module in `deep_analyze.rs` that tripped `clippy::items-after-test-module` under `-D warnings` (only real CI breakage found in Linux/CLI/TUI); normalized 5 Windows `.cs`/`.xaml` files to CRLF+BOM (codex left mixed EOL + one missing BOM → would fail the charset/format gate); gitignored the untracked 165 MB root `dist/` tool-bundle output.

**Verified:** engine clippy + tests (Windows); app build 0/0 + tests + `dotnet format` clean; WSL **1.90** clippy + tests green for Linux app, CLI, TUI (incl. the relocated recommendation tests); both Windows runtime-dep arches hash-verified. Apple reviewed by inspection (can't compile Swift here): compiles-clean, test target auto-wired, 5 recommendation tests pass by hand-trace — **3 macOS-only follow-ups deferred to NEXT.md** (Intel-Mac ORT install hard-fail, missing Whisper row, menu-picker `.disabled` no-op).

## 2026-07-11 (cont. 2) — "everything says not-installed" fixed (3 root causes) + deep app audit (owner-reported)

Owner ran the installed app and hit widespread "X isn't installed" errors + repeated CUDA-pack install spam despite a fully-provisioned Models dir (all 13 sentinels valid, weights verified complete on disk). Three distinct status-lie root causes, all fixed with regression tests:

1. **Engine `find_weights` kind-vs-dir mismatch** (`models/vlm.rs`): probed `vlm/<snake_kind>` (`mistral_small_3_2`) while the installer writes the registry's dotted dir (`vlm/mistral-small-3.2`) → Deep Analyze reported EVERY installed VLM missing (`vlm_model_missing` on a complete install). Now resolves through `registry::lookup_full` (accepts both spellings) with the naive join as fallback. + registry test pinning that both spellings of every VLM kind agree on one dotted dir.
2. **App sentinel probes only matched flat `{id}.installed`** while the engine writes revision-keyed `{id}-{hash}.installed` → Settings never latched "Installed" for CUDA llama.cpp/ORT-CUDA/OpenVINO packs, and a ToggleSwitch echo re-dispatched the CUDA install on every Settings visit (the owner's repeated-progress spam; engine kept answering `already_installed`). New shared `Services/SentinelProbe.cs` (both forms, prefix-collision-safe) now backs `ModelInstallerService`, `SettingsView`, `CudaAutoInstaller`, `LlamaRuntimeAutoInstaller`; toggle-echo no-op guard added. 7 tests.
3. **App-side twin of #1** (`DeepAnalyzeView.VlmWeightsPresent`): same snake-kind probe → Deep Analyze cards showed "not installed". New `Services/VlmWeightDirs.cs` kind→dir map. + tests.

**EP guard: verified clean on disk** — the 2026-07-11 WinRT crashes did NOT latch any EP (crashes post-dated bind disarm); no `.ep_disabled` anywhere. OpenVINO probe deepened to match the CUDA gate (recursive provider-DLL check) so advertised EP can't diverge from the bind chain.

**Six-tab audit fixes:** startup **library-root recovery** (`Services/LibraryRootRecovery.cs`) — `lastFolderPath` pointing at a deleted dir (the codex 07-08 scratch corpus) previously mounted a Library showing the DB's files under a dead root with misleading `empty_folder` errors on rescan; now falls back to the DB's most recent existing scan root with a dismissible banner (unplugged-drive case deliberately no-ops). Engine death mid-Deep-Analyze now synthesizes a Cancelled completion in `EngineClient.Cleanup` (banner/tab no longer wedge forever); Restructure releases its apply guard on engine respawn via `SpawnGeneration` (tab no longer bricked for the session after a mid-apply crash); unguarded `ContentDialog.ShowAsync` in 4 destructive flows wrapped (double-dialog crash → Cancel). 10 new tests.

**Verified:** engine clippy + 424 tests (incl. new); app build 0/0 + 206 tests + IpcSchema 48 + format; WSL Linux clippy. Deferred (NEXT.md): ep_guard latch visibility + app-side stale-sentinel truthfulness (both need a modelsStatus/HardwareInfo IPC schema addition), shallow qnn probe, Settings CUDA button flip-on-send, engine `scan_root_missing` distinct error kind.

## 2026-07-11 (cont.) — CRITICAL native crash on real data root-caused + fixed; full 71k Adlon scan GREEN at 43 f/s

**The bug (0xC0000005, mid-scan, silent):** scanning the real family drive (`F:\Adlon Drive\Family Shared`, 71,333 files) killed the engine ~2 min in — reproduced **4/4 times at the identical corpus position**, with nothing in the engine log (native AV never reaches tracing), nothing in WER, and the iterate harness blind to it. Root-caused via procdump minidump: **fault READ inside the former address range of WinTypes.dll, which sat in the dump's unloaded-module list** next to Windows.Media.Ocr.dll / windowscodecs.dll / Bcp47Langs.dll. Mechanism: every shell/WinRT call site (OCR/HEIC/video) does per-call `CoInitializeEx`/`CoUninitialize` on tokio blocking-pool threads; when a corpus stretch produced an OCR-idle gap, the last uninit dropped the process MTA to zero → Windows **unloaded the WinRT stack** → the `windows` crate's process-wide factory cache kept dangling pointers → the next OCR call read a dead vtable. Timing-dependent: the harness's slow async stdout consumer created the gap (4/4 deaths); a fast-consumer driver never crashed (30+ min). The 1,955-file subset containing the "suspect" PNG passed — the last-logged file was pipeline position, not the victim.

**The fix:** pin the MTA for process lifetime with `CoIncrementMTAUsage()` at engine startup (`main.rs`, before the tokio runtime) — the WinRT DLLs and cached factories can then never unload. Cookie intentionally never decremented.

**Verification (on-hardware, RTX 5080 + USB-SSD Adlon drive):** full 71,333-file iterate run — **all assertions GREEN, 43 files/sec** (above the 40 f/s internal-drive baseline; the drive is a Micron X9 USB SSD, no seek penalty, so the new decode clamp correctly does not engage), peak RAM 6,992 MB (< 8,500 cap), 84,582 SFace embeddings, person clusters sane (largest 6,782 faces). Scan+cluster 1,478 s, zero crashes, procdump armed and silent.

**Also landed with this:** iterate.ps1 now registers an `Exited` handler that prints the **engine exit code** on any mid-scan death (this run's `0xC0000005` line was the pivotal clue); a Trace-gated `decode start` forensic line in the decoder pool; AppImage script hardening (raster icon fallback + root-icon pre-place) — the AppImage refresh itself is **blocked by a linuxdeploy-continuous icon regression** ("Could not find suitable icon" even with valid 256px PNG + .DirIcon pre-placed, 3 attempts) — the v0.0.1 release keeps the 07-05 AppImage; Windows installers were rebuilt post-fix and re-uploaded.

## 2026-07-11 — Post-merge multi-agent audit: 12 findings, 11 fixed (2 HIGH data-integrity), macOS CI fixed

Ran an adversarially-verified multi-agent audit over the merged `scale-and-paged-plans` branch (4 dimensions: merge-loss, engine coherence, WinUI runtime, perf-scaling). 12 findings, 8 confirmed by refutation agents (0 refuted), 4 verified by hand (all real). All fixed except two deliberately deferred (NEXT.md).

**macOS CI breaker (fixed):** the codex-written `Restructure.swift` never compiled — five GRDB calls inside an async fn resolved to the async overloads without `await` (lines 95/162/183/210/238) + Swift-6 captured-var warnings. Fixed with `try await`, an immutable keyset-cursor copy, and chunk-base+offset sequencing instead of closure mutation.

**HIGH — v0.0.1-upgrade heal regression (engine, fixed + regression-locked):** upstream's BLAKE3→SHA-256 switch left `legacy_content_hash` reproducing only "recipe v1" (head‖tail‖size) AND gated the legacy probe on `size > FULL_HASH_MAX_BYTES`. But shipped v0.0.1 stamped **full-file BLAKE3 under-cap** and **recipe v2 (head‖4×64 KB interior‖tail‖size) over-cap** — so NO pre-switch row could ever heal by content hash; a cross-volume move (file_ref is volume-local) orphaned tags/persons/embeddings. Now `legacy_content_hashes()` returns both shipped recipes in one read, the probe runs at any size, and `HEAL_LOOKUP_SQL` widened to `IN (?2, ?4, ?6)`. 5 new tests incl. two end-to-end heals seeded with the exact v0.0.1-stamped digests.

**WinUI paged-plan UX (all four real, fixed):** (1) `plan_restructure_store`/`plan_restructure_db` error kinds weren't in RestructureView's filter → spool-persist failure froze the tab on "Computing plan…" (now prefix-matched `plan_restructure*`); (2) a mid-stream spool read error aborted `apply_iter` with `Err`, discarding partial counts → app claimed "your files are unchanged" and never showed Undo despite a fsynced journal (engine now returns a truthful partial result counting the unread remainder as failed; app copy fixed; test added); (3) a fresh plan arriving mid-apply released the static `_applying` guard → second concurrent apply would truncate the first's undo journal (new `_applyInFlight` gate + testable release helper); (4) Cleanup Similar-mode >20k cap exception left stale Exact groups rendered under the Similar header (mode-stamped groups; cross-mode failure clears the list). 11 new C# tests (app tests 174→185).

**Merge-loss restores:** CLI macOS trash guard (`trash_unavailable_on_this_platform()` before the confirm prompt + `{aborted, reason}` JSON contract) and the TUI's sixth DeepAnalyze companion tab (hotkey 6, tests; README already documented it). Upstream's HF-only download-host test had been grafted at merge time.

**Perf-scaling for the dev box (9900X/24T + RTX 5080 + USB "F:\Adlon Drive"):** the decode pool sized [2,12] by CPU topology alone — 12 concurrent blocking reads thrash seek-penalty media. `Tagger.with_scan_root()` now clamps the pool to 4 on Hdd/Removable/Network via the existing `storage_type_for_path` probe (discovery already used it); NVMe unchanged. Also: stale `.planning.sqlite` scratch orphans are now swept before each new large-plan (Windows + Apple mirror), placed pre-creation so the live scratch is never unlinked.

**Verification (combined tree):** engine clippy + 420/422 tests; CLI clippy + 18/8; TUI clippy + 83; app build 0/0 + 185 + IpcSchema 48 + `dotnet format`; WSL Linux-1.90 clippy. macOS Swift compile-verified by CI only.

## 2026-07-10 — Paged / truncated restructure plans finished + verified (cross-platform)

Picked up an **uncommitted, undocumented** in-flight feature ("codex" work in the working tree, not in any STATE/NEXT entry) and completed the loop: audited it across every platform, found it code-complete (no stubs/`todo!()`/placeholders anywhere), and **verified everything buildable on this Windows box is green**.

**The feature — paged/truncated restructure plans (million-file scale).** Previously the restructure planner materialized every `(source→destination)` move in memory and shipped the whole list across IPC just to draw the Sankey preview — O(N) memory + wire on a 500k-file library. Now: `planRestructure` gains `supportsPagedPlans` (client opt-in); when the plan exceeds the preview cap the engine **spools the full move list to disk** (`<planID>.ndjson`, a versioned header + one move per line, written atomically tmp→rename, stale spools cleared first) and returns only a **bounded preview (≤5000 moves) + opaque `planID` + exact `totalMoves` + `truncated=true`**; `applyRestructure` then takes the `planID` and **streams the spooled plan** through `apply_iter` instead of echoing every move back over IPC. Schema (`ipc.schema.json`) is the contract; per-platform DTOs mirror it (there is no codegen — DTOs are hand-maintained + guarded by conformance tests).

**Per-platform status:**
- **Windows** — COMPLETE + VERIFIED. Engine spool (`commands/restructure.rs`), `restructure_plans_dir()` (`paths.rs`), streaming apply (`restructure_apply.rs::apply_iter`), C# DTOs + app opt-in + apply-by-`planID` (`RestructureView.xaml.cs` hides per-row surfaces + gates apply on `PlanId` when truncated). `cargo check` ✓ · `clippy -D warnings` ✓ · **engine 414 tests** ✓ · **IpcSchema conformance 48** ✓ · **app build 0/0** ✓ · **app 174 tests** ✓ · `dotnet format --verify-no-changes` ✓.
- **Apple** (reference) — COMPLETE by inspection; `Restructure.swift` has the full disk-spool (`storePlanStream`/`applyStoredPlan`) + `proposeLargeStoredIfNeeded` million-file streaming path + tests. **Unverified-until-Mac** (can't build Swift here).
- **CLI** — COMPLETE + VERIFIED. In-process (no IPC), so it uses a native RAII `PlanSpool` streaming the full plan to `--json` / apply without buffering. `clippy` ✓ · **8 tests** ✓ (built on Windows).
- **Linux** — COMPLETE by inspection (opts in, handles `truncated`, applies via `plan_id`); DTOs come from the shared engine crate. **Unverified-until-Linux** (GTK app needs Linux; Rust field-alignment is correct by reading).
- **TUI** — N/A: read-only 3000-row preview, no restructure-apply surface. Not affected.

Same-batch sibling scalability changes in the tree (all verified by the above compiles/tests): discovery skip-cache keyed by a 128-bit BLAKE3 `SkipFingerprint` instead of full `PathBuf` (`discovery.rs`), and Cleanup dup-group preview truncation.

**Not yet committed.** The whole working tree is dirty (this feature + prior excludedPaths churn) and shows tree-wide CRLF normalization noise (no `.gitattributes`; files written LF vs committed CRLF). Landing on a branch is the next step, pending owner go-ahead.

## 2026-07-09 — windows-prod-hardening landed (PR #89); FileIDSetup.exe installer working; v0.0.1 assets refreshed

Landed the in-flight `windows-prod-hardening` branch to `main` after full local verification + an adversarial review pass, then produced the first working single-EXE installer and refreshed the v0.0.1 release assets. All real CI green (engine x64/arm64-native/arm64-cross, .NET app x64/arm64, macOS SwiftPM, Linux CLI/TUI/GTK/engine); the lone red is the known Flatpak advisory.

**Engine perf** (deferred NEXT.md levers, now on-hardware-measured on RTX 5080/CUDA): EP-aware CLIP dispatch (CUDA/TensorRT → Session pool, measured 40.96s vs 50.68s batched, identical assertions; DirectML keeps batching); planar CHW preprocessing (MobileCLIP + RAM++) with a byte-identity golden test; RAM++ takes the batch coordinator only when the ONNX exposes a dynamic batch axis, else falls back to the single-image pool.

**Engine robustness** (audit 2026-07-08): trash restore recovers the physical extension when Explorer hides known extensions (the batch silently restored nothing before) + O(1) reconciliation off the writer lock; bulk rename does its filesystem moves with no writer lock held; restructure undo keeps its journal on partial failure.

**macOS/Linux lockstep**: mirrored the Windows user-folder-exclusions feature (`startScan.excludedPaths` + `purgeExcluded`) across the Swift + Linux engine clients + schema round-trip tests. Review find (fixed): the macOS immediate purge compared `lower(path_text)` (NFD on disk) against an NFC needle → accented excluded folders purged nothing; now queries the NFC `path_search` column.

**CI drove out three real breakers** (the "loop until green" paid off): (1) CLI/TUI didn't set the new `StartScanPayload.excluded_paths` (missing-field compile error); (2) `crossbeam-epoch` <0.9.20 newly flagged by RUSTSEC-2026-0204 → bumped to 0.9.20 (MSRV-1.90-safe, cargo-deny green); (3) two session-changes `.xaml` views lacked the UTF-8 BOM the charset gate requires. Also reverted a local-only Cargo.toml lint rename (`unchecked_time_subtraction`) that would have failed CI's pinned 1.90 (this box's on-PATH clippy is 1.96, which masked it).

**Installer**: fixed the Burn bundle theme (`hyperlinkLicense` → `rtfLicense`; the former needed a nonexistent `WixStdbaLicenseUrl` and blocked every clean build) — `publish-bundle.ps1 -SkipSign -SkipArm64` now produces `FileIDSetup.exe` (x64 single-EXE bootstrapper, ~83.5 MB) + `FileID-x64.msi` end-to-end, privacy gate green. Directory.Build.props VS18 AppxPackage probes are what let `dotnet publish/build` (hence the whole installer) run on a VS18-only box.

**Release**: per owner, kept VERSION/ProductVersion at 0.1.0 and the `v0.0.1` tag (a 0.0.2 MSI would be a lower ProductVersion and wouldn't in-place upgrade the shipped 0.1.0 install). Refreshed the v0.0.1 pre-release assets with the rebuilt MSI and added the new `FileID-0.0.1-Setup-x64.exe`. Both UNSIGNED (no EV cert) — SmartScreen will warn on first run.

## 2026-07-05 (cont.) — 5 on-hardware Windows-app bug fixes (PR #88); 0.0.2 pending owner test

Owner ran the installed 0.0.1 app and reported 5 bugs; all fixed + merged (PR #88, CI green). NOT runtime-verified here (no WinUI runtime) — owner testing the rebuilt MSI (`~/Desktop/FileID-bugfix-test-x64.msi`) before a 0.0.2 release.

- **Preview arrow keys dead** (HIGH): `FilePreviewSheet.HandleKeyDown` bailed on `if (e.Handled) return;` as its first line — defeated the host's `AddHandler(…, handledEventsToo:true)` that exists precisely because the ContentDialog pre-marks Left/Right/Space Handled. Removed.
- **Preview image blank** (root cause unconfirmed → two safe additive fixes): (1) reset `_unloaded` in `OnSheetLoaded` — a ContentDialog Unloaded→Loaded cycle during open latched `_unloaded=true`, short-circuiting every image write; (2) `TryDirectImageDecodeAsync` — direct BitmapImage decode-from-stream fallback when the shell thumbnail returns Size==0 (FilePreviewSheet was the one image callsite missing it). Prev/Next buttons had no bug (only looked dead because the image never changed).
- **Library tiles not staying loaded** (HIGH): cancelled thumbnail loads were mislabeled as render FAILURES (LibraryView.LoadThumbAsync null branch didn't check `ct.IsCancellationRequested`) → broken-glyph over a valid tile; and the ThumbnailService bounded(256) DropOldest channel dropped the oldest request WITHOUT completing its TCS → a visible tile's awaiter hung forever. Fixed both (cancelled = silent drop; channel → unbounded, since per-request cancellation sheds stale work O(1) at drain).
- **Speech model not on onboarding** (HIGH): Whisper was installable only via Settings → Models. Added an optional Whisper card to the WelcomeSheet (own Install button; excluded from `AllInstalled` so it never gates completion). macOS correctly has NO such card — it uses built-in **Apple Speech** (`DeepAnalyzeNaming.swift`), no download; the divergence is right, not a parity gap.
- **No auto-launch on install** (HIGH): `FileID.Msi` had no launch action. Added a first-install-only, impersonated, asyncNoWait CustomAction after InstallFinalize (WiX `msi validate -wx` passes).

Rebuilt the fixed MSI (83.5 MB, WiX-validated). Version still 0.1.0 (mismatch with the v0.0.1 tag persists — resolve at 0.0.2 cut).

## 2026-07-05 (cont.) — NEXT.md doable-headless backlog cleared (PR #86 + #85 installers)

Ran a `/loop` over the 1841-line NEXT.md: a triage agent split every item into DONE-already, BLOCKED (needs a Mac / EV cert / on-hardware GPU profiling / real Linux hw / the F:\TrueNAS scan / a repo Pages toggle / deferred future-phase features), and DOABLE-headless. Landed **every** doable item (PR #86, all real CI green; the lone red is the known Flatpak advisory):

- **installer:** `FileID.Msi.wixproj` version-drift gate's `%(Identity)` batching parses empty under both VS msbuild + `dotnet build`, tripping a false "drift" that blocked EVERY local MSI build — now errors only on a real (non-empty) mismatch. Also produced the actual **v0.0.1 single-file installers** (PR-adjacent): `FileID-0.0.1-Setup-x64.msi` (WiX) + `FileID-0.0.1-x86_64.AppImage` (linuxdeploy GTK), which replaced the folder-zips on the release.
- **engine:** CPU-clamp ML-pool log `warn!`→`info!` (correct/expected on a CPU box, not a fault); `FaceClusteringResult.unmatched_faces` was hardcoded 0 → now reports the suppressed `person_id=NULL` count.
- **cli:** `search` restored intra-tier FTS relevance (was sorting same-tier hits by file id, discarding `ORDER BY rank`).
- **app:** 6 `Convert.ToInt32` COUNT/SUM reads → int64 + clamp.
- **build/docs/packaging:** `scan_assertions.py` no longer red-trips zero-clusters on a <10-face corpus; DECISIONS.md documents the shared `FILEID_FACE_*` per-platform defaults; AUR `optdepends` gains libheif; Flatpak manifest notes the `heif-dec` gap.

Deliberately NOT done (marginal, per triage): FileTile materialize-once (`x:Bind` reads once), MergeById tail (primary O(n²) already fixed), TUI redraw dirty-flag (cadence unconfirmable), CLI `has_text` monotonic (by-design). Everything else remaining in NEXT.md is BLOCKED off-box.

## 2026-07-05 (cont.) — 0.0.1 SHIPPED: adversarial audit + dead-code sweep + macOS parity mirror → first public pre-release

Cut **[v0.0.1](https://github.com/WebWorldWide/FileID/releases/tag/v0.0.1)** (pre-release) after a release-hardening pass (PR #85, merged `d4d15ed`, all main CI green: engine x64/arm64/cross, .NET app x64/arm64, macOS SwiftPM, Linux, Flatpak).

- **Adversarial pre-release audit** (3 parallel review agents + a Restructure-tab review). Restructure verdict: release-ready (butler-grade clobber-proof apply/undo, survived prior data-loss audits; UI fully wired). Findings fixed: (1) the `[THUMB]` log firehose was only half-gated — 8 hot per-tile lines in `LibraryView` still did synchronous UI-thread disk I/O on scroll at Debug → moved to Trace + `PathRedactor.Redact`; (2) `SankeyFlowControl` had no UI-Automation surface → added accessible Name/HelpText + `IsTabStop`; (3) engine env hardening — reject non-finite (NaN/inf) env values + clamp cosine knobs to [0,1]; (4) stale doc comments (mutual-kNN default-on).
- **Dead-code sweep** (~1,968 lines, survey-driven, each removal grep-verified). Engine: removed unwired future-phase scaffolding (`cluster_suggestions`, `usn`, `elevation`, `florence2`, the YuNet-superseded SCRFD detector, a dead field) — tracked in NEXT.md; kept everything verified WIRED (job_queue, keywords, wordpiece_tokenizer, doc_extract, audio_meta). C#/Swift: removed accidental orphans + unused visual primitives (IridescentBorder, CompletionRipple, BadgePill, ThemedTogglePicker, dead SpringEasing helpers, EmptyStateView action subsystem, etc.); kept GlassCard (foundational glass surface the design system anchors to), ThemedSegmentedControl, ShimmerView, Swift BadgePill.
- **macOS parity mirror** (UNVERIFIED-UNTIL-MAC; macos.yml compile-verified green): added the mutual-kNN + pre-clustering quality-gate MECHANISMS to the Swift engine, both DEFAULT-OFF (zero behaviour change). The Rust VALUES can't transfer blind (Apple Vision quality scale + FaceAlign unwired) — macOS still needs its own on-Mac label-calibration pass (the face-labeler tool works there). Documented in MACOS_LOCKSTEP_NOTES + SHIP.
- **Release artifacts** (unsigned, built here): Windows app (self-contained x64 zip) + Windows tools (engine/CLI/TUI) + Linux (GTK app + engine + CLI/TUI tar.gz). No macOS binary (needs a Mac) and no signed installer (needs EV cert) — both noted in the release.

## 2026-07-05 (cont.) — Face clustering: LABEL-DRIVEN retune (People-tab F1 → 1.0 on the owner's labelled set)

The owner hand-labelled ~185 faces across ~12 people (via a new self-contained face-labeler HTML tool: `scripts`/scratchpad generator → base64 crops → export `labels.json`). Those ground-truth labels **overturned the cohesion-only guess** from the entry below (pass1=0.82) and gave the first real precision/recall.

**What the labels revealed:**
- On real same-age same-person pairs SFace works WELL — same-person cosine median **0.59** (p90 0.82), different-person median 0.16, MAX only **0.47**. Clean separation; optimal link threshold **~0.43–0.50, not 0.82**. At 0.82 recall was **~1%** (near-duplicates only) — the true cause of people fragmenting.
- **Two confounds** that had hidden this: (1) a person across a big AGE gap (child↔adult, e.g. the owner) is genuinely unmatchable by any face model — those correctly land in separate clusters (manual/"Suggest merges" unites them). (2) LOW-QUALITY faces (this corpus is scanned/old, quality caps ~0.42) give noise embeddings — same-person cosine on quality<0.35 faces is ~0.14 (==different-person) and they chain into cones.

**Fix (defaults, all env-overridable):**
- `pass1_cosine` 0.82→**0.50**, `pass2_cosine` 0.74→**0.45** (link in the same/diff gap).
- **mutual-kNN default ON** (`FILEID_FACE_MUTUAL_KNN`, was off) — kills the last single-bridge chaining; lifted recall to 1.0 with no fragmentation.
- **NEW pre-clustering quality gate** `FILEID_FACE_CLUSTER_MIN_QUALITY` (default **0.35**, `commands/face_clustering.rs`) — drops only the deepest noise (below the real faces at ~0.375) so it doesn't chain into cones; gated faces stay UNCLUSTERED (searchable). This mild gate lifted labelled F1 from 0.89 to a clean **1.00**.

**Measured (owner's labels, non-Adam identifiable people): precision 1.0 / recall 1.0 / F1 1.0** (vs F1 ~0.02 at the shipped 0.82). Adam stays age-split (correct). Verified: clippy clean, 414 engine tests green; baked defaults reproduce F1 1.0; subset iterate GREEN. **Supersedes the pass1=0.82 in PR #83** (that was a cohesion proxy; labels prove 0.50 is right). Follow-ups: cross-corpus labels to confirm the thresholds generalize; a stronger face embedder is the only lever for cross-age / very-low-quality faces (NEXT).

## 2026-07-05 (cont.) — Face clustering REDESIGN: eliminated the mega-cone over-merge (Pass-1 hub-bridge fix, cohesion-validated, no labels)

Follow-on to the singleton-flood floor fix below — user authorized taking on the deeper over-merge. **Validated WITHOUT labels via intra-cluster COHESION**: genuine same-person SFace cosine is ~0.85+, so a cluster whose members are mutually ~0.85+ is one identity. Confirmed the embedding *does* discriminate — 582 small clusters on the 44k-face set sit at cohesion ≥0.85 — so the mega-cones were separable-in-principle (a clustering bug), NOT an embedding ceiling.

**Root cause of the 10,690-face "person":** Pass-1 single-linkage at cosine **0.66** chained different people through "hub" faces (a generic face that is 0.66+ to many different people) into cones — median PAIRWISE cohesion ~0.30 but to-centroid ~0.61, so it slipped just past the Pass-3 split floor (0.60) and was never split. Neither `FILEID_FACE_MUTUAL_KNN=1` nor a deeper Pass-3 split cap broke it (both measured — cheap knobs do nothing); `consolidate()` re-merged any split pieces.

**Fix (`identity_clustering.rs` + `face_clustering.rs` defaults; all env-overridable, calibrated on F:\TrueNAS via cohesion):**
- `pass1_cosine` 0.66→**0.82** — cut hub-bridges (0.66–0.80) while keeping same-person edges (0.85+); cones stop forming.
- `pass2_cosine` 0.54→**0.74**; `AUTOMERGE_COS_DEFAULT` (consolidate) 0.75→**0.88** — re-join same-person fragments without re-gluing cone pieces.
- Exposed `FILEID_FACE_PASS1_COSINE` / `_PASS2_COSINE` / `_PASS2_MARGIN` / `_PASS3_MAX_SPLITS` as new env knobs (were hardcoded).

**Measured on 44k faces (faces in coherent ≥0.75 clusters / largest cluster):** the sweep was 0.66→14%/10,690, 0.78→36%/6,047, 0.82→**67%/1,586**, 0.85→89%/429. Shipped **0.82** as the balance: 0.85 fully eliminates the cone but craters recall (on the sparse 1k subset it clustered only ~75 faces), so 0.82 keeps a 7× smaller, far-less-egregious residual cone (cohesion 0.63 vs the old 0.30) with materially more recall.

**Tradeoff (honest):** precision-biased per the design's explicit over-split-safe stance. Recall drops (~43% of embedded faces clustered on the 44k set) and the same person may span several clusters — mitigated by the app's "Suggest merges", and unclustered faces stay searchable. A mutual-nearest re-merge stage to recover recall without re-coning, plus full-corpus + labelled-set validation, are the remaining work (NEXT).

**Verified:** engine clippy clean + **414 lib tests green**; subset iterate GREEN (all 13 assertions), clean small clusters, no cone; baked defaults reproduce the 44k numbers.

## 2026-07-05 (cont.) — Face over-clustering: singleton flood FIXED (safe threshold); mega-merge + fragmentation found at scale (deferred, needs labelled data)

The People tab's biggest shippability gap. Pre-fix full-corpus baseline: **10,208 persons from 84,629 faces**. Investigated on the 1k-file subset (1,011 faces) AND a partial ~44k-face scale set (from a timed-out full-corpus re-scan — the Adlon external drive ran the scan at <17 f/s and hit the 60-min cap; the partial DB was reused for clustering experiments, which need no re-scan).

**What I fixed — the SINGLETON flood (safe, verified, shipping):** `solo_quality_floor=0.12` was a macOS Apple-Vision value; the code ASSUMED Windows scored on 0..~0.95. Measured, the Windows `face_quality` (YuNet det.score × landmark geometry, `scrfd::validate_face_geometry` — `geom_conf` structurally caps ~0.42) is compressed to **~0.23..0.42**, so 0.12 admitted *every* single face. Raised to **0.40** (~p90 of the real range). Subset (min=3): 438→34; scale: singleton flood gone (56 singletons on the 44k set). Pure suppression — no identity merged; suppressed faces → `person_id=NULL`. `min_cluster_size` kept at **3** (I briefly tried 2; at scale that keeps ~3,800 size-2 pairs — a pair flood — so reverted).

**What the scale test EXPOSED (NOT fixed here — deeper clustering problems):**
- **Bridge-face mega-merge.** The largest "person" on the 44k set is an **11,665-face blob whose members' median intra-cosine is ~0.30** (i.e. many DIFFERENT people chained together, embeddings valid/L2-normed). `FILEID_FACE_MUTUAL_KNN=1` did NOT break it (chains still form through dense mutual-edge regions; the blob even regrew via Pass-2/consolidate), and Pass-3's 2-means split cap (7 → ≤128 pieces) can't shred a blob that large.
- **Size-2/3 fragmentation.** HNSW at scale spawns many tiny clusters.
- Net at scale with the shipped defaults (min=3/floor=0.40): ~1,566 persons — far better than ~10k, but the mega-cluster + fragments mean the People tab is not yet "sane" at 44k+ faces.

**These are a clustering-ALGORITHM problem, not a threshold one, and can't be validated without a LABELLED library** (cluster counts don't tell you precision/recall). Deferred to a scoped ML effort — see NEXT.md. The threshold fix that IS shipping is a strict improvement with zero regression risk. Engine: clippy clean, 414 lib tests green (updated default-assertion test). macOS keeps its own Apple-Vision floor (intentional divergence).

## 2026-07-05 — M5/M6 unblocked + landed: WinUI now builds on the dev box; DebugLog Trace-gate + Sankey teardown

The WinUI build blocker is gone. Plain `dotnet build` can't build the app (the SDK lacks the MrtCore/PriGen `AppxPackage` task DLL), but **VS 18 Community** is installed with the Universal + WindowsAppSdkSupport.CSharp workloads — `"C:\Program Files\Microsoft Visual Studio\18\Community\MSBuild\Current\Bin\MSBuild.exe"` has the `v18.0\AppxPackage` tooling, so the app + tests build the same way CI does (msbuild, not `dotnet build`). Recorded the full recipe (RID restore → build `-restore:false`; `dotnet format` clobbers RID assets so re-restore before rebuild; App.Tests need VS-msbuild build then `dotnet test --no-build`; VS18's newer Roslyn flags CA1861 that CI's SDK-8 analyzers don't, so build tests with `-p:RunAnalyzersDuringBuild=false` to mirror CI) in auto-memory.

**M5/M6 shipped on branch `m6-debuglog-trace-gate` (pushed; PR/merge pending user gh auth):**
- **DebugLog Trace-gate (the one genuinely worthwhile WinUI perf change).** `DebugLog` did synchronous, `s_writeLock`-held file I/O on the UI thread for every `[THUMB]` line — and those fire per-tile on every scroll realization, stalling the render thread on disk during a fast scroll. Added a `Trace` level gated OFF by default (`FILEID_LOG_TRACE=1`/setter) that skips the lock + I/O entirely when disabled, and moved all 25 per-tile `[THUMB]` `Debug` lines onto it (the 3 `[THUMB]` `Warn` failure lines stay). Everything at `Debug`+ stays always-on + synchronous, so the `[APPLY:N]`/`[ENGINE-SUB]`/fast-fail forensic tail (CLAUDE.md: load-bearing) is never suppressed.
- **SankeyFlowControl `Unloaded` teardown.** The `_renderDebounce` timer was never stopped on unload; a pending `Tick` held the detached control alive via its `RenderIfResized` closure across a tab-swap-mid-resize. Stop it on `Unloaded`.
- **Verified:** app builds clean (0 warn/0 err, VS18 msbuild); `dotnet format FileID.sln --verify-no-changes` clean; **209 tests green** — 159 App + 46 IpcSchema + **4 new `DebugLogTraceGateTests`** that lock the contract (Trace suppressed when disabled, written when enabled, Debug/Error always written regardless of the flag); runtime launch (staged Debug build + engine + ORT against the scanned corpus DB) starts clean with the changes and shows `[THUMB]=0` at default level.
- **Audit-dissolved M5 items (left as-is, correctly):** `MergeById` O(n²) is sub-ms (grid capped at 200); `Convert.ToInt32` overflow already moot (sizes are `long` end-to-end); `FileTile` display is a micro-opt; `IridescentBorder` is dead (only its own Style/template reference it — no instantiation), left in place as an unused signature primitive; welcome-sheet sentinels already fixed by #73.

## 2026-07-04 (cont.) — M11 endgame adversarial audit: fixed a silent DATA-LOSS bug; C# app confirmed clean

Fanned out adversarial audits over every surface I can verify (engine scan/DB, engine ML/IPC, Linux GTK app, WinUI C# app). Biggest find of the whole hardening pass:

- **[DATA LOSS — FIXED + CI-green] rename-heal clobbered a live copy's row + user metadata.** `files.path_text` is `UNIQUE ON CONFLICT REPLACE`, so the heal's plain `UPDATE path_text` silently REPLACE-deleted a live row already at the target path and FK-cascaded ITS user tags + person/name assignments. The call-site guard meant to skip this (catch ConstraintViolation) was DEAD CODE — a plain UPDATE never raises under ON CONFLICT REPLACE. Trigger: two identical files, user tags one, deletes the other, rescans the survivor. Proven empirically against SQLite; fixed with `UPDATE OR ABORT` (the guard now engages), regression test added. A follow-up verification pass is auditing for sibling instances of this bug class (restructure_apply / bulk rename use the same plain UPDATE — claimed safe behind no-clobber FS moves).
- **[FIXED] CLIP encoders (image+text, 4 paths) lacked an output-dim guard** — both the engine ML/IPC audit and the macOS audit flagged it independently; now bail on width != 512 like SFace/RAM++/BGE.
- **[FIXED] Linux restructure Undo** sent the wrong root after a destination switch.

**WinUI C# app — audited, confirmed in excellent shape.** The high-value target (a 4th native fast-fail variant of the three documented ThumbnailService/ModelSlot/SidebarQueueList bugs) does **NOT exist** — every off-thread PropertyChanged path re-marshals through DispatcherQueue. No data-loss, wrong-file, or lifecycle race. The NEXT.md perf items were verified with **corrected (lower) impact**: `Convert.ToInt32` size-overflow **dissolved** (sizes are `long` end-to-end); `MergeById` O(n²) is sub-millisecond (grid capped at 200); `FileTile` display is a micro-opt. The one genuinely worthwhile WinUI change is the **DebugLog level-gate** (M6 — confirmed synchronous locked file I/O on the UI thread in the `[THUMB]`/`[APPLY:N]` hot paths; load-bearing lines identified). Plus a minor Sankey `Unloaded`-teardown leak. Both need the VS UWP build tooling (install pending) to build+verify — M5/M6 held for that, per the user's "verify properly" choice.

## 2026-07-04 (cont.) — macOS static audit + lockstep-doc reconciliation; docs/README/website truth pass

**macOS parity audit** (`shared/docs/MACOS_AUDIT_2026-07.md`, static — Swift can't build on this Windows box, so all code findings are UNVERIFIED-UNTIL-MAC). The audit overturned two planned assumptions (both were wrong): macOS restructure is **not** computed app-side — the engine computes the plan and the app renders it, confidence/reason/tier are used; and the Sankey palette does **not** diverge — macOS also uses Okabe-Ito. Real findings: **F1** (macOS `content_hash` is computed for images only, so Cleanup "Exact" silently misses non-image duplicate PDFs/docs/video/audio — a within-macOS gap vs Windows' all-kinds hashing; left for an on-Mac session as it touches the hashing pipeline); **F4** applied (added the missing CLIP `embedImage` output-dim guard, mirroring the SFace ENG-69 guard — a wrong/substituted model now fails cleanly instead of persisting an off-dim blob); **F5 NOT applied** (verification showed Windows stores the same `"mobileclip_s2"` tag, so changing only macOS would *create* cross-platform divergence). Swift hygiene re-confirmed clean (0 fatalError/try!/as!).

**Lockstep docs reconciled** — the commercial-clean swap (SFace 128-d, RAM++ primary, ViT-B/32) **landed on `main` and is wired as primary** (SFace-only `FaceEmbedderKind`, prewarmed at `FileIDEngineMain.swift:669–676`), but apple/CLAUDE.md, MODELS.md, SHIP.md still said "pending / needs a Mac / main loads prior weights." Corrected all three: the wiring is done; only on-hardware embedding-parity verification remains.

**Docs/README/website truth pass** — fixed wrong repo + Pages URLs in README (`AdamNolle`/`adamnolle` → the real `WebWorldWide`; Pages live at webworldwide.github.io), the false "code-generated DTOs" claim (hand-maintained, held to the schema by conformance suites), and the incomplete layout diagram. Wired root `build.sh -linux` to actually build+run the GTK4 app (was a Phase-5 "not yet supported" stub — verified end-to-end in WSL). De-staled website/index.html and both platforms/linux docs (all six tabs ship, not "Phase 0 scaffold"; phantom `flatpak/` dir fixed).

## 2026-07-04 (cont.) — RTX 5080 perf profiled: pipeline-bound on DirectML, NOT compute-bound; pool is already optimal; the real lever is the CUDA pack

Measured the perf story on the RTX 5080 rather than assuming the "hardcoded to the RTX 2060" constants needed raising for a bigger GPU. **They don't — the measurement inverts the premise:**

- **GPU is idle most of a scan.** `profile_gpu.ps1` over a 1K-image subset: GPU util **p50=19%, mean=27%**, only 6% of samples >80%, power p50=113 W of a ~360 W budget. The RTX 2060 was the opposite (compute-saturated ~87%). So the 5080 is **dispatch-latency / pipeline-bound**, waiting on per-call DirectML dispatch + CPU preprocessing, not on GPU compute.
- **Raising the model pool REGRESSES throughput** (A/B on the subset, env `FILEID_MODEL_POOL_SIZE`, 953 files): pool **4 → 28.9 f/s**, 6 → 23.8, 8 → 20.3, 10 → 21.7 — monotonic decline. More concurrent DirectML sessions add dispatch/allocator contention that the idle GPU can't offset. **So `MODEL_POOL_SIZE = 4` (tuned on the 2060) is empirically optimal on the 5080 too** — no VRAM-tier pool function is warranted; one would slow this machine down. Left the constants unchanged (correct, now for a documented reason on both cards).
- **The real 3-5× lever is the CUDA Performance Pack, which isn't installed** — the engine is on DirectML and logs "~3-5x slower" without `onnxruntime_providers_cuda.dll`. That pack's hosting is the open item in CLAUDE.md; provisioning it is the single biggest perf win and is deferred (needs the pack built + hosted). This is now the #1 perf recommendation.
- **Batching is the one code lever the data supports** (unlike pooling): the GPU is dispatch-bound, so *fewer, larger* RAM++ dispatches could help where *more concurrent* ones hurt — exactly the "high-SM card that doesn't saturate at batch=1" case `ram_plus_batch.rs` predicted. It needs the dynamic-batch-axis ONNX re-export (`export_ram_plus_onnx.py --dynamic-batch`). **The Py3.14 tooling blocker is now GONE** (`onnx 1.22` ships a cp314 wheel — installed + verified), but producing/shipping a new weight requires the MODELS.md vetting + SHA-pin process, so it's a tracked experiment, not this session's change.

Harness retune (shippable now, no engine change): fixed the stale `G:\TrueNAS` → `F:\TrueNAS` corpus default in the five live perf scripts (`profile_gpu`, `measure_batch`, `perf_bench`, `sample_corpus`, `audit_onhw`); reset `iterate.ps1` defaults to measured reality — `-ThroughputTarget 100→25` (~80% of the measured DirectML median; the 100/140 predated RAM++ and false-failed every run) and `-MemoryCapMB 6000→8500` (measured full-library peak was 7913 MB). Historical `G:\` references in dated audit docs left as-is (accurate when written).

## 2026-07-04 — `linux-audit-fixes` verified on the new dev box (RTX 5080 + WSLg) and landed; cross-OS parity proven on one corpus

New dev machine (Ryzen 9 9900X 24T / 31 GB / **RTX 5080 16 GB**; corpus drive is now **`F:\TrueNAS`** — the `G:` in older entries is stale). Environment stood up from zero: .NET 8 SDK, VS Build Tools UWP tooling, Ubuntu 26.04 under WSL2/WSLg (engine/CLI/GTK toolchain + models), stratified 1,000-file perf subset at `C:\fileid-perf-corpus` (seed 20260704, incl. 60 HEIC).

**The branch's four commits verified everywhere the dev box allows, then merged:**
- **Windows headless:** engine clippy `-D warnings` + full tests; CLI + TUI green (one new fix: `sort_by_cached_key` for a clippy-1.96 lint in `tui/src/app.rs`).
- **Linux (WSL, ext4):** engine clippy + 391 tests; GTK app clippy + release build. **Fixed a real staging bug:** `platforms/linux/build/build.sh` looked for the app binary at the pre-workspace path (`src/app/target/`) — staging always failed despite a green build; now points at the workspace `target/`.
- **Full-ML on Linux (static ORT, CPU EP):** 953/953 files, 6,835 RAM++ tags, 386 files with faces @ ~2 f/s. The 61 decode failures ≈ the 60 staged HEICs (graceful-skip path, `heif-dec` not installed — decode-path verify still owed, NEXT 06-30).
- **Six-tab GTK walk under WSLg (screenshots):** Library grid + lazy thumbnails, People (**face clustering ran on Linux**: 954 faces → 442 people with crop thumbnails), Cleanup (10 dupe groups, KEEPER badges), Deep Analyze (model tiers), Restructure (**full Sankey folder map** + recommendations + Apply/Undo), Settings (model manager). Gold palette/LavaLamp/dark theme all read correctly.
- **Windows on-hardware (`iterate.ps1`, RTX 5080, DirectML, default pool):** subset scan 953 files in **33 s ≈ 29 f/s** (2060 ceiling was ~7.9), 950 tagged, 3 failures, 1,011 SFace 128-d prints, 443 persons — **all 12 assertions green** at `-ThroughputTarget 20`. The stock 100/140 f/s target remains stale pending the M8 re-baseline (SHIP.md item). Full `F:\TrueNAS` baseline: see figure below.
- **Cross-OS parity proof:** same subset scanned on Windows (DirectML/GPU) and Linux (static-ORT/CPU) produced matching DB outputs — 953 files, 1,011 face prints, 443 vs 442 person clusters.

**RTX 5080 full-corpus baseline (pre-retune, DirectML, defaults):** full `F:\TrueNAS` (62,731 files) scanned + tagged + face-extracted in **1,548 s ≈ 40.5 files/sec** (~5× the RTX 2060's ~7.9 f/s), 85 decode failures (corrupt JPEGs, graceful), **84,629 face embeddings → 10,208 person clusters**, peak RSS **7.9 GB**. Two harness bugs surfaced and were fixed on this branch, not engine bugs: (1) `iterate.ps1` waited a fixed 5 s after `runFaceClustering` before shutdown — fine for a ~1K-face subset, but 84K faces cluster far longer, so shutdown killed clustering mid-run and persisted 0 persons (A5/A12 false-RED); now waits for the `faceClusteringComplete` event. (2) the scanComplete wait was hardcoded 15 min (message still said "5 min") — now `-ScanTimeoutMinutes`. Open items feeding later milestones: peak 7.9 GB exceeds the 2060-era 6000 MB cap (M8 memory posture); 10,208 persons from 84K faces is high (junk-cluster suppression, à la the macOS 2026-06-21 407→285 tuning — quality pass). Also bumped **quick-xml 0.36→0.41** (RUSTSEC-2026-0194/0195; reworked the OOXML text extractor for 0.41's `GeneralRef` entity split).

## 2026-06-30 (cont.) — Linux GTK app: UI overhaul toward macOS/Windows parity (on-hardware, iterated from live captures)

The GTK4 app (`platforms/linux/`) was a Phase-0 scaffold that "looked cheap / like stock GNOME." Reworked it toward the macOS/Windows reference, verified on real COSMIC hardware. Key wins:
- **Left sidebar navigation** (`adw::OverlaySplitView`) replacing the GNOME top `ViewSwitcher` — gold-tinted active row, **collapsible** (header toggle + `adw::Breakpoint` auto-collapse when narrow, which also fixes small-window resizing; `set_size_request(360,320)` + `GridView` min-columns 1).
- **Global brand stylesheet** (`theme.rs`) — recolors libadwaita's `accent` to gold so every stock widget (buttons, switches, checks, **progress bars** — was Adwaita-blue) brands itself; transparent view bgs so the LavaLamp reads through; glass cards with real depth + padding; **Inter** font with a fallback chain (DE-independent); button/pill/nav transitions.
- **LavaLamp** retuned to the canonical **gold + orange `#FF6600` + dark** recipe on `#141414` (was pastel 4-blob) with a lighter scrim so the warm glow shows.
- **Real app icon** in the dock — the shared brand mark (gold "?" warning-triangle), installed to the hicolor theme + fixed the committed `platforms/linux/data/*.svg` (was a placeholder document icon); `set_default_icon_name` for cross-DE.
- **Preview navigation** — the photo dialog gained `‹`/`›` + "N of M" counter + ←/→ keys (was missing).
- **Thumbnails** now respect EXIF orientation (were shown sideways); **Settings** is a centered `adw::Clamp` column with padded cards + uppercase section headings (was full-width/cramped).
- Added a dev-only headless **self-capture** (`FILEID_SELF_SHOT`) since cosmic-comp exposes no screenshot API — enables headless UI iteration. App is clippy-clean; the required CI gates (engine/CLI/TUI) unchanged since the audit commit.

**All distros:** pure GTK4 + libadwaita, no DE-specific code; Flatpak (planned) bundles the runtime + Inter for identical rendering everywhere.

## 2026-06-30 — Linux audit: full-ML CLI/TUI verified **on real Linux hardware** (first time); ORT static-link fix unblocks ML

First on-hardware Linux run of the engine + `fileid` CLI + `fileid-tui` (branch `linux-audit-fixes`). CI only ever proved they *compile*; this session proved they *run*, end-to-end, against a real corpus on a Pop!_OS box (RTX 2060, but **CPU EP** — see below). A portability audit found 8 issues (1 data-loss, several silently-missing features); all fixed, then verified.

**The headline blocker (was silent): ONNX Runtime didn't load on Linux at all.** The engine's `ort` was `load-dynamic` on all non-Windows targets, but pyke's `download-binaries` ships **only a static `libonnxruntime.a`** for Linux x64 (verified: no dynamic `.so` in the CPU *or* `cu12` set) — so `dlopen("libonnxruntime.so")` had nothing to open and every ML session would fail. Fix: Linux now **statically links the CPU ORT** (`download-binaries` + `std`, no `load-dynamic`; `cuda`/`openvino` dropped). macOS/Windows untouched. `ldd FileIDEngine` → no onnxruntime dep. GPU on Linux is future work (the `cu12` provider needs CUDA 12; the box has 13, and there's no DirectML fallback to size threads against). See DECISIONS.md (2026-06-30).

**Fixes (engine = `platforms/windows/src/engine/src/`):**
- **#1 (data loss):** `commands/bulk.rs` `no_clobber_rename` used `std::fs::rename`, which on POSIX *atomically replaces* the destination — a Linux rename onto an occupied name silently deleted it. Added the `symlink_metadata` pre-existence guard (mirrors `restructure_apply::move_file`). +regression test.
- **#2:** `commands/trash.rs` restore-from-trash was a no-op on Linux. Implemented a freedesktop-trash restore in `shell::trash::restore` (parse `.trashinfo`, move back from `…/Trash/files/`, EXDEV-safe, no-clobber). +round-trip tests.
- **#3:** the ORT static-link fix above.
- **#4:** `models/vlm.rs` / `vlm_server.rs` hardcoded `llama-*-cli.exe` — added `BIN_EXT` gating (parity with `whisper.rs`) so Deep-Analyze can find its runtime on Linux.
- **#5:** `.heic`/`.heif` were undecodable on Linux. Added a best-effort `shell::heic` Linux backend (`heif-dec`/`heif-convert` → temp PNG → `image` decode; graceful skip when absent — no GPL libheif linked) wired into `tagging::decode_image_sync`.
- **#7:** `platform.rs` `file_ref` now returns the inode (`MetadataExt::ino`) on Unix (was `None`) so rename/move heal works without a content rehash.
- **#8 (correctness):** `util/path_safety.rs` `stable_path_hash` lowercased every path → on case-sensitive ext4, `Foo.jpg`/`foo.jpg` collided on the dedup key (UPSERT shadowing). Now case-preserving on Linux; the macOS/Windows parity-pin test is `cfg`-gated off Linux.

**Verification (all green):**
- **Static gates (mirror `linux.yml`, Rust 1.90):** engine clippy `-D warnings` + **391 tests**; CLI clippy + **20 tests**; TUI clippy + **80 tests**. `scripts/build-tools.sh` installs `fileid`/`fileid-tui`/`FileIDEngine` (34M/8.4M/3.5M).
- **CLI/TUI functional (deterministic corpus):** scan/search/info/dedupe + `--json`; the 5-tab `fileid-tui` renders live data and exits clean (PTY-driven).
- **Full-ML on a 208-file Adlon/TrueNAS subset (CPU, 286s, 1.4 s/file):** `scan_assertions.py` → **GREEN**: 204 files, **0 failures**, **1604 RAM++ tags** (accurate: boy/child/dog/poodle/beach/sunset…), **128-d SFace face prints** (512 B — the commercial-clean model, not 2048-B ArcFace), **18 person clusters** (after a direct `runFaceClustering`), CLIP `--similar` (cos 0.82–0.87), exact dedupe (BLAKE3), and `restructure --plan` with EXIF GPS geo-buckets. HEIC graceful-skip confirmed (heif tools not yet installed).

**Pending (need `sudo apt`, user-gated):** GTK4 app build (`libgtk-4-dev libadwaita-1-dev`) and real HEIC decode (`libheif-examples`). The engine/CLI/TUI — the session's target — are done.

## 2026-06-24 (cont.) — audit loop CLOSED on `main`: 6 PRs merged + CI-green; engine bug-hunt found ZERO bugs

Committed/pushed the on-hardware audit fixes as **6 PRs (#73–#78), all merged to `main`, all three CI workflows green** (Windows engine, Linux, Flatpak). The "/loop until perfect" backlog from the entries below is now closed:

- **#73** — WinUI welcome/install sheet showing every launch: `ModelInstallerService.SentinelInstalled` now matches `{id}.installed` OR globs `{id}-*.installed` (hashed sentinels). Runtime-confirmed (log + screenshot: sheet gone when models present).
- **#74** — CLI exit code (`bail` when `result.failed>0`) + `--exact/--similar/--apply` gating + `--json`-decline emits JSON; TUI Windows browse-root (SystemDrive, not `/`) + one fewer `visible_files()` alloc; `PeopleView` logs full `ex` + catches `OperationCanceledException`; Windows backslash-path fixes in CLI test seeds. CLI 20 + TUI 80 tests green.
- **#75** — downloader egress hardening: initial-URL host allowlist (`download_url_allowed`, enforced in `download_simple`/`download_parallel`) + `actual_len == total` size-integrity check.
- **#76** — `main.rs` stdio-loop panic firewall (`catch_unwind` per dispatch → `command_handler_panic` error frame; no silent hang holding the DB writer).
- **#77** — TUI event-loop dirty-flag (redraw only on real events, not ~10×/s idle).
- **#78** — fix-forward: #75's egress unit test embedded literal off-allowlist URLs, which broke the Windows-engine CI **source-URL allowlist scan** (greps source for `https?://<host>`); rebuilt the test URLs via `format!` so no literal URL appears. Root-caused from the CI log, replicated the scan locally, confirmed green. (Gotcha saved to auto-memory.)

**Post-merge follow-ups (this PR):**
- **`models/whisper.rs`** — `transcribe`'s `child.try_wait()?` propagated a rare OS error WITHOUT reaping the child (orphaned whisper-cli), while the timeout path cleaned up. Now all three exit paths (success / timeout / try_wait-error) reap the child + drain the reader symmetrically — matches the `kill_on_drop` convention every other engine child uses. clippy `--all-targets -D warnings` + 413 tests green.
- **`build/iterate.ps1`** — `$MemoryCapMB` 1500→6000. The 1500 MB cap was macOS-MLX-derived; the Windows CUDA stack floor is ~2.4–3.6 GB (RAM++ Swin-L + CLIP + YuNet/SFace + cuDNN), so A4 false-failed every on-hardware run (measured peak 3635 MB @300 files). In-flight decode is concurrency-bounded so RSS plateaus; 6 GB clears steady state with headroom while still tripping a true unbounded leak. (Not CI-gated — `build/` is outside the engine workflow trigger.)

**Deep engine bug-hunt (concurrency / cancellation / lifecycle / panic / locks): ZERO bugs found.** A focused agent + 5 parallel sub-audits read the whole high-risk surface (coordinator, stdio loop, discovery, tagging, dbwriter, sink, scan/wipe, all `models/*`, ipc, db, clustering, restructure, deep_analyze, obj_render, usn, util) and confirmed the engine is well-hardened — every hazard already guarded (panic firewalls, vision/clip semaphores, per-file 60s timeouts, length-gated tensor decodes, bounds + zero-dim guards, `kill_on_drop` children, NaN-safe divides, `try_reserve_exact` + pixel/byte clamps), with **zero production lock-unwraps** (only a test `ENV_LOCK`; `ep_guard` recovers from poison). The only two sub-P2 items it honestly flagged: whisper try_wait (now fixed) and `cluster_suggestions::compose_pair_jpeg`'s large-alloc path — but that module is `pub mod`-declared in `pipeline/mod.rs` and **never referenced anywhere**, so it's dead/unreachable, not a bug.

**Perf — data-driven conclusion:** the ~7.9 f/s scan ceiling is GPU-compute-bound on RAM++ Swin-L (matches the documented ceiling). CPU-side micro-opts (ndarray per-pixel→planar fill) are marginal against a GPU-bound pipeline and risk a silent ML regression with no golden test — **deferred as not worth the risk** until the only real lever, the 384→256 RAM++ re-export (blocked on Py3.14 export tooling), is unblocked. Batching / INT8 / fp16 already verified dead-ends (model is fp16).

## 2026-06-24 — ON-HARDWARE validation (RTX 2060 + real G:\TrueNAS data); CLI+TUI Windows bugs fixed; WinUI app smoke-passed

Ran the dev box's real hardware this session (NOT the usual headless env): **Adlon ext drive at `G:`, RTX 2060 (CUDA EP pack + cudnn installed — engine now auto-pins CUDA, no longer DirectML-only), 129 model files installed.** Backed up the user's real DB (311 files / 180 persons / 623 face_crops → `%LOCALAPPDATA%\FileID-backup-onhwtest`) before any wipe, then validated end-to-end:

- **Engine on real photos (`G:\TrueNAS\iMac Documents`, 300-file cap via `iterate.ps1`):** GREEN except the memory cap. 300 files, **0 failed, 38s ≈ 7.9 files/s** (matches the documented RAM++ Swin-L GPU-bound ceiling — not a regression). A1/A5/A6/A7 green (no crash/hang/fatal/WER), **A11 zero-telemetry green, A12 GREEN** (RAM++ tags + 128-d/512-byte SFace embeddings). Tags genuinely accurate (child/smile/slide/water park/baseball uniform); 607 faces → 125 person clusters. **My 10 engine fixes do not regress real-data scanning.**
- **OCR STA→MTA fix FUNCTIONALLY VERIFIED:** the photo corpus didn't exercise OCR (`should_run_ocr` skips camera-EXIF images by design — that's why `has_text=0` in BOTH my scan and the pre-change backup; OCR was never the regression). Generated 3 text PNGs (no camera EXIF) → scanned → **`has_text=3`, RAM++ tagged "text", 7s, no hang.** So the MTA OCR path produces text and does not deadlock on the blocking pool. ✅
- **CLI (`platforms/cli`):** 3 smoke tests were RED on Windows — root cause was **pre-existing Windows-portability bugs in the TEST seed helpers** (`seed_content_hash`/`seed_phash` used `path_text LIKE '%/{name}'`; engine stores native backslash paths, so they seeded 0 rows). Fixed to `%{name}`. Also fixed a real **`--json` decline bug** (bare `println!` corrupted the JSON stream on confirm-decline in both `dedupe` and `restructure`). **clippy clean + 20 tests green** (was 3 failing).
- **TUI (`platforms/tui`):** `drives_root_resolves_without_panicking` was RED — `drives_candidates()` returned `Vec::new()`→`/`, and `/` is NOT absolute on Windows. Fixed the Windows branch to return the system-drive root (e.g. `C:\`). Real production bug (the TUI's Windows browse-root was drive-relative). **clippy clean + 80 tests green** (was 1 failing).
- **WinUI app smoke (`smoke-screenshot.ps1`, app staged at `%LOCALAPPDATA%\FileID-App`):** **launches + renders the real library** (screenshot: gold/Mica theme, six-tab sidebar, People tab showing real face clusters "Person 3·11", folder `G:\TrueNAS\iMac Documents`). Engine spawned, RTX 2060 probed (5955 MB), CUDA EP pinned, `ReadyEvent` fired, `[APPLY:N]`/`[ENGINE-SUB]` diagnostics intact, no fatal crash. Restored the user's DB afterward.

**New findings (app-side, for NEXT):** (1) welcome/install sheet shows every launch because the sentinel check still keys on `clip`/`arcface` but the installed commercial-clean models are `mobileclip`/`sface` — stale sentinel names after the model swap; (2) `PeopleView.OnLoadedAsync` catch logs only empty `ex.Message` (un-diagnosable — `RefreshAsync` swallows internally, so the escaping throw is an `OnUi` teardown race; log full `ex`); (3) **peak RSS 3635 MB > the 1500 MB cap** (macOS-derived; the Windows ONNX/CUDA stack baseline is ~2.4 GB just for loaded models — likely a cap-calibration gap, pre-existing, needs profiling). **Caveat:** the restored DB's 623 face_crops were wiped by `iterate.ps1` during testing and NOT backed up — face data/clustering is intact in the DB, but face *thumbnails* need a rescan to regenerate.

## 2026-06-24 — Deep bug + perf audit loop (Windows engine): 10 fixes, cargo-green

Self-paced "/loop until perfect" audit. Repo already at `origin/main` (17af6dc) — nothing to pull. Ran 6 correctness agents (Rust pipeline ×2, models+shell, ipc/main/db/util/downloader, C# app, C# theme) + IPC-schema-drift + 3 perf agents (Rust engine, Rust subsystems, C# app+theme). Verified **every** finding against the real code before acting (several high-rated agent findings did **not** survive scrutiny — see below). All fixes self-verified: `cargo clippy --all-targets -D warnings` (default **and** `--features vlm-native`) + 824 tests green. **Not committed/pushed** (working tree only) — and **none of it is on-hardware verified** (no RTX 2060 / WinUI runtime here).

**Fixed + cargo-verified (11 files):**
- **`shell/ocr.rs`** — OCR ran a blocking WinRT `RecognizeAsync(..).get()` on an STA (`COINIT_APARTMENTTHREADED`) tokio blocking-pool thread that pumps no messages — a latent STA-deadlock. Switched to `COINIT_MULTITHREADED`, matching the *documented* invariant `shell::heic`/`shell::video` already use for the identical blocking-`.get()` pattern (OcrEngine/SoftwareBitmap are agile). **Needs on-hardware OCR re-verify.**
- **`pipeline/obj_render.rs`** — `read_text` did unbounded `read_to_string` on every `.obj`/`.mtl` during scan (decoder pool, ≤12 threads) → multi-GB/newline-sparse mesh could OOM/abort. Capped at 64 MB via `Read::take` (sibling `deep_analyze::parse_obj_names` already caps; ASCII so no UTF-8 split).
- **`pipeline/restructure_apply.rs`** — feedback `applied_pairs.push` was nested inside `if let Some(j) = journal` → a forward run whose undo journal failed to open silently disabled learn-from-corrections. Moved out, gated on `record_undo`.
- **`pipeline/deep_analyze.rs`** — `spawn_blocking(metadata_naming_blocking).await.unwrap_or(empty)` swallowed a `JoinError` (panic in whisper/symphonia/obj parse) and recorded the file analyzed-but-empty. Now logs (path-free) and degrades explicitly.
- **`shell/video.rs`** — `propvariant_to_i64` only tried `i64::try_from`; `MF_PD_DURATION` is `VT_UI8`, so duration resolved to 0 → keyframe grabbed from frame 0 instead of 25%. Now tries `u64` first, saturating into i64.
- **`pipeline/restructure_semantic.rs` + `commands/restructure.rs`** — the image/document/non-image passes each used a *fresh* `used_group_names` registry, so two passes could mint the SAME new folder and silently merge unrelated content (the apply layer de-collides file names, not folders). Threaded one shared registry through all three passes (now `pub(crate)` fns taking it; `implicit_hasher`-clean).
- **`models/vlm.rs`** — `cargo clippy --features vlm-native` was broken (6 lint errors) + 2 latent dead-field errors. cfg-gated the not-`vlm-native` imports, dropped an unused `mut`, made the `native` placeholder use explicit imports + `#[allow(unused_async)]` + reference the full runner/request contract. `--all-features` now clippy-clean.
- **`pipeline/dbwriter.rs`** — per faces-evaluated file the writer tx did a `SELECT id` then a `DELETE`; collapsed to one `DELETE ... RETURNING id` (one fewer query under the single-writer lock; single-writer ⇒ identical id set).
- **`ipc/bounded_read.rs`** — `drain_to_newline` (oversized-frame resync) read **one byte per `read_exact`**, unbounded, *outside* the stdio `select!` → a multi-GB newline-free stream could pin the loop and defeat shutdown. Rewrote chunked (`fill_buf`/`consume`, same R4-04 reasoning as `bounded_read_line`) + 64 MB cap; dropped now-unused `AsyncReadExt`.
- **`db/mod.rs`** — `open_writer` orphaned-session sweep and `wipe_all`'s FK-re-enable both swallowed errors with `let _ =`; now log on failure (the FK one matters — a silent failure leaves the shared writer with FK enforcement OFF).

**Verified NOT a bug (high-rated agent findings that didn't survive review):** EngineClient respawn FSM "double-spawn" — `StartAsync` already returns early on `_process is { HasExited: false }` + the `_isStarting` CAS (documented "BUG-3"); no change. IPC schema — 0 drift (both Rust + C# have conformance suites). yunet box decode — matches OpenCV. C# app/theme — no instance of the documented DispatcherObject fast-fail class.

**Deferred with reasons (see NEXT.md 2026-06-24):** ndarray per-pixel preprocessing→linear-fill (GPU-bound ⇒ marginal; byte-imperfect = silent ML regression with no golden test; needs hardware profiling); C# `DebugLog` level-gate (load-bearing forensic infra — CLAUDE.md is emphatic; needs careful scoping + hardware forensics re-verify); downloader hardening (initial-URL host allowlist, `actual==total` size check, per-stream download cap — security/robustness, careful pass); stdio inline-dispatch panic firewall; C# `MergeById` O(n²) + `FileTile` display materialize + `Convert.ToInt32`→`ToInt64`; assorted LOW perf items. **Two stray untracked junk files** in the tree (`platforms/windows/test_vuln.ps1`, and a mangled-name `C：Users…keytest.ps1` in repo root from a botched temp-write) — recommend deletion.

## 2026-06-23 — Quality/perf/dead-code audit (ultracode), round 1: scene_vocab cache removed

Ran a deep performance + dead-code + code-quality + comment/doc audit (`find → 3-skeptic verify → describe-only recipe`, distinct from the correctness bug audit). It hit the **session limit mid-run** (resets 3:10pm Chicago) — verify/recipe agents for cli/linux/swift/windows failed, so coverage landed on the Rust engine + TUI: **56 candidates → 26 confirmed** (8 perf, 8 dead-code, 4 quality, 6 comments-docs). With agents rate-limited, switched to direct compile-verified application.

**Applied + verified (e22d19b):** removed the **dead on-disk scene-matrix cache** in `scene_vocab.rs` — 118 lines (SCENE_CACHE_* consts + 5 cache fns + orphaned ClipTokenizer import, all `#[allow(dead_code)]`), superseded by the `SCENE_EMBEDDINGS` static; fixed the stale doc. clippy -D + 386 tests green. Mechanical scan baseline: 67 `#[allow(dead_code/unused)]` in the engine (many legit cross-platform cfg-gating), CLI/TUI/Linux annotation-clean, only 2 Rust TODOs.

**The other 25 confirmed items are enumerated in NEXT.md** (highlights: TUI dashboards materialize all ≤5000 rows every frame instead of windowing to the viewport [perf-H]; `ClusterAnchor.anchor_embedding` + `uncertain_pairs()`/COS band dead; `restructure_semantic` clones every 512-d SemanticFile per segment; scan_session discovery-notice DRY). Continue the loop — compile-verify each, watchdog the suite — and re-run the audit for cli/linux/swift/windows when capacity returns. Perf-critical scan paths still need on-hardware profiling vs the TrueNAS corpus (don't guess thresholds).

## 2026-06-23 — Whole-codebase bug audit (ultracode): 23 confirmed → all fixed + delta-verified

Ran a multi-agent `find → 3-skeptic default-reject verify → fix-recipe` audit over the entire codebase (16 areas: Rust engine ×8, CLI, TUI, Linux GTK, C# Windows ×2, Swift macOS ×3). 146 agents, **33 candidates → 23 confirmed** (2 high / 9 med / 12 low; ~19 unique after dedupe). Rate limits + a session cap killed the `win-core`/`win-ui` finders and some verify/recipe agents — **the C# Windows app got no coverage this round** (flagged in NEXT). The recipe agents applied fixes directly to the working tree; I read-before-committed each, compile-gated all Rust, and ran a delta re-audit.

Fixed + committed:
- **f45f5a7 (Rust, cargo-verified: engine 386 / CLI 27 / TUI 80, clippy -D clean):** HIGH `trash.rs` Windows multi-file restore silently restored nothing (NUL in env value aborted the PowerShell spawn; the Err was swallowed) → U+001F separator + matched spawn result. MED: CLI `--json --apply` (dedupe+restructure) human lines corrupting JSON; macOS scan/read library mismatch now warns + emits the `--db`; `.obj` parser OOM (unbounded line + mtllib redirection); TUI reload/scan/typed-path now guard `scanning`. LOW: LIKE-wildcard escaping, models `--json` abort, empty `CFFIXED_USER_HOME` relative DB, deep-analyze full-pass done-marking, WAL `checkpoint_truncate` reading the busy row (was `execute_batch` → false Ok), empty `LOCALAPPDATA`, downloader resume progress overshoot. **Also fixed 2 recipe-agent test defects: removed a localhost-server downloader test that deadlocked on `incoming().take(2)`+`join`, and reordered a deep-analyze test that self-deadlocked (`db.lock()` then re-locked via `insert_file`).**
- **dc40735 (Swift/Linux, source-level):** HIGH `CLIPTokenizer` fatal `0..<(-1)` trap on an oversized grapheme → `guard word.count >= 2`. MED CRLF in CLIP merges + BGE vocab; Linux People thumbnail cache keyed by path only → keyed by `(path,bbox)`. LOW Swift `movePersonFaces` stale `representative_face_id`; `FinderTagsEditor` tag-write clobber after navigation; Linux engine respawn counter never reset (died after 5 lifetime crashes).
- **868acda (delta re-audit):** tightened both Swift tokenizers from `.isNewline` to `\n`/`\r\n`-only to byte-match the Rust `.lines()` parity (verified with `swift`).

Delta re-audit (separate adversarial pass) found **no regressions** and **compiled all 4 Swift files clean** (Swift 6.3.2 on this Mac). Linux GTK fixes still need an on-hardware build.

**Follow-up — C# Windows app audit (a6855cb):** ran the rate-limited-out slice as its own focused audit (3 areas × 2 lenses, describe-only recipes). 9 candidates → **7 confirmed** (2 med, 5 low; 6 unique). Applied by read-before-apply (the agent also caught a CS8602 the recipe missed). MED: dead Whisper/BGE Settings Install buttons (`SlotFor` missing cases); Deep-Analyze apply soft-lock (busy-reset outside try/finally). LOW: stuck face-clustering banner, deep-analyze concurrent cross-resolve, restructure apply-guard never released + "ask"-tier opt-in lost on tab re-entry. **NOT compiled** (.NET is CI-only) → needs `dotnet build/test/format` on CI. Commits are local (not pushed).

## 2026-06-21 — ✅ Full-AI scans VERIFIED end-to-end on macOS (CLI + TUI), ONNX Runtime provisioned

Closed the loop on the ONNX-on-macOS work (commit e559bd9). On this Mac (arm64): `brew install onnxruntime` → **1.27.0** at `/opt/homebrew/lib/libonnxruntime.dylib`; `fileid runtime status` → ✓ resolved; `fileid models download arcface mobileclip_s2` → installed. Then:
- **CLI** `fileid scan /tmp/fid_corpus --models --db /tmp/x.sqlite` → engine dlopen'd ONNX Runtime, `mobileclip warmup complete`, `model loaded mobileclip_s2_image.onnx`, **AI scan complete 3/3 · 8 tags · 294 files/s** (exit 0).
- **TUI** PTY-driven (`s`→`t`→type `/private/tmp/fid_corpus`→Enter) → **Scan complete: 6/6 files indexed**, tags, stays responsive.

So both surfaces do full-AI scans on macOS now. Two findings vs. the agent's docs: (1) the sandbox did NOT block dlopen of the Homebrew dylib here; (2) **ONNX Runtime 1.27 is ABI-compatible with `ort 2.0.0-rc.10`** (the `< 1.22` panic is a floor, not a ceiling) — RUNTIME.md updated to record 1.27 verified. The macOS user one-liner: `brew install onnxruntime` (or `fileid runtime install`) once → full AI in TUI + CLI. Remaining polish (not blocking): finalize the HF-mirror SHA pin for a pure-HF-egress `fileid runtime install` (currently relies on a local runtime or `FILEID_ORT_DYLIB_URL`).

## 2026-06-21 — TUI scan PTY-driven + fixed; ONNX-on-macOS chosen (in progress); cross-OS TEST.md

User: "scan just does nothing / locks up — steer it start to stop on TUI and CLI." Drove the TUI in a real PTY (pyte + pty.fork, TIOCSWINSZ, full-frame capture) and ran `FileIDEngine` directly with stderr visible. **Root cause:** the engine is correct — on missing models it emits `phaseChanged:failed` + `error{kind:"models_not_installed", message:"Missing: mobileclip_s2, arcface"}` and exits clean. The **TUI** swallowed it: (1) `missing_models()` checked the macOS desktop-app model dir instead of the engine's `engine_models_dir` (so no early bail, no banner), (2) showed only a misleading "Scan phase: Failed" then reverted to blank, (3) `stderr(Stdio::null())` discarded the real error. Fixed all three in `platforms/tui/src/scan.rs` (commit d8bc809): dir-correct check, suppress bare `PhaseChanged`, TUI-appropriate message, bounded stderr capture; 78 tests. PTY-verified: failure path now shows a persistent "AI models not installed — press D…" (0 "Scan phase: Failed"); CLI metadata scan works (`Indexed: 5 · 2438 files/s`).

**Deeper blocker surfaced:** full-AI scans can't COMPLETE on macOS via the engine — `model_load_failed: libonnxruntime.dylib (no such file)`. The Rust engine uses ONNX Runtime; on macOS no dylib is provisioned (`ort =2.0.0-rc.10` load-dynamic + download-binaries produced none; the engine's `ORT_DYLIB_PATH` pin in `main.rs:121-159` is Windows-only). The macOS desktop app is unaffected (MLX/CoreML). **User chose: provision ONNX Runtime on macOS** (vs. metadata-only). Launched a background agent to add a cfg(macos) dylib resolver + `fileid runtime install` (SHA-pinned MIT ONNX Runtime) + docs; final dlopen verification needs the user's hardware (sandbox blocks running downloaded native code). [in progress]

Also wrote `shared/docs/TEST.md` — cross-OS end-to-end runbook for app/TUI/CLI (safety/isolation rules, fixtures, build matrix, the PTY-drive TUI method, engine direct-diagnosis, per-OS prereqs incl. the macOS runtime, acceptance criteria). Indexed in root CLAUDE.md.

## 2026-06-21 — Model-install progress bar (CLI bar + TUI gauge) + interface polish pass

User: "make a progress bar for installing all the models … and again try to fix up the interfaces more, there is still a lot left to do." (Context: the user had already run `fileid models download --all` in the CLI window from the prior turn — all 9 models / 24.9 GB now installed — so this is the install *experience*, not a blocker.)

**Architecture:** the engine downloader already exposes `install_model_blocking(model, cancel, progress: Arc<dyn Fn(InstallFileProgress)>)` (fields: file_index/file_count/file_name/bytes_done/bytes_total/bytes_per_second, throttled ~20 Hz). The CLI already drove it with a plain text line; the TUI shelled out to the CLI and streamed raw lines. Kept the CLI as the single source of truth for the model catalog and added a machine contract between them:
- **SHARED CONTRACT:** `fileid models download --porcelain-progress` (hidden flag) writes `PROGRESS\t{percent 0-100}\t{label}` lines to stdout (overall, monotonic, byte-weighted by catalog size; label e.g. `arcface · 182/271 MB · 3.4 MB/s · model 2/9`), milestones/summary as plain non-`PROGRESS` stdout lines, final `PROGRESS\t100\tdone`. `--json` wins if both set; fancy stderr bar suppressed in porcelain mode.
- **CLI (`models.rs`):** real carriage-return overall bar to stderr on a TTY (gold fill, name/size/speed/ETA/`model X/N`), `Plainish` (json/quiet) + `NonTty` (piped — milestones only) modes. Unit tests pin the exact porcelain line + monotonic-reaches-100 + size units.
- **TUI (`models.rs`/`app.rs`/`ui.rs`):** spawns `--all --yes --porcelain-progress`, `parse_porcelain_line` → `LoadMsg::DownloadProgress{percent,label}` → `App.download: Option<DownloadState>` → `render_download_gauge` paints a ratatui `Gauge` (gold on dark track) in the banner slot, green "✓ installed — press s to scan" on done, cleared + missing-models re-checked on worker exit. Malformed line → status, never panic.

**Verified with REAL data:** `FILEID_MODELS_DIR=/tmp/probe fileid models download arcface --porcelain-progress` streamed real `PROGRESS` lines with overall percent 50→…→100, final `PROGRESS 100 done`, quiet stderr (throwaway dir; user's installed models untouched). CLI `cargo build`/`clippy -D`/`test` clean; TUI same, **75 tests** (+12: porcelain parse, gauge lifecycle, TestBackend gauge/empty-state frames).

**Interface polish (same pass):**
- CLI: bare-`fileid` first-run tour (what it is + copy-paste Get-started block, names the 2 gate models + model-free fallback); `models list` aligned table with `★` required / `✓ installed` / totals / dir / exact install commands; `scan` + `scan --models` get start line + live CR progress + richer summaries with next-step hints; one-line `Example:` on every subcommand `--help` + top-level `after_help`; honors `$NO_COLOR` + non-TTY.
- TUI: real empty/summary states on every tab (Library "press s to scan" + no-match, People "scan to detect & group faces", Cleanup dedupe explainer, Restructure preview explainer, Settings live model-status line + folder-browser key docs); shared gold-keycap `cta()` so every empty screen answers "how do I do something" (the user's #1 complaint). Redesign palette/banner/dark-bg preserved.

Both release binaries rebuilt + reinstalled (fresh inode + ad-hoc sign). All models installed, so the TUI shows the ready state (no banner); the gauge appears during an actual download.

## 2026-06-21 — TUI interaction fix: welcome-screen masked every tab; `D` "downloaded" nothing

User: "TUI still isn't letting me switch into settings or prompting me to download the models… when I click S to scan nothing happening… ensure the logic from top to bottom is fixed." Audited the full key-dispatch path. Key dispatch was actually fine (`Tab::next/prev` is modular over all 5 tabs, `switch_tab` has no guard, `s` opens the browser overlay). **Real root cause: `ui.rs::render_body` short-circuited to the welcome screen for *every* tab whenever the DB didn't exist yet** — and a fresh empty-scratch start has no DB. So Tab/`1-5` moved the header underline but the body never changed → Settings (and its `D` download prompt) was unreachable. Two more defects: `Char('D')` was gated to the (unreachable) Settings tab; and `models.rs` spawned `fileid models download --all` with null stdin + no `--yes`, so the CLI's `confirm()` returned false on non-TTY → `--all` aborted with **exit 0** while the TUI reported "models installed… ready" (downloaded nothing).

**Fixes (all cargo-verified — `build` ok, `clippy -D warnings` clean, `cargo test` 63 pass):**
- `render_body` welcome screen gated to `Tab::Library` only → Settings + all tabs render on a fresh DB.
- New `render_model_banner`: standing gold/pink one-line banner on *every* tab when `missing_models()` is non-empty (`⚠ AI models not installed — press D to download (~25 GB)…`) / `⟳ Installing…` with progress while downloading.
- `Char('D')` → `request_download()` is now **global** (any tab), not Settings-only.
- `models.rs` spawns `models download --all --yes` so the non-interactive download actually runs.
- `render_status` shows a distinct pink `⚠` on `status_error` (was a green ✓ next to failures).
- `main.rs` event loop now accepts `KeyEventKind` Press **and** Repeat (only ignores Release) — defensive against terminals that report non-Press kinds.
- New tests: Tab-from-last-tab wraps to Library; `D` arms a download from a non-Settings tab.

CLI side ("same for the CLI") was already honest and verified non-interactively: `fileid models list` (marks the 2 scan-gate models), `models download --all --dry-run` (24.9 GB / 9 models), and `scan --models` with no models → clear "AI models not installed → `fileid models download --all`" (graceful exit). Both release binaries rebuilt + reinstalled to `~/.cargo/bin` (fresh inode + ad-hoc codesign to dodge macOS "Killed: 9"). Runtime TUI not drivable in this headless env (alternate-screen teardown wipes the PTY frame); user to verify on Terminal.app.

## 2026-06-21 — face-clustering QUALITY: junk-cluster suppression (407→285 persons on real data, zero merges)

User repeatedly unhappy: 991 faces over-split into **407 person clusters**. Investigated on the REAL DB (worked on a `/tmp` copy incl. the WAL sidecar; real `fileid.sqlite{,-wal,-shm}` md5-proven untouched before+after).

**Data analysis (991 faces / 407 clusters):** 268 clusters (66%) are singletons, 372 (91%) are 1–2 faces; the 12 biggest (≥10) hold 412 faces. Tiny clusters are LOW quality (Apple Vision `faceCaptureQuality` avg 0.15–0.19) vs big clusters (0.33). Multi-face clusters are tight (mean intra-cohesion **0.94**). **Cross-cluster face cosines top out at 0.585** (genuine same-person sits 0.88–0.95) — the 407 are genuinely well-separated, so there are essentially no high-cosine fragments to merge.

**Decision:** centroid/per-pair *consolidation is UNSAFE here* — the only candidate merges sit at 0.55–0.57, dead in the age-progression/family overlap zone the floor forbids; `frac≥0.60`=0 for every pair, so no high-threshold fraction guard merges anything. The safe, high-impact lever is **quality gating**: junk faces form spurious singletons, so don't let them.

**Change (mirrored macOS↔Windows, byte-faithful):** after clustering, a cluster is persisted as a person iff `size ≥ minClusterSize(3)` **OR** `max member quality ≥ soloQualityFloor(0.12)`; else its faces are left unclustered (`person_id NULL`, still candidates — never deleted, never merged). Env-tunable `FILEID_FACE_MIN_CLUSTER_SIZE` / `FILEID_FACE_SOLO_QUALITY` (=0 disables). Eyeball-validated on the crops: <0.12 singletons/doubletons are unrecognizable blur/profile/burst-frames; ≥0.25 singletons are real distinct people (sunglasses/face-paint/one-offs) and are kept; size≥3 protects real low-light recurring people (e.g. a 20-shot cluster).

**Offline result on real embeddings (faithful pipeline order):** floor=0 reproduces 407 exactly (no-op proof); **default 0.12 → 285 persons** (127 junk micro-clusters suppressed, 216 faces unclustered, 775 still clustered), **zero identity merges**. Files: `platforms/apple/.../FaceClustering.swift` (`suppressLowQualityClusters` + Phase-3 wire), `platforms/windows/.../pipeline/face_clustering.rs` (`suppress_low_quality_micro_clusters` + `min_cluster_size`/`solo_quality_floor`) wired in `commands/face_clustering.rs` after `consolidate()`. Tests: 4 Swift (`FaceQualitySuppressionTests`) + 4 Rust, all green. **Verified:** `swift build` + `swift test --filter FaceClustering` green (16 tests/3 suites); `cargo clippy -D warnings` + `cargo test face_clustering` green (22 tests).

## 2026-06-21 — on-hardware test of macOS engine/app + CLI + TUI on real data; RAM++ CRLF bug + EP/CLI fixes

Tested the macOS Swift engine/app and the Rust CLI/TUI against the user's real ~3372-file library (isolated copies; real DB proven untouched). Results:

- macOS: 260/260 swift tests pass; face pipeline healthy (128-d SFace, clustering→persons); restructure BL-01 holds (de-collided folder names, confidence tiers); FTS search + GUI render confirmed.
- **BUG FIXED — RAM++ tagging was silently broken** by a CRLF parse bug (ram_plus_tags.txt is CRLF; Swift treats \r\n as one grapheme so split on "\n" yielded 1 tag → every inference failed the 4585-count guard → silent CLIP-fallback tagging; also why restructure names were noisy). Fixed all 3 parse sites (tags/threshold/suppress) to split on .isNewline + trim .whitespacesAndNewlines. Verified: 4585 tags parse, real RAM++ tags now written.
- **CoreML EP tuning**: A/B-tested on-device — MLProgram 2x faster for SFace (kept), but the 926MB RAM++ Swin is ANE-rejected under MLProgram (234s compile) and CLIP fails to build the plan (both reverted); kept MLComputeUnits=All across all three. SFace/CLIP/RAM++ outputs verified intact.
- **Perf reality**: macOS scan ~1 file/s, inherently bounded by the RAM++ Swin-L forward (~6s/file); even an RTX 2060 does ~6 f/s with RAM++. The ≥140 files/s target predates the RAM++ tagger and is stale.
- CLI/TUI: both build/clippy/test green; CLI reads the Swift-written DB correctly (407 clusters cross-engine), all apply safety-gating works, TUI renders+navigates+exits cleanly (TerminalGuard). Fixed: macOS CLI now auto-finds the Swift app library; dedupe --similar --apply guarded against transitive-chain over-delete; stale docstrings corrected.

## 2026-06-20 (website + docs) — static brand landing page (`website/`) + GitHub Pages workflow; README refreshed for every front-end + packaging

Added the project's public front door and wired its deploy. Docs + web assets only — no app/engine/CLI/GTK source touched.

- **`website/`** — a self-contained static brand landing page (`index.html` + `style.css` + `app.js` + `assets/` brand logo SVG + 256 px PNG; own `README.md`). Dark theme on the signature palette, Open Graph / Twitter-card metadata, **no trackers / analytics / external JS** (telemetry-clean, same bar as the apps). Copy leads with the on-device / no-cloud / no-telemetry / Apache-2.0 positioning across macOS, Windows, Linux, CLI and TUI.
- **`.github/workflows/pages.yml`** — deploys `website/` to GitHub Pages via `actions/configure-pages` + `actions/upload-pages-artifact` + `actions/deploy-pages` on push to `main` (path-filtered to `website/**`), `workflow_dispatch`-able, single non-cancelling `pages` concurrency group. **Owner action still required:** Pages must be enabled (Settings → Pages → Source = "GitHub Actions") for the first publish — tracked in NEXT.
- **README.md refreshed** — now documents all front-ends (macOS · Windows · Linux GTK · `fileid` CLI · `fileid-tui`) and the universal packaging matrix (Flatpak / AppImage / Nix / AUR), replacing the old macOS/Windows-only framing.

## 2026-06-20 (engine Linux parity) — `shell/*` Linux backends (trash · reveal · tags · OCR · video); std+libc+subprocess only, no new crates, CI-green

The engine's `shell/*` file actions gained real **Linux** implementations behind `#[cfg(target_os = "linux")]` (were non-Windows no-op stubs), reaching macOS/Windows parity for the Linux app + CLI. Built with **std + libc + subprocess only — zero new crates** (libc was already in for `getppid` parent-death detection). macOS keeps the graceful `#[cfg(all(not(windows), not(target_os = "linux")))]` stub (its file actions are app-side). Verified: `cargo clippy --all-targets -D warnings` + `cargo test --lib` green on the macOS host (stub + shared arms); the Linux arms compile/clippy/test on ubuntu via `linux.yml`.

- **trash** — pure `std::fs` against the freedesktop Trash spec (`~/.local/share/Trash/{files,info}` + `.trashinfo`), no `gio`.
- **reveal** — DBus `org.freedesktop.FileManager1.ShowItems` via `dbus-send`/`gdbus`, falling back to `xdg-open` on the parent dir; graceful no-op when neither is present.
- **tags** — `user.xdg.tags` xattr via libc `setxattr`/`getxattr`/`listxattr`/`removexattr` (no `xattr-rs`).
- **OCR** — `tesseract` CLI over a temp P6 PPM we write ourselves (so no PNG/JPEG decoder crate); degrades to empty text when tesseract is absent.
- **video keyframe** — `ffmpeg`/`ffprobe` CLI extracting a representative keyframe as P6 PPM; degrades gracefully when absent.
- **Portable restructure move/symlink** is already covered under the Linux-foundation entry below (`std::fs::rename` + EXDEV copy-fallback; `std::os::unix::fs::symlink`).

A follow-up CI fix (`0705cc8`) swapped a percent-encoder helper for manual hex to satisfy the Linux engine clippy. **HEIC has no Linux decoder yet** (image-rs lacks one; libheif is GPL/LGPL — rejected per download-and-run) → tracked in NEXT.

## 2026-06-20 (CLI follow-ons) — `fileid` gains `scan --models`, `dedupe`/`restructure --apply`, `search --similar <file>`; cargo-verified on macOS

Landed the documented CLI follow-ons in `platforms/cli/`, all reusing the engine's exact code/IPC so nothing can drift. **Verified on this macOS host: `cargo clippy --all-targets -- -D warnings` clean, `cargo build` clean, `cargo test` green (2 integration tests, model-free + isolated).**

- **`scan --models`** — full ML pipeline (tags/faces/CLIP/phash). Mirrors the engine's `startScan` model gate in-process (`mobileclip_s2` + `arcface` sentinels via `models::registry::{lookup_full,sentinel_path}`); when present it **spawns the `FileIDEngine` binary** and drives it over the engine's own newline-JSON IPC (`ipc::IpcCommand`/`IpcEvent`, `StartScan` → stream `progress`/`phaseChanged`/`scanComplete`/`error`). Default model-free `scan` unchanged. Engine binary located via `$FILEID_ENGINE_BIN` → beside-exe → dev `target/` → PATH. Missing models / missing binary → clear actionable message. The full pipeline writes the engine's own library (`$XDG_DATA_HOME`/`%LOCALAPPDATA%`); a pinned `--db` is reported not-applicable there.
- **`dedupe --apply [--similar] [--dry-run] [--delete] [--yes]`** — keep one per group, remove the rest via the engine's exact `shell::trash::trash` (Recycle Bin / freedesktop Trash; macOS has no engine trash → instructs `--delete`), or permanent `std::fs::remove_file` with `--delete`; drops the `files` row per the `trashFiles` handler. SAFE: no removal without `--apply`; prompts unless `--yes`; non-interactive stdin without `--yes` aborts. Works only once `content_hash`/`phash` exist (full scan).
- **`restructure --apply [--dry-run] [--symlinks] [--yes] [root]`** — executes the plan via the engine's exact `pipeline::restructure_apply::RestructureApply` (collision-uniquify, stale-plan + path-traversal guards, undo journal; cross-platform MoveFileExW / `std::fs::rename`). Same prompt/dry-run safety.
- **`search --similar <path-or-id>`** — decodes the seed file's `clip_embeddings` BLOB (LE f32) and ranks all others by cosine; distinguishes "no embeddings in library" from "seed not embedded". Replaces the old bool `--similar` "needs models" stub.
- **New dep:** `parking_lot 0.12` (transitive engine dep, now declared direct — zero new lock-graph crates) to name the `Mutex` `RestructureApply::new` wants. **Smoke test extended** to cover `--models` messaging, both `--apply --dry-run` paths (seeding `content_hash` to exercise a real dedupe group), the non-interactive abort, and `--similar` — every assertion checks nothing on disk or in the DB changed. README + DECISIONS updated.

## 2026-06-20 (TUI MVP) — new cross-platform `fileid-tui` terminal UI (ratatui + crossterm), cargo-verified on macOS

Added a fifth client alongside apple/windows/linux/cli: a cross-OS **terminal UI** at `platforms/tui/` — its own standalone Cargo workspace (pinned Rust 1.90), mirroring `platforms/cli`. It links the shared engine as a path dep (`fileid-engine = { path = "../windows/src/engine", default-features = false }`) and **reuses the CLI's exact engine-access patterns**: reads via `fileid_engine::db::open_read`, the restructure preview via the pure `pipeline::restructure::classify`, `discovery::FileKind`, and library-path resolution with byte-identical precedence to the CLI (`--db` → `$FILEID_DB` → `$CFFIXED_USER_HOME` → engine default). Zero contract drift; in-process like the CLI. **Verified on this macOS host: `cargo clippy --all-targets -- -D warnings` clean, `cargo build` clean, `cargo test` green (24 tests).**

- **New deps (justified in DECISIONS.md):** `ratatui 0.29` + `crossterm 0.28` — pure-Rust, no system libraries/C build dep, so CI needs no extra apt packages and the "download-and-run" promise holds. `rusqlite`/`anyhow` are unified from the engine (no new lock-graph crates).
- **Screens (mirror the app tabs at terminal fidelity, signature gold/lavender/cyan/pink palette):** Library (searchable live `files` list + master/detail pane with kind/size/date/flags + tags + text snippet); People (live `persons` clusters); Cleanup (exact-duplicate groups by `content_hash`, master/detail); Restructure (read-only `classify` plan preview — source→dest, category, confidence); Settings (resolved DB path, counts, engine wiring, stubbed-feature notes). Keyboard nav (Tab/Shift-Tab, 1-5, arrows/jk, g/G, `/` search, r reload, ? help, q quit). The bottom **status line is driven by a live `mpsc` event stream** from a background loader thread (`Opening… / Reading files… / Computing plan… / Loaded N files · …`) — the architecture an engine-spawn-IPC feed slots into.
- **Headless tests:** state machine (tab cycle, selection clamp, number-key jump, q/Ctrl-C, reload flag), search filtering + cursor reset, db-path precedence, layout split, date/size formatting, and a **ratatui `TestBackend` full-frame render assertion** proving live data paints (FileID + Library + a filename). e2e non-crash smoke: CLI-scan a temp corpus → drive `fileid-tui --db` through a PTY → clean exit 0.
- **CI:** new `tui` job in `.github/workflows/linux.yml` (now "engine + CLI + TUI + GTK app"; `platforms/tui/**` added to both path filters) — clippy -D warnings + build + test on ubuntu, no extra system deps. Docs: new `platforms/tui/README.md`, DECISIONS entry, NEXT follow-ons.

**Stubbed (noted in-app + README):** engine-spawn command IPC (live `scan`/`cluster` over stdio — the event stream is pre-wired for it); in-app scan/cluster triggers (index with the CLI, reload with `r`); restructure apply (preview is read-only); semantic search / people merge / Deep Analyze (app-side/model features). No engine/Swift/C#/GTK/CLI source was modified.

## 2026-06-20 (Linux packaging) — full GTK GUI (6 tabs) compiles + CI-green; universal Flatpak + AppImage + Nix + AUR landed

The Linux app is now feature-shaped across all **six tabs** (Library · People · Cleanup · Deep Analyze · Restructure · Settings) and the whole stack — engine + CLI + GTK app — **builds and passes CI on ubuntu** (`.github/workflows/linux.yml`, 3 green jobs; GTK app `cargo build` is the required gate). On top of that compiling base, **packaging landed** so the app can reach every distro:

- **Flatpak (primary, universal).** `packaging/flatpak/io.github.fileid.FileID.yaml` — `org.gnome.Platform//46` + `org.gnome.Sdk//46` (GTK 4.14 + libadwaita 1.5, an exact match for the app's `gtk4 0.8` / `adw 0.6` bindings) + the `rust-stable` SDK extension. Builds the GTK app **and** the `FileIDEngine` binary it spawns from the one workspace/lockfile, installs both to `/app/bin`, and reuses `platforms/linux/data/` for the desktop entry, AppStream metainfo, and icon. `finish-args`: `--share=ipc`, `--socket=wayland`, `--socket=fallback-x11`, `--device=dri`, `--filesystem=home`, and `--share=network` **only** for user-initiated HuggingFace model downloads (the sole network egress — no telemetry). One Flatpak covers Debian/Ubuntu/Arch/Gentoo/NixOS/Fedora/openSUSE.
- **AppImage (secondary).** `packaging/appimage/build-appimage.sh` — linuxdeploy + linuxdeploy-plugin-gtk bundling GTK4/libadwaita + both binaries + `libonnxruntime.so`; old-glibc baseline documented (GTK 4.14 floor is the open item). README included.
- **Nix flake.** `packaging/nix/flake.nix` — `rustPlatform.buildRustPackage` with gtk4 + libadwaita + onnxruntime + `wrapGAppsHook4`.
- **AUR.** `packaging/aur/PKGBUILD` — native Arch build; `depends=(gtk4 libadwaita onnxruntime …)`.
- **CI.** New `.github/workflows/packaging.yml` runs `flatpak-builder` against the manifest, **advisory (`continue-on-error: true`)** so the ONNX-in-sandbox part can iterate without red-gating `main` (mirrors how the GTK clippy job is advisory).
- **Data assets created** (were referenced but missing): `platforms/linux/data/io.github.fileid.FileID.metainfo.xml` (AppStream) + `.svg` icon (brand palette). All four channels reuse them — no duplication.

**ONNX-sourcing decision + the honest caveat.** The engine's `ort` crate is locked to `load-dynamic` + `download-binaries` (`cfg(not(windows))`, not editable from packaging). `download-binaries` fetches `libonnxruntime.so` from pyke's CDN **at build time**; `load-dynamic` dlopen's it **at runtime**. Per channel: Flatpak grants `--share=network` to the build step only + stages the `.so` to `/app/lib` + `ORT_DYLIB_PATH`; AppImage bundles the `.so` + `ORT_DYLIB_PATH` hook; Nix/AUR use the system onnxruntime via `ORT_LIB_LOCATION`/`ORT_DYLIB_PATH`. **This is the riskiest part and the reason the Flatpak CI job is advisory** — whether the build downloads vs. uses the staged lib, and the exact `target/` path of the `.so`, need confirming on a real Linux box (none available in this dev env). The packaging files are declarative (no compile here); the GTK app/engine/CLI source was not touched. Docs: new `packaging/README.md` (distro matrix), CONTRIBUTING distro-support section, DECISIONS entry.

## 2026-06-20 (Linux foundation) — engine + CLI + GTK app all build on Linux (CI-green); CLI MVP; engine Linux restructure fallback

The Linux build foundation is now CI-verified end to end (new `.github/workflows/linux.yml`, 3 jobs on ubuntu-latest, all green):
- **Engine (cross-platform Rust) — green.** `cargo fmt`/`clippy -D warnings`/`test` pass on Linux + telemetry scan. The "engine is cross-platform-clean" claim is now concretely proven on an actual Linux host (was only inferred).
- **CLI (`fileid`) — green.** Builds + the model-free smoke test passes on Linux. A new cross-platform front-end (`platforms/cli/`) linking the engine in-process: scan (FTS), search, info, people, dedupe (exact + phash near-dup), restructure --plan; `--json/--quiet`. For headless/NAS/scripting.
- **GTK4 app (`fileid-linux`) — green (compiles).** The Phase-0 scaffold builds once the dependency train was aligned: it pinned `gtk4 0.7` against `libadwaita 0.6`, which transitively needs `gtk4 0.8` (two gtk4 majors both linking native gtk-4 → Cargo conflict). Fixed to the single `gtk4 0.8 / libadwaita 0.6 / glib+gio 0.19` train (GTK 4.14 / libadwaita 1.5 — what ubuntu-24.04/Fedora 40/Arch ship); committed the resolved Cargo.lock.
- **Engine restructure file-move/symlink** now has a portable `#[cfg(not(windows))]` impl (`std::fs::rename` + EXDEV copy-fallback for NAS mounts; `std::os::unix::fs::symlink`) — Restructure apply works on Linux. cargo-verified, 2 new portable tests.

This is the FOUNDATION (everything compiles + the engine/CLI are functional on Linux). Remaining for a feature-complete Linux GUI is tracked in NEXT.md. Note: the GTK app's *behavioral* verification needs an actual Linux box (CI only proves it compiles), same as C# is CI-verified.

## 2026-06-20 (CLI MVP) — new cross-platform `fileid` command-line front-end (in-process over the engine)

Added a fourth client alongside apple/windows/linux: a cross-OS **`fileid` CLI** at `platforms/cli/` (its own standalone Cargo workspace, pinned Rust 1.90 to match the engine). It links the shared engine crate as a path dependency (`fileid-engine = { path = "../windows/src/engine", default-features = false }`) and integrates **in-process** — calling the engine's public library surface directly rather than spawning the engine binary. **Verified on this macOS host: `cargo clippy --all-targets -- -D warnings` clean, `cargo build` clean, `cargo test` green (1 smoke test).**

Architecture choice (in-process vs spawn): the MVP is read/query + plan, and search/info/people/dedupe have **no IPC command** — the macOS/Windows apps run them as direct read-only SQL — so the CLI does the same via `db::open_read`. The engine's `startScan` IPC hard-gates on ML models (`mobileclip_s2`+`arcface`), which is incompatible with a model-free CLI, so `scan` is a model-free FTS indexer writing through `db::open_writer` (engine schema + migrations; `doc_fts` filled by the v15 triggers). `restructure --plan` reuses the engine's public, pure, model-free `pipeline::restructure::classify` rule cascade. Net: zero contract drift, single self-contained binary, no engine-binary dependency.

Commands (each mapped to the engine surface): `scan <path> [--rescan]` (model-free index → `files` + `doc_text`); `search <query> [--similar]` (FTS5 over `doc_fts`+`ocr_fts` + filename fallback; `--similar` returns a clear needs-models notice); `info <path-or-id>` (metadata/tags/people/snippet); `people` (persons + face counts); `dedupe [--exact|--similar]` (content_hash groups / phash Hamming union-find, default threshold 8 mirroring the engine); `restructure --plan [root]` (read-only plan). Global flags `--json`/`--quiet`/`--no-color`/`--db`; library location resolves `--db` → `$FILEID_DB` → `$CFFIXED_USER_HOME` → engine default (`$XDG_DATA_HOME`/`%LOCALAPPDATA%`), so it reads the same DB the desktop apps build. Smoke test is model-free + isolated (`--db` tempdir): creates text/markdown files, scans, asserts search + info + re-scan-skip + plan. No engine/Swift/C#/GTK files were modified. Docs: new `platforms/cli/README.md`, ARCHITECTURE front-ends note, CONTRIBUTING "Build the CLI", DECISIONS entry, NEXT follow-ons (apply commands, semantic-search wiring, full-pipeline `scan --models`, TUI).

## 2026-06-20 (quality audit loop) — perf + accuracy audit across all features; near-duplicate detection feature

Workflow-driven audit campaign: 8 subsystems each FIND→adversarial-default-reject-VERIFY, fix verified safe/high-value findings, re-audit. Shipped (all: swift build clean, 260/260 Swift tests pass, CI green):
- **N+1**: `filesWithPersonTags` 407 per-person queries → 1 JOIN (mirrors Windows `NamedPersonFileIdsAsync`).
- **Tag dedup+cap**: visionTags now case-insensitively deduped + capped at 16 before write (mirrors Windows `tagging.rs`) — fixes "Dog"/"dog" dupes + unbounded tags.
- **Document content search**: macOS now stores extracted doc text into `doc_text` at scan (v15 trigger fills `doc_fts`), so PDFs/docs are keyword-searchable — was a parity regression vs Windows. Verified e2e (4/4 distinctive tokens matched their files; real DB untouched).
- **Representative face by max `face_quality`** (was `.first`) — sharper People cluster thumbnails, mirrors Windows.
- **Person search** adds title/middle_name/suffix (both platforms were missing them).
- **Approximate-dup disclosure**: duplicate groups whose largest member exceeds the 16 MB full-hash cap are matched by a composite fingerprint, not byte-exact SHA — now badged "~ likely match" in Cleanup (Windows already disclosed this).
- **Windows parity**: duplicate keeper selection is now quality-first (aesthetic/size/created/path) instead of alphabetical, mirroring macOS.
- **`FILEID_INFERENCE_CONCURRENCY`** env knob on the 4 model inference semaphores (default 4 kept).
- **NEW FEATURE — perceptual near-duplicate detection** (Cleanup "Visually similar" mode): uses the already-computed-but-unused dhash; union-find on Hamming distance; default threshold 8 (validated on real images: resize/re-encode → Hamming ≤1; nearest distinct photo = 24, a 3× safety margin). No auto-select for delete; "review — not identical" warning. **macOS only so far** (Windows mirror is the remaining lockstep item).
- Restructure proposal-row string precompute (minor perf).

Audit verifiers correctly REJECTED marginal items: CLIP batching (RAM++ Swin-L@384 is the dominant scan cost, not CLIP) and "synchronous applyPlan" (a full scan of 3372 rows is ~10 ms, not the claimed 100 ms).

## 2026-06-20 — fix(macOS): 4 user-reported issues fixed + Windows lockstep confirmed — preview nav, face clustering, restructure sorting, performance

User reported macOS being slow, with broken Restructure sorting, broken face clustering, and totally-broken preview navigation (arrow keys + clicking). All four root-caused and fixed. Verified: `swift build` clean (debug+release), 253/253 Swift tests pass, on-corpus e2e green (CFFIXED_USER_HOME-isolated copy of the user's library), Windows confirmed already in lockstep.

**Preview navigation (LibraryView.swift) — rebuilt.** Root cause: `.sheet(item: $selected)` swapped the sheet's bound identity on every nav so content didn't update in place; no `@FocusState` so `.onKeyPress` arrows never received key focus; and `openPreview` dropped the full-library sibling upgrade after the first nav (the `selected?.id == seedID` gate). Re-architected to a single stable `.sheet(isPresented:)` whose displayed file is driven by `previewSelectedID: Int64?`; `step()` mutates the id in place; added a `@FocusState` focus-grab + a tag-field typing guard (mirrors Windows `focused is TextBox`); fixed the sibling upgrade to track by id; nav buttons disable-don't-hide. Mirrors Windows SetSiblings/HandleKeyDown.

**Face clustering (ArcFaceService.swift, FaceClustering.swift).** On-hardware diagnosis DISPROVED the suspected CoreML-EP failure — the pipeline is healthy (CoreML binds, ORT auto-appends CPU, 991/991 embed, 407 persons). The user's all-NULL embeddings / 0 persons came from clustering never *completing* in their running app session (stale bundled engine, or quit before the post-scan auto-trigger finished), not a code defect. Hardening still landed: CoreML→CPU EP fallback + `FILEID_FACE_EP` env (mirrors Windows runtime.rs; helps CoreML-less Macs), the discarded `load()` bool is now captured and surfaced as `face_cluster_embedder_load_failed` (instead of a wrong "install the model" prompt or silent NULL), plus `arcface_preprocess_failed` logging. Post-scan auto-enqueue confirmed correct as-is. The real DB was additively backfilled to 407 persons during diagnosis (adds embeddings+persons only; nothing destroyed).

**Restructure (RestructureSemantic.swift + SankeyFlowView.swift + RestructureView.swift).** Mirrored the Windows BL-01 fix: `usedGroupNames` is now threaded across all time-gap segments (was fresh per segment → two different events could mint the same folder name and merge). Added count-desc/name-asc sort tie-breakers to Sankey sources/destinations + proposal grouping (determinism; matches Windows). On-corpus: 20 folders / 20 distinct leaf names (BL-01 active; disambiguation suffixes present), sensible grouping.

**Performance (PeopleView.swift, DeepAnalyzeViews.swift, ReadStore.swift, ThumbnailService.swift).** macOS was catching up to Windows. Throttled the People/DeepAnalyze `store.version` reload storms (1000+ DB queries/scan → ≤1/s; copied LibraryView's leading-edge + trailing-debounce). Replaced the N+1 correlated face-count subquery in `persons()` with a `GROUP BY` join (indexes already present). Thumbnail cache: in-memory 800→4000 (+512 MB cost limit), added an on-disk SHA256(path|mtime|size|px) JPEG cache + a 6-way decode concurrency gate (NAS-friendly). Windows audit confirmed it already had all three (10 Hz throttle, GROUP BY, L1+L2 thumbnail cache) — macOS now matches.

**Threshold decision:** kept macOS face auto-merge at 0.65/0.55 (NOT Windows' 0.75) — an on-corpus sweep showed 0.75 zeroes the auto-merge polish and increases fragmentation (34→39 persons, all 5 verified-correct merges lost). See DECISIONS.md.

**Lockstep:** RAM++ tagger + BGE doc embeddings are already implemented on macOS (byte-faithful to Windows). BGE model isn't installed in the user's library (install via Settings → rescan to cluster the 1737 docs by content); RAM++ is installed + wired.

**Follow-ups (same day):** preview-nav focus hardened — the sheet now grabs key focus after a brief defer (`.task { try? await Task.sleep(.milliseconds(50)); keyFocus = true }`) so arrows work without a click. Face-cluster fragmentation investigated on the real embeddings: it is intrinsic to age progression (same child across years overlaps the different-people cosine band), so auto-merge thresholds stay at 0.65/0.55 and consolidation is left to the already-wired "Suggest merges" UI (which surfaces the 0.50–0.64 candidates) — see DECISIONS.md.

## 2026-06-19 (parity audit) — fix(parity): macOS/Windows UI parity audit — 8 bugs fixed: statusBanner error icon, dismissedDeepAnalyzeHint persistence, FaceEmbedderCard copy, dead lastSeenVersion, FaceClusteringInFlight banner (Windows), LastScanProcessedFiles PropertyChanged, ApplyBarHint text, FaceClusteringBanner wiring

Full six-tab parity audit via parallel agents (macOS inventory + Windows inventory). Verified: Swift build clean, 253/253 Swift tests pass, Rust clippy -D warnings clean.

**macOS (RestructureView.swift + LibraryView.swift + SettingsView.swift):**
- **statusBanner always showed gold checkmark** even for error messages ("Engine unavailable", "Couldn't compute a plan", etc.). Fixed: added `isError: Bool = false` parameter + `@State private var statusIsError`; error sites set `statusIsError = true`, success sites set `false`; banner shows orange `exclamationmark.triangle.fill` for errors.
- **`dismissedDeepAnalyzeHint` not persisted**: `@State` var reset on every tab navigation, so the "Run Deep Analyze" hint banner couldn't be permanently dismissed. Fixed: changed to `@AppStorage("restructure.dismissedDeepAnalyzeHint")`.
- **FaceEmbedderCard copy outdated**: description said "Pre-converted from Buffalo (Immich) ONNX" — the app now uses SFace (Apache-2.0). Fixed: updated to accurate SFace description.
- **Dead `lastSeenVersion` state** (LibraryView): `@State private var lastSeenVersion: Int = -1` declared but never read or written. Removed.

**Windows (EngineClient.cs + EngineClient.Commands.cs + LibraryView.xaml.cs + RestructureView.xaml.cs):**
- **`LastScanProcessedFiles` missing PropertyChanged**: auto-property with `private set` bypassed the `Set(ref ...)` helper, so any XAML binding to this property would never update after scan completion. Fixed: added backing field + `Set(ref _lastScanProcessedFiles, value)`.
- **FaceClusteringBanner always Collapsed**: banner existed in XAML but `SyncBanners()` hardcoded `Visibility.Collapsed` unconditionally. Fixed: added observable `FaceClusteringInFlight` bool property to EngineClient (backed by `_faceClusteringInFlight`, set true at both `AutoTriggerFaceClusteringAsync` call sites on UI thread, cleared on `FaceClusteringCompleteEvent` and `face_clustering_failed`, dispatched via `_ui.TryEnqueue` from catch block); `SyncBanners` now checks it. Mirrors macOS `faceClusteringInFlight`.
- **ApplyBarHint wrong text for real moves**: hint always said "Originals stay put - applying creates shortcuts you can review" — inaccurate when user applies real moves (originals move). Fixed: "Shortcuts leave originals in place · Moves are permanent but undoable."

## 2026-06-19 (latest) — fix(quality): 11 bugs fixed across macOS+Windows — cross-segment name collision, zero-timestamp guard, audio year folders, IPCSink false-positive pinning, transcribeAudio timeout, scan queue guard, planRestructure shutdown race, resolveTargets data corruption, audio year tag format

Two parallel code-review audits (macOS engine + Windows engine) surfaced 11 bugs/quality issues. All fixed and verified: Rust clippy -D warnings clean, 370 Rust tests pass, 253/253 Swift tests pass, 11/11 iterate.sh assertions GREEN.

**Windows engine (restructure_semantic.rs + restructure.rs + audio_meta.rs):**
- **BL-01 CRITICAL**: `semantic_classify` — cross-segment name collision. After time-gap segmentation, each segment's `semantic_classify_profiled` call had its own local `used_group_names` HashSet that was discarded after the call, so two separate beach-trip segments could both produce a `Beach/` folder, merging photos from different events. Fixed: `used_group_names` is now passed as `&mut HashSet<String>` across all segment calls.
- **BL-02**: Zero/near-zero timestamp (FAT32 zero-epoch, corrupt mtime) silently placed files in `Photos/1970/January/` with Review confidence. Fixed: `ts_valid = ts > 86400` guard — invalid timestamps produce Ask confidence and flat (dateless) parent folders (`Photos/` not `Photos/1970/`). Applied to image, video, doc, and audio branches.
- **BL-03**: Audio files routed to flat `Audio/` with no year structure even when a valid timestamp was present. Fixed: now routes to `Audio/<Year>/` (mirrors Video → `Videos/<Year>/`). Flat `Audio/` used only when ts_valid is false.
- **WR-02**: Audio date tags emitted raw year strings (`"2019"`) instead of the `Year_NNN` format used by all other file kinds, producing double tags (`"2019"` + `"Year_2019"`) in the Library chip row. Fixed: `audio_meta.rs` now emits `"Year_2019"` format.

**macOS engine (FileIDEngineMain.swift + IPCSink.swift + DeepAnalyzeNaming.swift + DeepAnalyzeRunner.swift + Restructure.swift):**
- **CR-01 CRITICAL**: `transcribeAudio` had no timeout. SFSpeechRecognizer never delivers `isFinal` for silence-only files, unsupported codecs, or very short recordings — the `withCheckedContinuation` parked forever, stalling all subsequent Deep Analyze work. Fixed: wrapped in `withTaskGroup` race against 30-second timeout.
- **CR-02**: `resolveTargets` in DeepAnalyzeRunner coalesced nil DB columns with `?? 0` / `?? ""` defaults, producing `Target(id: 0, path: "")`. The id=0 UPDATE silently matched zero rows; the empty path resolved to the process working directory as a "file to analyze". Fixed: `compactMap` with explicit `guard let rowID: Int64, rowID > 0, !path.isEmpty` — invalid rows are dropped, not faked.
- **CR-03**: `startScan` enqueued jobs unconditionally. A rapidly-clicking or misbehaving app could pile up unlimited scan jobs. Fixed: rejects with `scan_already_queued` error if `JobQueue.shared.hasActive(category: .scan)` is true.
- **WR-01**: `planRestructure` spawned an unregistered `Task.detached`. A `.shutdown` arriving during plan generation would call `_exit(0)` before the plan task emitted `restructurePlan`, leaving the Restructure tab stuck on "Computing plan…". Fixed: task is now registered via `coordinator.setActiveRestructure` so `awaitActiveRestructure()` in `main()` drains it before exit.
- **WR-02 IPCSink**: `criticalNeedles` used bare strings like `"error"` — matched ANY JSON string value containing that word (e.g. a tag named "ready", a path component "scanComplete", a caption word "error"). Fixed: needles now use `"error":{` form to match JSON *object keys* only, not string values.
- **macOS rule cascade (Restructure.swift)**: Zero-timestamp guard and audio year structure applied identically to Windows (BL-02 + BL-03 mirror).

**Tests added:**
- Rust: `audio_year_subfolder` — asserts dated audio goes to `Audio/2024/`, not flat `Audio/`
- Rust: `zero_timestamp_gets_ask_confidence` — asserts zero-ts image gets Ask confidence and no 1970 folder
- Swift: Updated `videoAudioBuckets` and `missingTimestampYear` tests to match new behavior

## 2026-06-19 — feat(restructure): time-gap segmentation + richer time encoding + ask-deselection + CVD Sankey palette

Deep investigation into why restructure underperforms. Two-agent audit identified root causes; 8 improvements implemented across both engines and the macOS UI. All verified: Rust clippy -D warnings clean, 253/253 Swift tests pass.

**Root causes diagnosed:**
1. No time-gap event segmentation — photos from different days competed in the same cluster
2. Time encoding only had day-of-year (2 floats); same-day events in different years were indistinguishable
3. `ask`-confidence moves started selected — users could accidentally apply low-confidence moves
4. Sankey ribbons were all gold regardless of destination — no visual distinction between categories
5. `pass2Margin` not getting the granularity delta — asymmetry at `tight`/`loose` granularity settings
6. Face, GPS, path signals not fused (documented gap; tracked for future work)

**Changes (both engines — byte-faithful lockstep):**
- **Time-gap event segmentation**: `classify()` (macOS) / `semantic_classify()` (Rust) now pre-segment photos by capture-time gap (default 2 h, `FILEID_RESTRUCTURE_TIME_GAP` env override) before clustering. Events separated by hours/days never compete in the same cluster. Photos without timestamps cluster independently as a trailing group.
- **Richer time encoding**: `dayOfYearCyclical` → `timeFeatures` returning 5 values: day-of-year sin/cos (seasonality), time-of-day sin/cos (morning vs evening), log-compressed absolute year (separates same-calendar-day events across years). Fused-vector capacity updated from +2 to +5.
- **`pass2Margin` gets granularity delta** (`0.08 + d * 0.5`): margin now scales proportionally when `FILEID_RESTRUCTURE_GRANULARITY=tight/loose`, eliminating the asymmetry where cosines shifted but margin didn't.
- `#[derive(Clone)]` added to `SemanticFile` (Rust) to enable per-segment cloning.

**macOS UI:**
- **`.ask` proposals start deselected** (`RestructureView.swift`): "No clear signal — the decision is yours" moves are unchecked by default; user must explicitly select them before applying.
- **Sankey Okabe-Ito CVD-safe palette**: destination nodes now each get a distinct color (blue, vermilion, green, sky blue, orange, purple, yellow); ribbons carry the destination color so the user sees "what does this become?" from the hue. Source nodes are neutral (`.secondary`) so destinations dominate. Ribbon palette is CVD-safe for all common deficiency types.

**Windows UI:**
- **`.ask` proposals start deselected** (`RestructureView.xaml.cs`): `ask`-confidence rows deselected in a suppressed pass alongside the existing `_deselectedFileIds` restoration.

**Still tracked / deferred:**
- Face identity signal in fusion vector (requires person→file DB join + multi-hot block)
- GPS signal in fusion vector (reverse geocoding or coordinate normalization)
- VLM group naming (Qwen2.5-VL label-then-reason; per-call model reload too slow)
- `staysPutFiles` in IPC (IPC schema change needed)
- Win2D Sankey color upgrade (Windows uses a different Sankey rendering path)

---

## 2026-06-19 — fix: 3 final bugs (macOS: WelcomeSheet VLM error hidden, SettingsView blocking DB read; Windows: SettingsView SqliteConnection leak)

3 final bugs fixed. All verified: Rust clippy -D warnings clean, 253 Swift tests pass.

- **WelcomeSheet.swift** (MEDIUM): VLM install error was silently dropped. `vlmProgressLabel` returns "Failed: …" when `vlmLastError` is set, but that label was only rendered inside `if inProgress { }`. When an error fires, `vlmRequested` becomes false → `vlmInProgress = false` → the block is never entered. Added `else if let label = progressLabel { Text(label).foregroundStyle(.red) }` so errors surface below the title row.
- **SettingsView.swift** (LOW): `store.recentSessions()` called synchronously on the main actor in `.onAppear` and `.onChange(of: showAdvanced)`. GRDB's `DatabaseQueue.read` blocks the calling thread. Wrapped in `Task { }` so it runs cooperatively without stalling the run loop.
- **SettingsView.xaml.cs** (HIGH): `PopulateRecentScansAsync` opened `SqliteConnection` without `using` — leaked one OS read handle per Settings tab visit. Changed `var conn` to `using var conn`.

## 2026-06-19 — fix: 7 bugs (macOS: preview sibling race, DA error stale progress, DA url force-unwrap; Windows: SqliteConnection leak, ReadStore missing OpenAsync, _deselectedFileIds race, restructure_semantic empty filename)

7 additional bugs fixed. All verified: Rust clippy -D warnings clean, 253 Swift tests pass.

- **LibraryView.swift** (MEDIUM): `openPreview` race — async task that upgrades `previewSiblings` only gated on `selected != nil`; if user arrowed to a different photo before the task completed, the original photo's sibling list replaced the new photo's. Fixed: capture `row.id` at spawn time, gate on `selected?.id == seedID`.
- **EngineClient.swift** (MEDIUM): `deepAnalyzeProgress` not cleared on `deep*` engine errors. `deepAnalyzeInFlight` was cleared but `deepAnalyzeProgress` was not, leaving a frozen "Working…" progress card with no way to dismiss it. Fixed: added `deepAnalyzeProgress = nil` alongside `deepAnalyzeInFlight = false`.
- **DeepAnalyzeViews.swift** (LOW): `ModelInstallStatus.isInstalled` force-unwrapped `.urls(...).first!` — crashes if sandbox disallows the document directory. Changed to `guard let base = ... else { return false }`.
- **DeepAnalyzeView.xaml.cs** (HIGH): `RefreshNamePeopleGateAsync` opened a `SqliteConnection` without `using` — leaked a read handle on every call (tab load + face-cluster-complete events). Changed to `using var conn`.
- **DeepAnalyzeView.xaml.cs** (MEDIUM): `RunApplyAsync` used `ReadStore` without calling `OpenAsync()` before the first query. All three Apply buttons (Apply Tags, Apply People, Apply All) threw `NullReferenceException` inside the store and silently failed, showing 0 tagged/peopled. Added `await store.OpenAsync()`.
- **RestructureView.xaml.cs** (MEDIUM): `DeepAnalyzeComplete` handler called `_deselectedFileIds.Clear()` (a static field shared across view instances) after an `await` without re-checking `_unloaded` — could clobber the new view instance's selection state if the view was recreated during the await. Added `if (_unloaded) return;` after `await RefreshDeepAnalyzeHintAsync()`.
- **restructure_semantic.rs** (LOW): `file.source.file_name().unwrap_or_default()` produced an empty `OsStr` for paths ending in `/` or `..`, causing `dest_dir.join("")` to silently resolve to the directory itself. Changed to `let Some(name) = file.source.file_name() else { continue }`.

## 2026-06-19 — fix: 4 more bugs (macOS: face-name inheritance threshold, restructure feedback skip; Windows: RAM++ 0-tag wipes prior tags, tagging visual_tagger_ran scope)

4 additional bugs fixed. All verified: Rust clippy -D warnings clean, 253 Swift tests pass.

- **FaceClustering.swift L1034** (HIGH): Wave-1 name-inheritance threshold used floor division — `3 / 2 = 1` face could claim a 3-face prior cluster (33% overlap satisfying the "≥ 50%" comment). Fixed to ceiling: `(n + 1) / 2` → requires 2 of 3 faces.
- **Restructure.swift L771-778** (LOW): `appliedPairs` (learn-from-corrections feedback) was gated inside `if let h = undoHandle`, so moves were silently uncredited when the undo journal failed to open (disk-full / sandbox). Decoupled: always collect pairs when `recordUndo=true`, write to journal separately.
- **tagging.rs L1960-1966** (MEDIUM): `visual_tagger_ran = models.ram_plus.is_some()` — true whenever RAM++ model is loaded, even when it ran and emitted 0 tags. `tags_evaluated=true` then caused dbwriter to wipe previously-stored content tags on re-scan of abstract/solid-color images. Fixed: hoisted `ram_plus_ran` (set to `ram_emit_count > 0`) before the image block; `visual_tagger_ran` now requires RAM++ to have emitted at least one tag, or CLIP scene tags to be enabled with an embedding.
- **tagging.rs L1719** (bookkeeping): `let mut ram_plus_ran` was declared inside the `if let Some((rgb, w, h)) = image_source` block, making it inaccessible at the `visual_tagger_ran` site. Hoisted to function scope.

## 2026-06-19 — fix: 6 bugs (macOS: unknown-person survivor, displayName double-space, nonisolated statics; Windows: _lastAnyProgressAt race, Whisper/Bge subscriptions, thumbnail drain block)

6 additional bugs fixed. All verified: Rust clippy -D warnings clean, 253 Swift tests pass.

- **ReadStore.swift** (HIGH): `mergePersonsBatch` set `isNamed=true` for `is_unknown` persons, causing Unknown clusters to beat genuinely unnamed persons in survivor selection and swallow their faces. Fixed to `false`.
- **ReadStore.swift** (MEDIUM): `displayName` with suffix produced `"John Smith  Jr"` (double-space). `", Jr".replacingOccurrences(", ", " ")` yields `" Jr"` (leading space) before join. Simplified to `parts.append(s)`.
- **DeepAnalyze.swift** (MEDIUM): `compareCallsSinceClear` and `compareSampleLogged` declared `nonisolated(unsafe) private static`, opting out of actor isolation though only used inside the actor method `compareFaces`. Promoted to actor instance variables.
- **ModelInstallerService.cs** (MEDIUM): `_lastAnyProgressAt` was a plain `static DateTime` field written from PropertyChanged callback thread and read from thread-pool watchdog without synchronization. `volatile` is illegal on struct fields in C#; changed to `long _lastAnyProgressAtTicks` with `Interlocked.Exchange`/`Read`.
- **ModelInstallerService.cs** (LOW): `Whisper` and `Bge` slots not subscribed to `OnSlotPropertyChanged`. Added missing subscriptions to prevent perpetual stale aggregates if either slot is added to `IsBusy`/`CoreModelsInstalled` in the future.
- **ThumbnailService.cs** (MEDIUM): `Thread.Sleep(50)` in `TryEnqueueWithRetry` blocked the single drain worker for 50ms on every compositor-shutdown `TryEnqueue` failure, stalling all queued thumbnail requests. Removed; immediate retry is sufficient.

## 2026-06-19 — fix: 10 more bugs (Windows: mobileclip embedding corruption, face crop leak, downloader progress; macOS: mergeTags duplicate, mergePersons null, undo identity skip)

10 additional bugs fixed. All verified: Rust clippy -D warnings clean, 253 Swift tests pass.

1. **mobileclip.rs (HIGH) — integer division truncates last embedding in batch**:
   `embed_dim = total / batch` silently discards a remainder if ORT output isn't cleanly divisible.
   The last embedding in any batch gets the wrong dimension, corrupting cosine similarity in semantic
   search with no error emitted. Fix: bail if `total % batch != 0`.
   File: `platforms/windows/src/engine/src/models/mobileclip.rs`

2. **dbwriter.rs (HIGH) — `.filter_map(ok)` silently drops row errors → stale face crop files never pruned**:
   `stale_face_ids` was collected with `.filter_map(|r| r.ok())`, silently dropping any row error.
   The face DELETE still ran, but the silently-dropped IDs were never added to `crop_ids_to_prune`,
   leaving `face_crops/<id>.jpg` files on disk forever. Fix: `.collect::<rusqlite::Result<Vec<_>>>()?`
   so any row error surfaces and aborts the transaction rather than leaking files.
   File: `platforms/windows/src/engine/src/pipeline/dbwriter.rs`

3. **identity_clustering.rs (MEDIUM) — `dim == 0` returns contradictory cluster_ids/cluster_count**:
   Returned `cluster_ids: vec![0; n]` (all faces in cluster 0) with `cluster_count: 0`. Callers
   that iterate `0..cluster_count` create zero People entries, orphaning all n faces from the tab.
   Fix: return `cluster_ids: (0..n).collect()`, `cluster_count: n` (each face its own singleton).
   File: `platforms/windows/src/engine/src/pipeline/identity_clustering.rs`

4. **downloader.rs (MEDIUM) — final 100% progress event never emitted in `download_parallel`**:
   The 10 Hz throttle could suppress the last chunk's progress; a stale no-op block at the end
   silenced the "silence unused variable" intent. The progress bar stalled at ~99% permanently.
   Fix: emit unconditional final event inside the drainer when `rx.recv()` returns `None`.
   File: `platforms/windows/src/engine/src/downloader.rs`

5. **scan.rs (LOW) — panic in `ScanSession::new_with_options` leaves scan_state permanently set**:
   If the scan task panicked before the `*scan_state_release.lock() = None` cleanup, the slot
   stayed occupied and every subsequent `startScan` returned `scan_already_running` until restart.
   Fix: RAII `ScanStateGuard` struct whose `Drop` clears the slot on any unwind.
   File: `platforms/windows/src/engine/src/commands/scan.rs`

6. **TagWriter.swift (MEDIUM) — `mergeTags` allows case-variant duplicates from the `new` array**:
   `lowerExisting` was built once from `existing` and not updated as tags were appended. Two tags
   in `new` like `["vacation", "Vacation"]` both passed the guard and were written as distinct
   Finder tags. Fix: promote `lowerExisting` → `lowerSeen: var Set` and update on each insertion.
   File: `platforms/apple/shared/Sources/FileIDShared/TagWriter.swift`

7. **TagWriter.swift (LOW) — `undoBulkAdd` strips tags without identity verification for nil-identity journal entries**:
   The size/mtime guard was entered only if BOTH were non-nil; entries from older builds (nil fields)
   bypassed it entirely and could mangle an unrelated replacement file. Fix: `guard let` skips the
   undo for any entry with missing identity data (safer than mangling a different file).
   File: `platforms/apple/shared/Sources/FileIDShared/TagWriter.swift`

8. **Database.swift (MEDIUM) — `mergePersons` representative_face_id stays NULL during initial re-scan**:
   The `COALESCE(SELECT ... WHERE arcface_embedding IS NOT NULL, representative_face_id)` always
   evaluates to `representative_face_id` (NULL) while the v12 reset is in progress and no embeddings
   exist yet. Any merge during the initial re-scan leaves the target person with no representative
   face — the People UI shows no crop until the next re-cluster. Fix: add a second fallback
   sub-select (any face_print, ignoring embedding) before falling back to the existing value.
   File: `platforms/apple/engine/Sources/FileIDEngine/Storage/Database.swift`

9. **Database.swift (LOW) — `Int($0)` truncation in `mergePersons` args**:
   `validSources: [Int64]` were cast to `Int` before being appended to the GRDB args array.
   Safe on 64-bit macOS (same width) but fragile. Fix: pass `validSources` directly.
   File: `platforms/apple/engine/Sources/FileIDEngine/Storage/Database.swift`

10. **CleanupViewModel.cs (LOW) — dead null guard on content_hash read**:
    `(byte[])reader[3]` throws `InvalidCastException` for DBNull (the null guard can never catch it).
    The WHERE clause already filters, but the guard masked a future regression risk.
    Fix: `if (reader.IsDBNull(3)) continue;` before the cast.
    File: `platforms/windows/src/FileID.App/ViewModels/CleanupViewModel.cs`

## 2026-06-19 — fix: 6 bugs across macOS + Windows (trash DB deadlock, person ID corruption, actor blocking, data race)

Six bugs fixed and verified (253 Swift tests + Rust clippy -D warnings clean):

1. **trash.rs T-1 (HIGH) — DB mutex held during PowerShell restore (30 s stall)**:
   `handle_restore_from_trash` acquired `db.lock()` at the top and held it through
   `restore_batch_from_recycle_bin`, which shells out to PowerShell (up to 30 s). Every DB reader
   (including the UI's `ReadStore`) was blocked for the full duration of any undo operation.
   Fix: compute filesystem-only `pre_occupied` before the lock; read `allowed_canonical` in a
   short-lived block (lock drops immediately after); PowerShell runs with no lock held; re-acquire
   for the post-restore transaction. File: `platforms/windows/src/engine/src/commands/trash.rs`

2. **trash.rs T-2 (HIGH) — wrong person ID on revert-merge when `source_person_id` was recycled**:
   `INSERT OR IGNORE INTO persons (id=source_person_id)` silently no-ops when that id is already
   held by a *different* person (SQLite auto-incremented past it and reused the value). The
   subsequent `SELECT id FROM persons WHERE id = source_person_id` then returns the occupant,
   and all reverted face-prints land on the wrong person. Fix: check `execute()` rows-changed
   (0 = conflict); on conflict, do a plain `INSERT INTO persons` (no id) and use `last_insert_rowid()`.
   File: `platforms/windows/src/engine/src/commands/trash.rs`

3. **trash.rs T-3 (MEDIUM) — `let _ = tx.execute(...)` silences real DB errors after restore**:
   After a successful on-disk restore, `INSERT OR IGNORE INTO files` used `let _ =`, discarding
   errors from disk-full / corruption / schema mismatch — the file exists on disk but never
   appears in the Library. Fix: `tx.execute(...)?` — `OR IGNORE` already returns `Ok(0)` for
   constraint conflicts, so `?` only fires on genuine failures.
   File: `platforms/windows/src/engine/src/commands/trash.rs`

4. **restructure.rs R-1 (MEDIUM) — silent signals load failure**:
   The `spawn_blocking` signals load matched only `Ok(Ok(v))` + a catch-all `_`; a real
   `Ok(Err(anyhow_error))` or a panic `Err(JoinError)` both silently fell through to empty maps
   with no log line, making restructure silently degrade instead of reporting the cause.
   Fix: explicit `Ok(Err(err))` and `Err(err)` arms with `tracing::warn!`.
   File: `platforms/windows/src/engine/src/commands/restructure.rs`

5. **VLMDownloader.swift (HIGH) — `sha256HexOfFile` blocks actor thread**:
   For partially-downloaded models (no sentinel, right size), the sha256 verification was called
   synchronously on the actor thread — blocking for up to ~27 s per file (e.g. 13.5 GB Mistral)
   with no cancellation possible. Fix: offload to `DispatchQueue.global(qos: .utility)` via
   `withCheckedContinuation`, suspending the actor during the hash.
   File: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/VLMDownloader.swift`

6. **VLMDownloader.swift (LOW) + TLSPinning.swift (MEDIUM)**:
   - Sentinel write failure was `try?`-silenced; offline Macs hit HF on every DA launch with no
     log entry. Fix: log `vlm_sentinel_write_failed` via `JSONLog.shared.warn`.
   - `TLSPinningSessionDelegate.pinningRejected` was a plain `var` written on URLSession's
     delegate queue and read on the actor thread — data race under Swift 6. Fix: `NSLock`
     with a computed getter; writer uses lock/unlock in the delegate callback.
   Files: `VLMDownloader.swift`, `platforms/apple/shared/Sources/FileIDShared/TLSPinning.swift`

On-hardware verified (prior session): TrueNAS corpus 60K files, face clustering 3.75 s / 39 faces → 18 persons.

## 2026-06-19 — fix: face clustering "never completes" (macOS HNSW O(ef²) → O(ef log ef))

Three fixes for the face clustering hang reported on macOS:

1. **Root cause fixed — `HNSWIndex.searchLayer` was O(ef²·M)**:
   The beam search used sorted `[(Int32, Float)]` arrays as priority queues. `removeFirst()` is O(ef)
   (shifts all elements); `insertSorted` is O(ef) (array shift after binary-search position). With
   `efConstruction=200` and up to 200K faces, the HNSW build took an estimated 30+ minutes — explaining
   "never completes." Fixed by replacing the sorted arrays with proper binary heaps: `MinHeap` for the
   candidate frontier (O(log ef) extract-min, O(log ef) insert) and `MaxHeap` for the bounded result
   window (O(1) peek-max, O(log ef) evict). Complexity drops from O(ef²·M) → O(ef·M·log ef) per
   `searchLayer` call — approximately 1000× fewer operations for N=200K, ef=200, M=16.
   File: `platforms/apple/engine/Sources/FileIDEngine/Models/HNSWIndex.swift`

2. **Cancellation check added to HNSW build loop**:
   The 200K-face insert loop had no cancellation check, so a Cancel/Shutdown during HNSW build would
   either be ignored until the loop finished or kill the process mid-transaction. Now polls
   `clusterShouldCancel` every 1,000 insertions — responsive without measurable overhead.
   File: `platforms/apple/engine/Sources/FileIDEngine/Pipeline/FaceClustering.swift`

3. **`ArcFaceService.self.env` data race fixed**:
   `load(_:)` read `self.env` without holding `lock`, while `MobileCLIPService`, `BGETextService`, and
   `RamPlusService` were all fixed with the same lock-bracket pattern in the previous session (commit
   `72492b6`). Applied the identical fix: `lock.lock(); let cachedEnv = self.env; lock.unlock()`.
   File: `platforms/apple/engine/Sources/FileIDEngine/Models/ArcFaceService.swift`

Needs hardware verification: `swift build` + face clustering on real Mac against a library with 1K+ faces.

## 2026-06-19 — macOS↔Windows lockstep pass: extension sets aligned + restructure verified byte-faithful

Brought macOS up to Windows (and vice-versa) on every front I can verify headlessly:
- **Extension sets aligned** (the decodable ones, both directions): macOS gained `mts`/`m2ts`
  (AVFoundation) + `odt` (textutil); Windows gained `wmv` (Media Foundation) + `aiff`
  (symphonia) + `ppt`/`xls` (→ Documents/) + a real `odt` extractor. Left divergent only the
  capability-driven formats (macOS-only RAW `orf/rw2/raf`; `flv/mpg/mpeg` that Windows MF
  can't reliably decode → would create failed/looping rows).
- **Restructure verified byte-faithful** (no code change needed): identical pass order (P1
  visual → R3 docs → R1 non-image → rule cascade), identical profiles/thresholds, identical
  non-image bag-of-words signature (filename ∪ whole-lowercased tags — so the new audio
  artist/album tags cluster on both). The rule cascade gives every kind a destination ⇒ no
  file type is un-organizable. Restructure is at production quality on the engine side.
- **Known remaining macOS gap (big, hardware-gated): the RAM++ tagger.** Windows tags images
  with RAM++ (Swin-L, 4585 tags); macOS uses Vision + CLIP-scene tags. Affects the image
  pass's ~22% tag weight + search. A major model addition (ONNX via CoreML + install UI +
  calibration), needs a Mac to verify — tracked, out of this headless pass.

Verified: macOS build + 253 tests; Windows clippy -D warnings + 368 tests.

## 2026-06-19 — content clustering for EVERY file type: audio, code/e-books, 3D models + text-less-doc loop fixed

Restructure now groups every major file type by content, not just images/docs/video — both engines,
lockstep. Branch `feat/content-coverage-all-types` (off main @ d257973).
- **Audio (macOS parity):** macOS did nothing with audio at scan; now `processAudio` reads ID3/Vorbis/
  MP4 metadata (artist/album/title) via a NAS-bounded AVFoundation read → auto tags, so audio clusters
  by artist/album in the non-image pass — matching Windows (symphonia, already shipped).
- **Code + e-books:** ~40 source-code/prose extensions + EPUB classified as `doc` and clustered by
  extracted text (BGE). macOS DocText reads code as UTF-8 + a new `epubText`; Windows `doc_extract`
  mirrors it (read_plain + `extract_epub` + a dep-free `strip_tags`). node_modules/.git already skipped.
- **3D models:** recognize obj/stl/ply/glb/gltf/fbx/usd*/dae/3mf/3ds/off as `model`; `.obj` is
  rendered to a thumbnail at scan and CLIP-embedded (macOS QuickLook→embedImage; Windows obj_render→
  clip) so it clusters with photos/video; other 3D formats group under `3D Models/` + named by Deep
  Analyze. A `.obj`-limited CLIP-backfill carve-out reprocesses existing .obj once.
- **Text-less-doc loop fixed (the "error"):** the BGE backfill carve-out re-walked a doc that yields
  no embeddable text (image-only PDF, iWork, empty file) on every rescan. New `v19_files_text_stage_done`
  column (additive, byte-faithful both engines) gates the carve-out on `text_stage_done = 0`, so each
  doc re-walks once then stops — text-less docs stay skipped, text docs keep their embedding.

Verified: **macOS build + 253 tests; Windows clippy -D warnings + 368 tests** (new: extension mappings,
model + text_stage_done skip-set parity, strip_tags, gate behavior). The added code/e-book/3D extension
sets are byte-identical across engines; migration parity lists + counts updated (18→19) on both.

## 2026-06-18 — doc-embedding backfill on install-then-rescan (both engines) + macOS BGE concurrency hardening

Post-merge audit of the scan-time doc-embedding work found one real lockstep gap and three macOS
concurrency refinements; all fixed, both engines green.
- **Backfill on the "scan first, install BGE later" path (the common case).** BGE is opt-in, so a
  user's first scan predates it; the incremental skip-set then drops those docs by size+mtime and they
  never get embedded — stranding them on weak filename clustering (Windows has no plan-time fallback)
  or re-embedding them at every plan (macOS). Fixed by mirroring the existing CLIP-image backfill
  carve-out for docs: a `text_embeddings`-missing doc/pdf is kept in the pipeline so the rescan
  backfills it — macOS `DBWriter.skipSetTextBackfillExclusionSQL` (gated on `BGETextService`
  installed) ANDed into `Discovery.buildSkipSet`; Windows `SKIP_SET_TEXT_EMBED_GATE` (gated on
  `bge_installed()`) ANDed into the scan_session skip query. Install-gated so it can't force a perpetual
  re-walk; self-healing once embedded.
- **macOS `BGETextService` hardened for scan-time concurrency** (now hit by many doc workers, not the
  old serial plan loop): a `DispatchSemaphore(value: 4)` bounds concurrent ANE inferences and a
  double-checked `loadLock` builds the ORT session exactly once — parity with ArcFace/MobileCLIP. (ORT
  Run is already thread-safe + the tokenizer is immutable, so correctness was fine; these are perf +
  cold-start.)
- Verified: **macOS 251 tests, Windows clippy `-D warnings` + 366 tests** (incl. a new
  `text_embed_gate_reprocesses_embeddingless_docs_only`). A discovery test that asserted an
  embeddingless pdf is skipped was made BGE-install-independent by seeding its `text_embeddings` row.

## 2026-06-17 — macOS embeds doc vectors at SCAN (plan 3 min → 32 s); both engines read the store

Perf + lockstep: macOS embedded document BGE vectors at PLAN time (≈3 min over USB, re-done
every replan); now it embeds them at SCAN like Windows and caches them in `text_embeddings`, so
the plan reads them instantly. Added `processDoc` + a PDF BGE path (a shared `bgeTextEmbeddingBlob`
on visionQueue), a `textEmbeddingBlob` on the DBWriter struct + `insertTextEmbedding` (main +
unchanged-file backfill), and the restructure doc pass now PREFERS the scan-cached embedding
(plan-time only for docs scanned before BGE was installed). **Verified on the owner's library:
a rescan populated text_embeddings 0 → 1076, and the plan dropped from ≈3 min to 32 s at the same
53% doc clustering.** Both engines now read the same scan-time store (tighter lockstep). macOS
build + 251 tests.

## 2026-06-17 — Restructure file-type coverage COMPLETE: video by content + pptx/xlsx + BGE install UI

Closed the last content-clustering gaps so EVERY major file type groups by content, not filename:
- **Video** clustered the last filename-only kind. Windows already CLIP-embedded video keyframes at
  scan (just never used them — restructure filtered `kind='image'`); macOS `processVideo` was a no-op
  (the scan skips AVFoundation to avoid NAS hangs). Now BOTH: the restructure CLIP pass selects
  `kind IN ('image','video')`, and macOS embeds a ~25%-duration keyframe with the same CLIP model,
  bounded by a 6 s off-thread watchdog, run on visionQueue (not the cooperative pool — audit fix).
  **Verified on the owner's library: 364 video moves → 45 content groups at 87% folder-agreement**
  (videos now land with their event photos). 286/295 sample videos embedded (rest timed out gracefully).
- **macOS pptx/xlsx** text extraction (textutil can't read OOXML) via `unzip -p` + a:t/t tag mining —
  mirrors Windows `doc_extract`; ~122 such files now cluster by content.
- **BGE install UI both platforms** — macOS `BGEModelInstaller` + a "Document understanding" Settings
  GlassCard (mirrors RamPlus); Windows a `Bge` ModelSlot + Settings card (mirrors Whisper). So users
  can actually download the ~135 MB model that powers doc clustering (else it falls back to filenames).

3-agent adversarial audit of all the new code: 1 perf defect found + fixed (video embed on the
cooperative pool), rest clean (KeyframeBox lock coverage, the pptx regex, the filter interaction,
both installers vs their proven templates). Windows clippy + 364 tests; macOS build + 251 tests.

## 2026-06-17 — Restructure clusters documents by BGE CONTENT (46% → 53% on the real corpus)

Both engines clustered documents by FILENAME TOKENS only — they never read content. Added a
document-content pass (classify_documents / classifyDocuments) that clusters docs by a BGE-small
embedding. **Windows** already computed + stored these (bge_text + doc_extract → text_embeddings
at scan); it just didn't consume them — small change. **macOS had nothing**, so built it from
scratch + verified on-device: a Swift WordPieceTokenizer (parity-tested port of the Rust one), a
BGETextService (BGE ONNX via ORT/CoreML, mean-pooled like Windows), and DocText (textutil/PDFKit);
embeds at plan time (identical embeddings → lockstep with Windows' scan-time store). Proven by an
owner A/B (49%→57% NN-same-folder) then end-to-end (46%→53% folder-agreement). Calibration trap
hit + fixed: the engine mean-pools BGE (cosines compress high ≈ 0.79), so the A/B's CLS-pooled
thresholds collapsed docs into one folder (24%) until measured + moved to the mean-pooled range
(cluster 0.82 / folder_match 0.78 / auto 0.84). Windows clippy + 37 tests; macOS build + 251 tests
(8 tokenizer parity + 1 on-device BGE). BGE declared for macOS in the manifest (pinned). REMAINING:
the BGE download installer + Settings install card (both platforms) so users get the model — same
pattern as the RAM++ installer / Windows Whisper card (NEXT.md).

## 2026-06-17 — Restructure calibrated on a REAL library: fixed the photo-collapse (2 → 457 groups)

Calibrated Restructure against the owner's real photo library (the "Adlon" external drive) — and
found a genuine latent failure, the actual reason it "sucked": on a real ~3.3k-image personal set,
`planRestructure` auto-merged **109 distinct event folders into ONE "Camera Roll" destination** (2
groups, 38% folder-agreement). Drove it headlessly (release `FileIDEngine` over a FIFO; scanned
Personal + iMac Desktop into the live DB; disabled RAM++ for the scan since it's CPU-bound and the
CLIP embeddings — the calibration input — are produced regardless; numpy distribution analysis +
`planRestructure` scored against existing folders as weak labels).

Root cause (measured on the actual CLIP embeddings): cosines for a *coherent personal library*
compress HIGH (within-event ≈ 0.80, inter-folder centroid p90 ≈ 0.84), but the cluster cosines
(0.50/0.40/0.42) and image-routing bars (0.55/0.72) were tuned for *diverse* images and sat below the
whole distribution → the clusterer merged the entire photo set into one blob that routed to the
nearest catch-all folder.

Recalibrated both engines byte-faithfully (cluster 0.84/0.76/0.76; image folder_match 0.80 / auto_folder
0.86 / auto_coh 0.78 / review_coh 0.70), all now env-overridable (`FILEID_RESTRUCTURE_IMG_*` /
`_CLUSTER_*`). **Validated out-of-box: 2 → 457 event-sized groups, 38% → 70% folder-agreement, biggest
cluster 3254→375.** Rust 37 restructure tests + clippy clean; swift build + 242 tests (one name-agreement
fixture bumped 0.6→0.82 on each side for the new bar). BGE re-evaluation (the `Personal` folder has
~1.3k real docs) + the family-photo `Users` tree are the remaining calibration follow-ups (see NEXT.md).

## 2026-06-17 — Deep Analyze: TRUE AI understanding of audio + 3D (Whisper, 3D→VLM, macOS sound-ID)

Followed the metadata-naming entry below with *real* AI content understanding (owner: "the AI should parse these
things from 3D models to sound and movie files"; "use other models as long as they follow the licenses"). The
audio cascade is now **metadata title → speech transcript → sound event → original name**, and 3D models are
*looked at* by the VLM:
- **Speech (audio)** — Windows: a **whisper.cpp** subprocess (`WhisperRunner`, mirrors the llama.cpp VLM pattern)
  over the 16 kHz mono WAV from the new `audio_decode`; the CPU pack + `ggml-base` model (MIT, sha256-pinned
  `"whisper"` registry entry) install from a new **Settings card**. macOS: **Apple Speech** (`SFSpeechRecognizer`,
  on-device, no download; `NSSpeechRecognitionUsageDescription` added). `name_from_transcript` byte-faithful.
- **3D `.obj` → render → VLM** — Windows: a **hand-rolled software rasterizer** (`obj_render`, no new dep —
  parses `.obj`/`.mtl`, 3/4 camera, z-buffered flat-shaded triangles, 512² PNG via `image`), wired as the
  `"model"` arm of `rasterize_for_vlm`. macOS: the **OS QuickLook 3D generator** (the VLM loader's existing
  fallback) → MLX VLM. Reuses the installed VLM (no new model); falls back to embedded-name metadata on failure.
- **Sound events (non-speech audio)** — macOS: **Apple SoundAnalysis** (`SNClassifySoundRequest`) names field
  recordings / sound effects (rain → "Rain"). Windows YAMNet **deferred** (needs an unverifiable hand-rolled
  log-mel frontend — see NEXT.md/MODELS.md/DECISIONS.md).

All commercial-clean (MIT / Apache / OS frameworks), graceful metadata fallback everywhere. **Verified:** Rust
**364** lib tests + clippy `--all-targets -D warnings` clean; macOS `swift build` + **241** tests. New unit tests:
`collapse_transcript`, `name_from_transcript`, 3× `obj_render`, macOS `transcriptName`/`soundLabel`. No IPC/schema
change. On-device inference quality (VLM/Speech/SoundAnalysis, Windows whisper after install) is hardware-verified.

## 2026-06-17 — Deep Analyze: descriptive names for audio + 3D models (both engines, lockstep)

Deep Analyze's smart-rename now covers audio + `.obj` 3D models, not just image/video/pdf — named from their
EMBEDDED metadata (no VLM, no new model; video already works via keyframe→VLM):
- **Audio** (`.mp3`/`.ogg`/…) → "Artist - Title" from embedded tags (Windows: `audio_meta` `symphonia` probe via
  a new `extract_structured`; macOS: AVFoundation common-metadata in a new `DeepAnalyzeNaming`).
- **3D** (`.obj`) → a descriptive name from the modeler's embedded object/group/material labels (a small
  `.obj`/`.mtl` parser). Needed a new scanned file-kind `FileKind::Model` ("model") on both engines, because
  discovery DROPS `Other` (so `.obj` was never even scanned); audio was already a scanned kind.
- The metadata branch (`analyze_metadata_named_file` / `DeepAnalyzeNaming.metadataResult`) runs BEFORE the VLM
  weights resolve (works without a VLM) and always-handles a matched kind (a metadata-less file is an empty
  success, not a VLM-rasterize bail). Deep Analyze target filters now include `audio` + `model`.

Pure name-builders are byte-faithful across engines, pinned by matching unit tests (Rust 4 + macOS 5).
**Verified:** Rust **354** lib tests + clippy clean; macOS build + **240** tests. No IPC/schema/C# change.
Deferred (needs a MODELS.md/license decision + owner OK): *true* AI audio (Whisper/YAMNet) + 3D (render→VLM).
## 2026-06-17 — Folder-granularity picker (both apps) + app-side audit → 2 more fixes; roadmap reconciled

Closed the last real restructure gap and audited the least-covered area (the apps).

- **Folder-granularity Settings picker (both apps, merged #61, CI-green):** the engine has long read
  `FILEID_RESTRUCTURE_GRANULARITY` but no app surfaced it. Added a segmented Picker (macOS) / ComboBox (Windows)
  in Settings ▸ Restructure; each EngineClient forwards a validated non-default value at spawn (applies on the
  next engine start). Kept on the env mechanism — NO IPC/schema/engine change (the calibrated `granularity_delta`
  hot path is untouched), so zero conformance impact and no regression risk to default users.
- **App-side audit (2 verification-first agents over SwiftUI + WinUI — the engines were audited 4× but the apps
  were not):** the granularity picker audited clean on both. Two real bugs found + fixed: a **C# WinUI native-
  crash class** (`RestartAsync`'s `ConfigureAwait(false)` made `StartAsync`'s State writes raise PropertyChanged
  off-thread; `SettingsView`'s handler mutated `{x:Bind}` TextBlocks directly — the V15.2/V15.4 fast-fail shape;
  fixed by marshaling the batch through `DispatcherQueue.TryEnqueue` like every sibling view), and a **macOS
  People drag-merge data-loss** (dragging a named card onto an unnamed one deleted the typed name; fixed at the
  DB layer — `mergePersons` keeps the typed-named survivor, fail-safe + defense-in-depth). Two engine-lifecycle
  findings deferred with rationale (DECISIONS): "Stop Engine" respawns (crash-recovery-FSM, needs a new state +
  Mac verification) + exit-not-ordered-vs-pump (invasive, self-heals).
- **Roadmap reconciled:** several "remaining" NEXT.md items were already done (mid-review re-plan fix via
  `priorDeselectedIDs`; before/after tree via `TreeDiffView`; per-destination-bucket approval via the
  `.destBucket` drill-down) — corrected to reflect reality.

**Verification:** macOS `swift build` clean (the merge fix is build-verified; ReadStore is app-side, not in the
`swift test` engine suite); Windows app CI-verified. The implementable + headlessly-verifiable work is now
complete; what remains is owner-hardware-gated (threshold calibration on a real library — now a one-tap
experiment via the granularity picker; per-vendor on-hardware UAT; Authenticode/notarization signing).

## 2026-06-17 — Deep whole-codebase audit → 9 verified fixes landed (3 PRs, all CI-green) + a file_ref swap guard

A 4-agent verification-first audit (every finding re-checked against code — this repo has a ~40% audit
false-positive history) swept the restructure pipeline, the cross-platform DB/IPC contract, and the broad
Rust + Swift engines. The freshly-landed learn-from-corrections code audited **clean**. Findings triaged and
landed across three CI-green PRs on `main`:

- **#58 restructure lockstep (6 verified divergences):** macOS computed the Keep/Tidy/Junk tile counts + per-move
  tier on the ALREADY-STRIPPED proposals without the F-C1-004 semantic-claim exemption → the "Keep" tile
  undercounted folders left alone; now computed in `proposeAll` on the full pre-strip set (new tested
  `folderTiersAndCounts` + `PlanResult`). Rust `category_counts` sorted over a HashMap with no tie-break
  (nondeterministic on Windows + diverged from macOS) → count-desc-then-category-asc. `filename_tokens` counted
  graphemes vs Rust scalars → aligned. idf `ln` in f32 (libm-dependent) → f64. `FileDoneEvent.skippedStages`
  non-optional decode → tolerant. + a root-dest guard + a stale migration comment.
- **#59 engine robustness (2 fixed, 1 deferred):** `heic.rs` did WinRT activations with NO COM apartment on the
  apartment-less decoder-pool threads → **every HEIC/HEIF (the default iPhone format) silently failed on Windows**
  with a misleading "codec not installed" message; fixed via the `video.rs::ComScope` MTA pattern (confirmed
  compiling on `windows-engine` CI). `ArcFaceService` lacked the empty-output `baseAddress!` guard its sibling
  `MobileCLIPService` has → crash on a corrupt model; added. The `IPCSink` drainer's actor-held blocking write
  was DEFERRED with rationale (bounded self-healing backpressure; cancellation is independent; the clean fix
  fights Swift 6 `FileHandle` non-Sendability).
- **file_ref swap guard (follow-on PR):** the apply stale-check was path-only — it proved the DB row still
  NAMED the source, not that the file now AT that path was the planned one. Added a positive-evidence-only
  file_ref (NTFS ref / inode) comparison: skip ONLY when both the stored and on-disk refs are known and differ,
  so a same-path swap in the plan→apply window can't move the wrong bytes, and no missing-data case ever
  false-skips. Lockstep `file_ref_swapped` / `fileRefSwapped`, pinned by a pure unit test on both engines + a
  real same-path-swap integration test (real inodes on macOS, `cfg(windows)` NTFS ref on Rust).

**Verification:** Rust **350** lib tests + `clippy --all-targets -D warnings` clean; macOS build + **235** tests.
Both #58 + #59 merged green (incl. the Windows-only HEIC fix on `windows-engine`). The restructure butler is now
best-in-class AND audited; what remains is owner-hardware polish (per-bucket approval UI, granularity slider,
threshold calibration) + ship (signing, per-vendor UAT).

## 2026-06-16 — Learn-from-corrections: instance-based folder memory (both engines, lockstep)

The consuming logic for the v18 `restructure_feedback` table landed, completing the SOTA instance-based
"learn-your-style" loop (no model retraining). A new `restructure_feedback` module on each engine
(`pipeline/restructure_feedback.rs` / `Pipeline/RestructureFeedback.swift`):
- **`record(applied moves, now)`** — every move the user APPLIES is an approved example, so each moved file's
  `filename_tokens` are credited (+1 weight, UPSERT) toward its destination folder's basename. Wired into the
  apply loop (`restructure_apply.rs` / `Restructure.swift`) **alongside the undo journal**, so it shares the
  forward-only gate (stays empty on an undo run) and runs as ONE batched write after the loop. Best-effort —
  a feedback write never fails an apply.
- **`boost(&mut moves)` / `boost(proposals) -> proposals`** — the plan command sums each proposed move's
  (filename tokens → destination folder) feedback weight; at/above `FEEDBACK_AUTO_WEIGHT = 3` it upgrades the
  move to Auto with a "you've filed files like this here before" note. **Additive** — only raises confidence on
  moves the planner already produced, never re-routes — so it can't regress the calibrated image/non-image
  passes. Wired into `commands/restructure.rs` (new `db_for_boost` Arc clone) / `Restructure.proposeAll` on the
  full proposal set, before the anchor strip preserves the upgraded confidence into the emitted plan.

Validated against **authored labeled scenarios** (assistant-as-domain-expert, in lieu of real-data UAT): record
3 "acme invoice" files → /Invoices, then a NEW acme-invoice the planner marked Review is upgraded to Auto; an
unrelated move with no history stays Review; re-recording the same token→folder accumulates weight. Lockstep
parity tests on both engines (Rust 3, macOS 3).

**Both engines fully green:** Rust **349** lib tests + `clippy --all-targets -D warnings` clean; macOS build +
**232** tests (incl. the restructure apply-guard + round-trip suites). `filename_tokens` made `pub(crate)` (Rust)
for reuse; the Swift mirror's `filenameTokens` was already module-internal. Windows app C# is CI-only as always.
Next: commit on a branch → confirm both CI workflows → deep whole-codebase audit.

## 2026-06-16 — Restructure deep-research sweep + 4 verified best-in-class wins

`/deep-research` (27 web sources → 21 verified claims) + a 3-agent codebase audit graded Restructure against
the state of the art. Headline: the architecture already matches or beats the documented field (density
clustering, c-TF-IDF naming, journal-backed reversible apply, barycentre Sankey). Verification caught several
**audit false positives** (image profile IS calibrated; `Path::starts_with` is component-aware so the
"/Library2" containment bug is unreal; Windows already surfaces confidence+reason and already has the Undo
button). Landed + verified (Rust 343 + clippy, macOS build + 226):
- **Incremental crash-safe undo journal** (both engines) — append each inverse move as it happens + periodic
  fsync, instead of one write after the loop, so a crash mid-apply still leaves completed moves undoable.
  This is the research's #1 open question (the field only ships best-effort undo).
- **Single folder-granularity knob** (both engines) — `FILEID_RESTRUCTURE_GRANULARITY` ∈ {loose,normal,tight}
  shifts the cluster cosines (HDBSCAN `min_cluster_size` philosophy); one lever for owner calibration.
- **Empty-dir cleanup on undo** (both engines) — undo removes the orphan empty group folders apply created
  (`remove_dir` empty-only, root-contained).
- **Confidence + reason surfaced on macOS** — `RestructureView` rows now show the band badge + the engine's
  "why filed here" (the IPC always carried them; `mapProposals` was dropping them). Windows already had it.

Then a 3-agent **lockstep parity sweep** (instructed to verify values against code, since the prior audit had
false positives): **engine constants/algorithms = zero divergences**; **DB migrations (17) + IPC (31 cmds / 24
events) + conformance = perfect sync** (a library round-trips cleanly). One real bug found + fixed: the new
**person-as-tag name diverged** (macOS used `PersonRow.displayName`, full; Windows used title+first only) —
both now build an identical `personTagName` / `FormatPersonTagName` (title+first+middle+last+suffix joined,
else legacy name). Two agent claims were misses (Windows DOES have the Restructure confidence badge in
`DrillDownSheet` + the Undo button). macOS verified (build + 226); Windows person-tag change is CI-pending.

**All three CI workflows are green on `main`** (macOS app `0f61a04`, Windows engine `6cc3291`, Windows app
`fa40dfb`). Getting macOS green surfaced a real find: the long-standing `nonImageGroupsByFilename` CI failure was
NOT an engine bug — on-runner diagnostics proved the lone no-signal file is excluded before clustering and the
moves never contained it; the failure was the test's `#expect(!moves.contains { $0.fileID == 999 })`
negated-trailing-closure mis-evaluating on the runner's Xcode 16 swift-testing macro (not on local Xcode 26.5).
Fixed by materializing the ids + closure-free `contains`. Also hardened `macos.yml` to stop caching build
products (stale-object hazard) and kept the engine determinism improvements (singleton pre-exclusion + kNN clamp).

Remaining (research-backed, in NEXT.md): list-tier + "apply only high-confidence"; name-based routing signal
(Dropbox finding, needs real-data validation); learn-from-corrections (new migration); per-bucket approval +
before/after tree (large UI); granularity Settings slider; mid-review re-plan fix; file_ref stale guard;
owner threshold calibration. Pixel-level app parity + a full no-bug sweep need on-hardware runs + CI.

## 2026-06-16 — 5-item UX batch: byte-exact dedup, person-name filenames, apply buttons, explainer, auto-advance

Five user-requested changes, all lockstep on both platforms. macOS + both engines verified locally; the Windows
app slices (C#/XAML) are CI-pending (no local WinUI build).
- **Item 3 — person names → Deep Analyze filenames** (the reported bug). Named people are now prepended (deduped,
  ≤3, sanitized) onto the VLM proposed filename + injected into the caption/rename prompts. Byte-faithful
  `apply_person_prefix`/`applyPersonPrefix` + `fetch_face_names`/`format_person_ref` on both engines. Rust
  clippy + 343 tests; Swift build + 222 tests.
- **Item 1 — macOS byte-exact dedup.** Cleanup groups by `content_hash` (SHA-256, CryptoKit, **no new dep**)
  instead of perceptual phash, so only literally byte-identical photos count as duplicates. `ContentHash.swift`
  ports the Windows `content_hash` STRUCTURE (full ≤16 MB; head+4×interior+tail+size composite above), computed
  at scan into the existing `content_hash` BLOB; dedup + counter queries switched to GROUP BY content_hash.
  4 new tests + 226 total green. Values are macOS-local (SHA-256 ≠ Windows BLAKE3) — see DECISIONS.
- **Item 5 — apply buttons.** Deep Analyze tab now has separate **Apply tags / Apply people-as-tags / Apply all**
  (smart-name review path unchanged). New capability: person names written onto files as Finder/Explorer tags
  (DB-only before). macOS app-side via `TagWriter`; Windows reuses `applyTags`/`renameFiles` grouped by tag/person
  — no IPC contract change.
- **Item 2 — auto-advance.** People tab shows a gold "Continue to Deep Analyze →" CTA once ≥1 person is named
  (starts analysis + switches tab); the skip path stays. Both platforms.
- **Item 4 — explainer.** Dismissible, persisted "Tagging vs. Deep Analyze" banner on the Deep Analyze tab. Both
  platforms.

Verified: macOS `swift build` + 226 tests; Windows engine `cargo clippy -D warnings` + 343 tests. Windows app
(items 2/4/5 C#/XAML) is CI-pending.

## 2026-06-16 — Whole-codebase adversarial audit (6 parallel agents) + Windows undo button

Windows "Undo last run" button landed (WinUI `RestructureView` — `CanUndoRestructure`-driven, in the
ApplyBar; CI-pending, no local WinUI build). Then a 6-agent parallel audit swept the ENTIRE codebase
(Rust engine ×3 slices, Swift engine, Swift app, C# app). The **security-critical slice**
(IPC/shell/downloader) and the **ML slice** came back CLEAN — no-telemetry single-egress invariant
confirmed holding, all 32 default models SHA-pinned, file-op/traversal/IPC-framing/watchdog/WAL all
defended. Real bugs found + FIXED, all verified green:
- **P1 (Swift, data-loss + parity):** the macOS `files` UPSERT clobbered phash/GPS/camera/has_faces on
  a stage-skipped rescan — the Rust engine's R3-04 COALESCE/CASE-WHEN hardening was never ported.
  Ported verbatim + added the missing regression test.
- **P1 (Swift app, wedge):** Restructure apply/undo wedged `applying` forever if the engine replied
  with a bare error (db_unavailable: engine alive, no result, no reset) — the `lastErrorSignal`
  handler only cleared `loading`. Now clears `applying` too.
- **P2 (Rust, parity):** `path_search` stored verbatim (not NFC-normalized) at restructure-apply /
  bulk-rename / trash-restore → NFD-accented names unfindable until rescan. Normalized at all three
  (macOS already did). **P2 (Rust, parity):** image-pass *drained* tags so unclustered images lost
  content-tag grouping in the non-image pass — read non-destructively now (matches macOS).
- **P2 (Swift app):** `modelDownloadProgress`/`autoPilotActive` never reset on engine-exit (stale
  download bar + defeated watchdog after a crash); Cleanup per-group delete lacked a double-tap guard.
  Both fixed.

Verified: macOS swift test **222/222**, Rust cargo test **343/343** + clippy -D warnings clean.
Cosmetic/self-limiting P2s (C# dead error-kind, vestigial Tag-is-int drag branch, ClipSearchService
dispose race, ScanCoordinator bumpProcessed perf, the known-deferred bbox-verdict cross-platform key)
tracked in NEXT.md — no user impact; the C# ones are unverifiable without a local WinUI build.

## 2026-06-16 — Restructure R2: one-click "Undo last run" (reversibility) + R1 validated on a real library

**R1 validated on the owner's TrueNAS library** and tuned: real data exposed three naming bugs —
extension tokens leaking on double extensions (`E14.jpg.lps`→"jpg"), English connectors ("Boys
The"), and versioned junk folders (`Desktop 1.0` not caught) — all fixed (filename stopwords +
token-prefix junk detection, both engines). Result is good + conservative: résumés pulled out of
`Desktop 1.0` into a "Nolle Resume" group, copyright docs grouped, diverse docs left in place.

**R2 undo landed.** Apply was one-way. Now `apply` writes an inverse-move journal (each file's
new→original path; truncating → last-run-only) and `undoLast` replays the inverse moves THROUGH
`apply` itself — reusing every safety check (stale-guard, containment, no-clobber, DB update)
rather than duplicating them — then clears the journal so a run can't be undone twice. New IPC
command `undoRestructure`; the macOS app shows an "Undo last run" button after any apply that moved
files (+ corrected "you can reverse this" confirmation copy).

Both engines + the full contract: macOS (`Restructure.undoLast` + dispatch + EngineClient +
RestructureView), Rust (`RestructureApply::undo_last` + `handle_undo_restructure` +
`UndoRestructure` IPC), schema + C# command + C# VM (`CanUndoRestructure`). **macOS swift test
220/220** incl. a real apply→undo round-trip (file relocated → restored → DB + journal correct);
**Rust cargo test 343/343 + clippy -D warnings clean.** Remaining: the Windows app's XAML "Undo"
button (GUI parity, CI). **VLM cluster naming (P2) is CUT to R3** — real-data c-TF-IDF names are
already good, so it's marginal polish, not a ship blocker.

## 2026-06-16 — Restructure R1: butler now organizes ALL file types, not just photos (both engines)

**The fix for "Restructure just sucks."** Root cause: the semantic butler only ran on images
with a CLIP embedding (`Restructure.swift` guard `kind == "image"`); every document, PDF,
video, and download fell through to a 7-bucket date cascade — so a real mixed library dumped
all docs into `Documents/<Year>/` with zero content awareness. R1 adds an **additive non-image
semantic pass**: cluster everything the image pass didn't claim by a **filename-token + tag
bag-of-words** signature (the bag-of-words IS the representative vector — no model needed),
reusing the exact same density clusterer + learn-your-style folder matching under a separate,
tighter `nonImageProfile`. The image path is byte-identical (a `Profile` was extracted with the
old constants), so it cannot regress. Generic dumping grounds (Downloads/Desktop/Temp…) are
barred from being learn-your-style prototypes — the whole point is to move files OUT of them.

Both engines, byte-faithful + lockstep: macOS `RestructureSemantic.swift` + `Restructure.swift`;
Rust `restructure_semantic.rs` + `commands/restructure.rs` (also un-gated tag loading so docs get
their tags too, matching macOS). Owner kill-switch `FILEID_RESTRUCTURE_NONIMAGE=0`; thresholds
env-tunable (`FILEID_RESTRUCTURE_NI_*`) for calibration on a real library before defaults promote.

Verified here: macOS `swift build` + **swift test 218/218** (2 new non-image tests); Rust
**cargo test 343/343** (2 mirrored) + **clippy -D warnings clean**. R3-07B (IPC 64 MiB cap +
macOS newline-scan resume) was already landed in a prior session — verified end-to-end, so
whole-library plans render; only a stale comment needed fixing. **Remaining: R2** (VLM cluster
naming + one-click "Undo last run") and **owner UAT** to calibrate the non-image thresholds on a
real library (NEXT.md). macOS restructure builds + tests green — `RESTRUCTURE.md`'s "written,
unverified" line is now stale.

## 2026-06-16 — RAM++ in the first-run modal (macOS) + Welcome re-show parity (both platforms)

RAM++ was installable only from Settings on macOS — the first-run WelcomeSheet offered
CLIP / Face / Deep Analyze but not the tagger, so a new user silently got the weaker
CLIP/Vision tag fallback unless they hunted for it. Brought macOS to Windows parity:
RAM++ is now a gating row in `WelcomeSheet.swift` (row 2: CLIP → RAM++ → Face → Deep
Analyze), included in "Install all", the `.onAppear` refresh, `allInstalled`, and
`anyInProgress`; `FileIDApp.shouldShowWelcome()` now re-shows the sheet when RAM++ is
missing too. Pure wiring — `RamPlusModelInstaller` (download / SHA-256 / 12-part) already
existed and is identically shaped to the CLIP/ArcFace installers already in the sheet.

**Welcome re-show gating split (both platforms):** the re-show gate is now the three core
sub-1 GB models (CLIP + RAM++ + face); the multi-GB Deep Analyze VLM is install-once /
skippable and no longer re-nags every launch. macOS already had this split implicitly
(`shouldShowWelcome` ⊂ `allInstalled`); Windows now mirrors it via a new
`ModelInstallerService.CoreModelsInstalled` (CLIP+RAM+++ArcFace) consumed by
`MaybeShowWelcomeSheetAsync`, leaving `AllInstalled` (incl. VLM) for auto-dismiss + Done.

Parity audit of the onboarding/model surface also **cleared two suspected drifts**: the
Deep-Analyze VLM is already family+tier-aligned (Qwen2.5-VL-7B ≥16 GB / Gemma-3-4B <16 GB
on both; MLX vs GGUF formats differ by necessity, not drift), and CLIP is byte-identical
(same `Xenova/clip-vit-base-patch32` ONNX, same SHA-256s → embeddings round-trip safely).

macOS verified here: `swift build` clean, full suite **216/216 green**. Windows C# (the
`CoreModelsInstalled` split) is build-pending on CI — no local C# toolchain in this env.

## 2026-06-15 — bbox cross-platform parity landed (#55) — macOS lockstep COMPLETE

bbox parity was the last lockstep item. Root cause: macOS stores face bbox as "x,y,w,h"
NORMALIZED bottom-left (Vision); Windows stores JSON {x,y,w,h,…} in PIXELS top-left
(SCRFD). A library scanned on one OS + opened on the other had its faces fail to crop
(foreign parser → nil → excluded/blank). Fix is **macOS-side read-tolerance only** —
new `FaceBBox.parseNormalized` (FileIDShared) parses BOTH formats → normalized bottom-left,
threaded through the 3 macOS bbox readers (parseBBox/cropFaceCGImage, FaceAlign
matchLandmarks, PeopleView.cropFace). Windows needs NO change: it never reads bbox back
for cropping (saves face-crop JPEGs at scan time, clusters from embeddings; its only bbox
reads are R3-15 string-equality matches). The CSV branch is byte-identical → within-platform
behavior unchanged (safe). FaceBBoxTests verify CSV passthrough + the JSON pixel→normalized
+ origin-flip conversion; full macOS suite 216 green.

**macOS model-stack lockstep is now COMPLETE** — SFace/CLIP swap, RAM++ tagger (+ install),
VLM tags, R3-15, IPC cap, FaceAlign (on by default), detection-recall, and bbox parity all
merged + green. Remaining work is purely on-hardware/GUI UAT + release signing (NEXT.md).

## 2026-06-15 — face quality: detection recall fix + FaceAlign ON by default (#53, #54)

Two face-quality fixes after on-Mac feedback ("faces not detected + different people merged"):
- **Detection recall (#53):** the image fed to Vision face detection was downscaled to
  512 px, so faces <~10% of a 4000 px frame (~50 px) fell at/below Vision's limit and were
  missed. Bumped to 1536 px (CLIP/RAM++/phash/OCR downsample internally → unaffected),
  tunable via `FILEID_SCAN_MAX_PIXELS`. Cluster auto-merge + pass-1 cosines made env-tunable
  (`FILEID_FACE_TIGHT_COS`/`SMALL_COS`/`PASS1_COS`, defaults preserved) for corpus calibration.
- **FaceAlign ON by default (#54):** flipped `FaceAlign.enabled` to default-on (escape hatch
  `FILEID_FACE_ALIGN=0`). Aligned SFace crops are discriminative, so the thresholds (which
  assume aligned input) stop over-merging different people.

The fix for face quality was alignment + detection-resolution + threshold calibration —
NOT retraining the embedder (which would overfit + fork the cross-platform 128-d space +
break licensing). Full macOS suite 213 green; build debug+release clean.

## 2026-06-15 — 5-point FaceAlign landed opt-in (#51); only bbox parity remains (cross-platform, deferred)

FaceAlign (#51) wires `align112` into the macOS face-embed backfill behind
`FILEID_FACE_ALIGN` (default off): one `VNDetectFaceLandmarksRequest` per image →
5 NAMED landmark regions (leftEye/rightEye/nose/outerLips), assigned to template
slots by image-x (so no subject/viewer naming ambiguity), matched to the stored
bbox by center, aligned; falls back to the bbox crop on no match. Deterministic
(no landmark-order guessing), no schema change (landmarks re-derived at embed),
diagnostic `face_align_applied` log. FaceAlignTests verify the similarity fit;
full macOS suite 213 green. **Mac validation:** scan with the flag unset vs `=1`,
compare People clustering (the thresholds already assume aligned input, so it
should tighten, not regress); flip the default once confirmed.

**Only bbox pixel/JSON parity remains** — and it's deliberately deferred, not
merged blind. macOS stores normalized bbox, Windows stores pixels in JSON; safe
cross-platform read-tolerance needs a coordinate-space conversion threaded through
every consumer (parseBBox / cropFaceCGImage / matchLandmarks / PeopleView.cropFace
/ bboxArea), the exact multi-consumer change a prior swap broke clustering on. It
only matters for a library scanned on one OS and opened on the other (single-platform
users are unaffected), so it needs a real cross-platform DB + both-platform
validation — recipe in NEXT.md. Everything else in the lockstep is merged + green.

## 2026-06-15 — R3-15 data-loss fix + lockstep delta follow-ups landed (PRs #48–#49); only FaceAlign/bbox remain (Mac-gated)

Closed the last deferred FIX and the delta-re-audit follow-ups. Both engines build-
green; the new code was delta-re-audited by 5 agents (the macOS frame-scan even
survived a 200k-trial differential fuzz, 0 mismatches) and a delta-2 confirmed dry.

- **#48 R3-15 — durable face-verification keys (both engines, the last data-loss fix).**
  v13's face_a/face_b are face_print ids that churn on every faces_evaluated re-scan,
  so a user "different people" verdict silently stopped blocking the merge. Fix =
  churn-stable (file_id, bbox) keys via coordinated migration **v17** (identical
  identifier both engines; both canonical parity arrays updated + asserted equal),
  verdict-write populates them (Windows; macOS write is app-side/not_implemented),
  apply re-resolves (file,bbox)→current face id with legacy fallback. Additive
  migration + graceful-degrade = bounded blast radius. Churn-survival regression test
  on both engines. Windows 342 tests; macOS MigrationParityTests green.
- **#49 delta follow-ups.** The delta re-audit found: R3-15 was half-closed —
  `handle_find_merge_suggestions` still re-prompted rejected pairs via the churning
  ids (now resolves via the stable key too); RAM++ `FILEID_RAMPLUS_THRESHOLD` was a
  no-op with the per-class sidecar present + env knobs lacked a [0,1] guard; the RAM++
  downloads weren't SHA-256-pinned (now in ModelManifest, with the correct 925 MB size
  + a pre-preflight staging sweep); the VLM tag pass now uses 40-token greedy (Windows
  parity) and parseVLMTags trims/splits on all whitespace. (A ModelManifest parity test
  caught the JSON platform tag — fixed: RAM++ entries are now macos+windows.)

**macOS model-stack lockstep status:** RAM++ tagger (engine + install UI), VLM tags,
SFace/CLIP swap, and now the IPC cap + R3-15 are all MERGED. The only remaining
lockstep pieces are **FaceAlign** (Vision landmark order is undocumented →
Mac-only-determinable) and **bbox pixel/JSON parity** (coordinate-space change +
cluster-threshold retune → needs labeled data). Both are the user's-own-principles
"verify on Mac / tune against real data" items — full recipes in NEXT.md. Everything
mergeable + verifiable-here is on `main` and green. Behavioral validation of the
merged lockstep (install RAM++, scan, confirm tags/scores; Deep Analyze → vlm tags)
is the Mac UAT.

## 2026-06-14 — post-audit: IPC cap (R3-07B) + macOS model-stack lockstep underway (PRs #43–#46)

After the audit converged (round 7), turned to the remaining-work survey's real
code items — closing the last deferred IPC item and starting the macOS lockstep
(bringing the macOS engine onto the known-good Windows commercial-clean stack).
All merged green on `main`; macOS edits are build-verified (swift build debug+
release + unit tests) but their ML *behavior* still needs a Mac + labeled photos.

- **#43 R3-07B/R5-12 — IPC frame cap 32→64 MiB + O(n²)→O(n) macOS frame scan.** Bumped
  all 5 symmetric cap sites (incl. the macOS engine's stdin LineBuffer, a tighter
  16 MiB cap the survey missed). Both macOS framers now carry a scanned-prefix offset
  so a large frame isn't re-scanned every readability tick; LineBuffer made testable
  + 10 SharedTests. Two survey items debunked as already-fixed false positives
  (IpcSchema.Tests runs directly in CI; rename-heal exact-dup is guarded on both
  engines via the round-3 old-path-gone gate).
- **#44 RAM++ primary tagger (engine).** 4585-class RAM++ (RamPlusService, faithful
  port of ram_plus.rs) replaces the weaker Apple Vision classifier; per-tag scores →
  tags.score. Degrades gracefully to Vision tags when the model isn't installed.
- **#45 VLM searchable tags (source='vlm').** Second VLM pass in Deep Analyze mirrors
  the Windows Both-mode; parseVLMTags byte-identical + 4 ported unit tests.
- **#46 RAM++ install UI.** RamPlusModelInstaller + Settings card download the 3 RAM++
  files from the project HF mirror — activates the engine-side tagger.

**Remaining lockstep (NOT yet landed — see NEXT.md):** 5-point FaceAlign (⚠ Vision
landmark order is undocumented → Mac-only-determinable; wrong order = orthogonal
embeddings), bbox pixel/JSON parity (coupled + a prior swap broke clustering →
needs a threshold retune), and R3-15 durable face-verification keys (both-engine
additive migration + verdict write/resolve — a real data-loss fix). These need a
Mac / labeled data / focused care and are the next priorities.

## 2026-06-14 — round 7: the last-uncovered UI surface — 39 defects fixed + 4 delta regressions (PRs #40–#41); audit converged

Swept the surface rounds 1–6 never reached: the big action-bearing **C# Views +
ViewModels** (Library/People/Cleanup/DeepAnalyze + their VMs, EngineClient command
senders, modal sheets, sidebar) and the **remaining Swift Views** (Settings,
Cleanup, TreeDiff, sidebar, window shell, and the uncovered parts of the big three).
17 finder units → 3-lens default-reject vote → domain-expert recipe surfaced
**39 confirmed real defects** (0 P0/P1, 17 P2, 22 P3). All fixed via per-file
parallel fixers, every diff re-verified against live code + read before commit.

- **#40 macOS app (17):** stale thumbnails/previews from offset-keyed lists and
  `.task` without `id:` (Cleanup CopyTile, FilePreview, FinderTags, TreeDiff dup-id);
  heavy sync work off the MainActor (bulk delete+tag, face-reassign write, the 27×
  COUNT(*)/render pipeline strip, a dead per-batch COUNT); missing in-flight guards
  on bulk-merge (+ no silent row-drop on failure); a suggestions sheet force-opening
  over user nav; a bookmark-restore lost-update; an un-cancellable tag read; CLIP
  phantom "Downloading…" + per-tick syscalls; Start-button un-wedge; folder-pick
  readability pre-validation.
- **#41 Windows app (22):** overlapping-refresh races in 3 ViewModels (mirror the
  CI-green LibraryViewModel A4/A5 generation-guard + active-loads); re-entrant
  double-apply on bulk merge / mark-unknown / trash / per-row merge; inflight
  thumbnail-CTS removed by key without identity check (+ leak); the latched
  DeepAnalyzeLast re-applied every progress tick (inflated pill / stale caption);
  Cancel not stopping the Analyze-Selected batch; multi-MB IPC encode + blocking
  recursive wipe deletes on the UI thread; an O(N²)→O(N) selection-maintenance
  coalesce; a stale select-mode snapshot; a dead double row-build; the pipeline
  strip not resetting after a wipe; the per-prefix BulkActionResult reply gate.
- **Round-7 delta (folded into #40/#41):** the delta stage caught a self-inflicted
  regression in **4 files** (its 5th straight non-empty round): the FilePreview
  `.task(id:)` lacked the cancellation guard its own sibling added (stale-poster
  race); the CLIP `presentFilePaths` cache wasn't refreshed on partial-install
  failure; the Windows thumbnail `cts.Dispose()` was unconditional (cross-thread
  concurrent dispose) vs the guarded CleanupView twin; the pipeline-strip reset
  fired on "Clear folder" too (blanked an intact library) → re-derive the floor
  from the DB. All fixed; **delta-2 came back dry.**

**Audit converged.** Tally across the campaign: **64 real defects fixed** over
7 find-rounds (8/20/12/11/7/+delta/39) + 5 delta-rounds, every batch merged
CI-green with the only-`main`/no-open-PRs terminal state held between batches.
The find→verify→expert→read→delta→confirm-dry method covered the engine, data
paths, and the full UI surface on both platforms; round 7 was the last untouched
tier. Gates at HEAD: macOS `swift build` debug+release + 195 tests; Windows
`cargo clippy -D warnings` + 341 tests + .NET app (x64/arm64) + engine
(x64/arm64-native/arm64-cross); all post-merge `main` CI green. Remaining work is
exclusively HARDWARE / GUI / LABELED-DATA UAT + the two deferred coordinated
cross-platform changes — see NEXT.md.

## 2026-06-14 — round 6: action-bearing UI Views audited — 7 defects fixed (PRs #37–#39)

Pushed the same method onto the surface rounds 1–5 left for last: the **action-bearing
UI Views** on both platforms (Restructure, Deep Analyze, People naming, the model-install
Welcome sheet, the file-preview sheet). 7 candidates → 7 real, all merged CI-green; the
delta stage again caught a self-inflicted regression (R6-07), continuing its 4-for-4 record.

- **#37 macOS app Views (5):** R6-01 RestructureView applied the *live* eligible-move set
  at confirm time, not the set shown when the confirm dialog opened — a move that became
  ineligible between present and tap was still applied (P1) → snapshot `pendingMoves` at
  present; R6-02 DeepAnalyzeViews ran 4 Hz on-main DB `COUNT`s for the status header →
  cached via `refreshStatusCounts()` on appear/change; R6-03 Sankey source-tap drilled down
  on the basename, colliding distinct folders that share a leaf name → use the full
  `identityKey` path; R6-06 Sankey cursor `@State` was written on every mouse move even off
  any ribbon (layout thrash) → write only over a ribbon; R6-07 WelcomeSheet keyed the
  Failed-state `onChange` on the error *message string*, so a retry failing with the same
  message never re-flipped to Failed (spun forever) → key on the monotonic `lastErrorSignal`;
  also added a 45 s VLM download-stall watchdog.
- **#38 Windows app Views (2):** R6-04 RestructureView.xaml.cs `_applying`/`_applyingPlan`
  were per-instance, so a re-navigated view could release the plan another apply was mid-flight
  on → made `static` + release-guarded on `ReferenceEquals(plan, _applyingPlan)`; R6-05
  FilePreviewSheet.xaml.cs `OnMediaPlayerFailed` could enqueue against a stale media generation
  after unload → capture `_mediaGen` and bail on mismatch/unload.
- **Round-6 delta (#39):** R6-07's stall watchdog kept its 45 s timer armed through the
  silent post-download MLX cold-load and false-fired "Download stalled" on a legitimately
  loading model → gated the fire on `vlmLastFraction < 0.999` so it arms only while the
  download is actually in flight; the `vlmInstalled` sentinel owns the cold-load phase.

Gates at HEAD: macOS `swift build` debug+release + CI macOS-app green; Windows `cargo clippy
-D warnings` + 341 tests + both Windows workflows green; all post-merge `main` CI green.
Only `main`; no open PRs. Running tally across the campaign: 60 real defects fixed over
6 find-rounds + 4 delta-rounds.

## 2026-06-14 — rounds 4–5 deep audit: 23 more defects fixed (PRs #30–#35); the engine + app are now exhaustively audited

Extended the round-3 method to the surfaces rounds 1–3 didn't reach. Each round:
per-file finders → 3-lens default-reject vote → per-finding domain-expert
re-verification + fix recipe (with cross-platform parity check) → apply + test →
**delta re-audit of the landed diff** → confirm-dry. The delta stage earned its
keep: it caught a self-inflicted regression in **every** round (round-3 delta 2,
round-4 delta 2, round-5 delta 1 — all fixed + confirm-dry'd clean), which the
find+verify passes missed.

**Round 4 — engine files rounds 1–3 skipped (FFI, IPC, migrations, stdio loop,
person-ops, core clustering, discovery).** 14 candidates → 12 real.
- **#30 Windows engine (9):** R4-01 incremental skip-set was a tautological
  stored-vs-stored predicate that silently stranded edited files (P1) → now carries
  (size,mtime) + revalidates vs the live file (macOS already did this); R4-02/08
  Pass-2 clustering O(n²)+singleton-merge (P1); R4-03 cpu_topology FFI heap-OOB UB;
  R4-04 one-byte-per-read stdin; R4-05/06/07 person-ops data-loss (mark-unknown
  leaves sub-fields / merge deletes face-anchored verdicts / merge drops the
  source name); R4-09 EP-arm-on-forced-CPU false-poison; R4-12 CUDA version sort.
- **#31 macOS engine (3):** R4-02/08 Pass-2 twin; R4-10 engine-writer busy timeout
  (was dropping writes under the app's WAL lock); R4-11 cancelScan start-window race.
- **Round-4 delta (#32):** R4-07 over-grafted a name onto an already-named merge
  target; R4-11 cancel attributed to the enqueued (not running) epoch could poison
  a queued scan. Both fixed.

**Round 5 — app-side data paths + model install + C# (rounds 1–4 skipped).**
15 candidates → 11 real (the 12th = the already-deferred R3-07 PART B).
- **#33 macOS app (9):** R5-01 bulk-merge union-find could DELETE a user-named
  person + leave the survivor unnamed (P1) → keeps the named/larger root;
  R5-02 bulkMarkUnknown off-main batch; R5-03 bulk-rename notify-once; R5-04
  bulk-tag dropped off-window selections → id→FileRow model; R5-07 respawn-flap cap;
  R5-08 case-only-rename inode check; R5-09/10/11 off-main + cancellation hardening.
- **#34 Windows+C# (3):** R5-05 model-install legacy-sentinel now SHA256-revalidates
  before vouching (stale installs masqueraded as current); R5-06 C# auto-cluster gate
  reset on crash; R5-07 C# respawn-flap cap (cross-platform twin).
- **Round-5 delta (#35):** R5-11's off-main tag-edit conversion introduced a
  lost-update race + deferred draft-wipe → serialized via a per-editor task chain.

**Tally for the session:** 53 real defects fixed across 5 find-rounds + 3 delta-rounds
(8 + 20 + 2 + 12 + 11), all merged CI-green; 3 deferred with full both-platform
recipes in NEXT.md (R3-15 cross-platform face-verification-anchor migration; R3-07
PART B / R5-12 IPC cap raise). Gates at HEAD: Windows `cargo clippy -D warnings` +
341 tests; macOS `swift build` debug+release + 195 tests; both CI workflows + the
.NET app build green on `main`. On-hardware e2e (isolated Adlon copy) GREEN. Only
`main`; no open PRs. Remaining work is exclusively HARDWARE / GUI / LABELED-DATA /
the two deferred coordinated changes — see NEXT.md.

## 2026-06-14 (round 3) — round-3 deep audit: 20 more defects fixed across both platforms (PRs #25–#27); 1 schema-migration finding deferred

Ran a third, deeper adversarial audit: 16 per-file finders over the highest-risk subsystems →
3-lens **default-reject** verification (32 candidates → 21 survivors) → a per-finding
**domain-expert re-verification + fix-recipe** pass (all 21 confirmed real; 3 had the suggested
fix corrected). Landed in three platform-clean PRs:

- **#25 Windows engine (7):** R3-03 face-clustering unknown→named merge (P1 data-loss);
  R3-04 upsert nulling phash/camera/GPS + flipping has_faces/has_text on a stage-skipped re-scan
  (P1); R3-16 hoist heal `symlink_metadata` out of the writer txn; R3-17 heal size corroboration
  (cross-volume MFT-ref collision — Windows mirror of F-A2); R3-18 SEC-5 non-ASCII case-fold bypass;
  R3-19 `tags_evaluated` wiping auto-tags when no tagger ran; R3-21 scan-notice lost under
  backpressure. clippy clean; 340 tests (+2).
- **#26 macOS engine (9):** R3-01 empty-PARSED-description clobbering a caption (P1, extends F-A6);
  R3-02 unknown-unmark-during-window deletes a person + orphans faces (P1 data-loss); R3-09 HNSW
  `vDSP_distancesq` (no per-call scratch); R3-10 auto-merge `named` predicate widened to
  title/middle/suffix; R3-11 auto-merge re-reads identity under the writer lock; R3-12 semantic
  restructure HNSW above 5_000 files (was O(n²)); R3-13 PDF open-failure recorded as success;
  R3-14 hoist heal `lstat` out of the writer txn; R3-20 download progress monotonic guard.
  swift build debug+release clean; 195 tests (+1, persistCoalesces extended).
- **#27 macOS app (4):** R3-05 `duplicateGroupsAsync` off-main twin (Cleanup tab); R3-06 off-main
  `@Observable` writes (`version`/`lastError`) serialized on main + lock-backed `lastError` shadow;
  R3-07 PART A oversized-frame message made actionable; R3-08 serial AsyncStream event pump so
  `handleEvent` runs in receipt order. Built via `swift build --product FileID`.

**Deferred (see NEXT.md):** R3-15 (a "different people" verdict is lost after a re-scan churns
face_print ids — needs a churn-stable face identity → a NEW append-only migration mirrored
identically on BOTH engines, the C12 fork-bug class, plus Windows-runtime verification; not landed
blind). R3-07 PART B (32→64 MiB inbound cap + O(n²) buffer-scan fix + cross-platform constant
mirror — IPC-contract change, land coordinated).

Method note: the 3-lens skeptic pass over-passes (~40% historical FP), so the per-finding
domain-expert recipe pass is the load-bearing filter; every landed fix was read against the real
code before applying. See DECISIONS.md.

## 2026-06-14 (earlier) — deep-audit batch: 8 confirmed defects fixed across both engines (PRs #23–#24 merged; main green) + full on-hardware e2e

Ran an adversarial multi-agent audit (finder shards → 2-of-3 skeptic verification) over the
highest-risk subsystems, then **expert-re-verified every candidate before fixing** — the skeptic pass
alone still carried a ~40% false-positive rate. Net: **8 genuinely-real defects fixed, 6
false/over-stated rejected with rationale.**

**Windows engine (PR #23, CI-verified — `#[cfg(windows)]` paths don't compile under macOS clippy):**
- **SEC-5 casing bypass** (`restructure_apply.rs`): the reparse-point-in-chain walk compared
  paths with case-sensitive `starts_with` after only one side was normalized, so a drive whose
  casing differed from the root could slip the containment break. Replaced with a component-wise
  case-insensitive `ci_starts_with` (avoids the sibling-prefix bug a lowercased-string compare would
  reintroduce).
- **Verdict swallow** (`bulk.rs handle_find_merge_suggestions`): a `face_verifications` query whose
  error was `.ok()`-dropped could silently skip the "these two are different people" exclusions and
  re-suggest a rejected merge. Now propagates with `?`.
- **`cancelled` flag** (`deep_analyze.rs`): four non-cancel error exits hard-coded `cancelled: true`,
  mislabeling a model-load / DB-query failure as a user cancel. Now reports the real
  `cancel.load(Relaxed)`.

**macOS engine (PR #24, `swift build` debug+release clean, `swift test` 194/194):**
- **F-A2 heal size corroboration** (`DBWriter.healMovedRow`): rename/move heal matched on `file_ref`
  (st_ino) alone; st_ino has no generation number, so a reused inode could re-bind a deleted row onto
  an unrelated new file and hand it the prior file's tags / named person / OCR. Candidate now also
  requires `size_bytes` match. New regression test (reused inode + different size → fresh row,
  inherits nothing).
- **F-A4 restructure conflicts array**: `apply()` never populated `conflicts` despite the `(2)`
  uniquify path. Now reports each uniquified planned dest; existing test updated.
- **F-A5 restructure mkdir error swallowed**: silent `failed += 1` on `createDirectory` failure →
  added a redacted `restructure_mkdir_failed` warning.
- **F-A6 Deep Analyze empty VLM output**: empty generation → `description=""` →
  `COALESCE(?, vlm_description)` overwrote a prior good caption. Now surfaced as `Inference failed:`
  so the runner's `isFailure` skips the destructive write.
- **F-A7 VisionWorkerPool continuation leak**: a task cancelled while parked in `acquire()` leaked
  its continuation (`withCheckedContinuation` ignores cancellation). Now wrapped in
  `withTaskCancellationHandler`; `acquire()`→Optional, `with()`→`T?`, caller breaks on nil.

**Rejected (verified false/over-stated):** #1 file_ref COALESCE (current behavior correct; the "fix"
would leave a stale ref), #8 ReadStore TOCTOU (read-only double-open, low impact), #9 semantic
containment (PathBuf `.starts_with` is component-aware), #10 empty VLM tags (DELETE is inside
`if !tags.is_empty()`), #12 int width (face counts ≪ INT64_MAX), #13 mtime 1s tolerance (deliberate
FS-precision accommodation).

**On-hardware e2e (isolated /tmp copy of 40 Adlon photos, throwaway HOME, real models symlinked
read-only — corpus never modified):** scan 40/40 (0 failed) → face clustering 87 faces/41 persons →
semantic plan 40 moves → **apply 40/40 applied, 0 failed** → DB consistency check: 40 rows = 40 files
on disk, **0 missing, 0 orphans** (path_text refreshed correctly). The 41-from-87 person
fragmentation is the documented F-4 single-linkage limitation (blocked on labeled data), not a
regression.

## 2026-06-14 (later) — backlog cleared to terminal states (PRs #18–#20 merged; main green)

Continued the finish-everything push after PR #16. Five more PRs landed + merged, all CI-green:
- **#18 — macOS concurrency + flaky-CI fix.** `ReadStore` counters worker uses scoped `withLock`
  (Swift-6 lock-in-async gone); R-07 dedicated shutdown mirror so clustering aborts a fresh shutdown;
  AND the real fix for the intermittent CI hang the restored gate exposed — six tests read an IPCSink
  pipe with a BLOCKING `availableData` on the cooperative pool (parallel runs starved the executor and
  wedged the harness to the 12-min SIGALRM). Replaced with a shared non-blocking `WireCapture` (GCD
  readability handler + lock buffer) and added `swift test --no-parallel` as defense-in-depth. The
  process-spawning `ScanCancellationTests` is skipped on the GitHub runner (local + unit coverage kept).
- **#19 — restructure tiers + honest apply bar.** Engine `RestructurePlan` now populates per-move
  `tier` + `folderClassifications` (engine-authoritative Tidy/Keep); the vestigial two-step
  "shortcuts → convert" apply bar (both buttons did the same real-move confirmation; "reversible" copy
  mislabeled an irreversible move) collapsed to one "Apply moves" button.
- **#20 — Windows `hardwareReprobed`** reports the memoized `active_provider()` (actually-bound EP),
  not a fresh probe, so a post-install "Verify" shows "pack ✓ — restart to use it".

**Net:** every code-actionable backlog item is landed (this session also: containment fix, both-platform
apply-cancel, test-gate integrity, Deep Analyze verification). What remains in NEXT.md is exclusively
blocked on a specific resource (hand-labeled face data for F-4; the RTX 2060 for per-vendor GPU; the
owner's Mac/GUI for semantic search + Tidy/Keep render + `walkStreaming` UX) or deliberately deferred
because a blind change would regress a working path (R-11, in-process cancel test, the intentional
face-embedding backfill cap). `main` is the only branch; all GitHub workflows green.

## 2026-06-14 — Xcode-unblocked: Deep Analyze verified, cross-platform apply-cancel fixed, macOS test gate restored (PR #16 merged)

Xcode 26.5 was installed on the dev Mac, lifting the "no `swift test` / no Metal" ceiling. That
immediately surfaced and fixed three real defects and verified the last blocked feature.

**Bugs found + fixed (PR #16, both platforms, all CI-green):**
- **Restructure apply was uncancellable on BOTH platforms.** The F-C6-013 cooperative cancel loop
  existed but neither dispatcher ever set the flag — Windows built a fresh never-set `AtomicBool`
  (`with_cancel` was test-only); macOS ran the apply in a discarded `Task.detached` while `cancelScan`
  used a different mechanism (`ScanCoordinator`). A long apply on a large library could not be stopped.
  Wired `CancelScan`/`cancelScan` → the apply on both sides, with no stale-cancel (fresh apply = fresh
  signal). Deterministic tests added (`apply_dispatch_honors_preset_cancel`,
  `requestCancelCancelsRestructureTask`).
- **The macOS `EngineTests` target hadn't compiled since campaign commit `976a248`** — `Database`
  ambiguous (GRDB vs FileIDEngine) in 11 files, a stale `dbModified:` label, a Swift-6 capture, plus
  never-run tests with latent setup bugs (a `/private` vs `realpath` root mismatch, the R-14 CLIP
  carve-out, and an impossible `cache_spill == 0` assertion). Repaired → **192 tests, 48 suites, green**
  — the first time the macOS suite has actually run.
- **The CI `swift test` step never failed the job.** `if ! cmd; then status=$?` captured the negated
  condition's status (always 0), so a failing OR non-compiling suite reported exit 0 — the macOS test
  gate had silently never failed. Fixed to capture the real exit code. (This immediately caught a
  pre-existing CI-only hang in the process-spawning `ScanCancellationTests` — a leaked engine child
  wedging the harness; after collector/watchdog/stdout-drain hardening it's skipped on the GitHub
  runner and runs on every local `swift test`. Tracked in NEXT for an in-process rewrite.)

**Deep Analyze (MLX VLM) verified end-to-end on-hardware** (the last previously-blocked feature):
placed the cached `mlx.metallib` next to the engine, downloaded Qwen3-VL 4B (3.5 GB). `deepAnalyzeFile`
produced an accurate caption ("A smiling boy in a white Nike shirt hugs a happy, fluffy dog…") + a
smart-rename (`boy_hugging_dog_wooden_background`) in 8.5 s; `deepAnalyzeFolder` captioned 3/3 images;
a corrupt non-image stub was gracefully skipped (processed=0, failed=0, no crash). Caption quality is
genuinely good.

## 2026-06-13 (latest) — on-hardware write-path + perf verification loop; 1 bug found+fixed (PR #13 merged)

Drove the **real release engine** against isolated throwaway sandboxes (`HOME`/`CFFIXED_USER_HOME`
override → disposable DB; synthetic `/tmp` libraries) and the read-only Adlon/TrueNAS corpus, to
exercise every path the earlier campaign couldn't on hardware. No corpus data or the user's library DB
was ever modified.

**Bug found + fixed (merged to main, PR #13, commit `b927031`):** the restructure-apply SEC-7
containment guard (`pathIsContained`) rejected **valid in-root moves** when the library root resolved
through macOS's `/private` shortening. `resolvingSymlinksInPath()` strips `/private` only when the path
EXISTS, so the (existing) root canonicalized to `/tmp/…` while a not-yet-created destination parent
stayed `/private/tmp/…`, breaking the prefix check — every move failed as "escapes_root". Latent in
production (real libraries live under `/Users/…`, never `/private`) but a real hole in a security guard.
Fixed by resolving symlinks against the deepest EXISTING ancestor then re-appending the literal tail;
the escape vector stays closed (true-escape + existing-symlink-escape both still rejected, unit cover
added in `PathContainmentTests`). After the fix the isolated apply run reports `applied=2 failed=2`:
both valid moves land (D-7 → `shared (2).jpg`), `path_text`+`path_hash` refreshed, the stale-plan move
and the `../` escape both fail with files untouched.

**Verified working on-hardware (no change needed):**
- **Restructure apply** — the one write-path never exercised on hardware: D-7 auto-rename, B4
  stale-plan guard, SEC-7 escape rejection, `path_hash` refresh — all confirmed end-to-end.
- **Incremental skip-set** — rescan of the already-scanned 9,745-file Bernadine/CD processed **0 files
  in 2.95 s** (vs 156 s full): ~53× rescan speedup, the unchanged files skipped at discovery.
- **Rename/move heal (F-2)** — rename `foo.jpg→bar.jpg` then rescan: same row id re-bound to the new
  path, attached tag preserved, no duplicate row, `rename_heal` logged.
- **Garbage-file robustness** — the 10 perpetual `image_decode_failed` files on Bernadine/CD are not
  JPEGs at all (UTF-16LE `\\?\C:\Users\…` Windows path stubs / cloud placeholders with `.JPG`
  extensions); the engine fails them gracefully and continues, no crash. Source-data artifact, not a bug.
- **Graceful mid-scan cancel** — cancel during tagging returns a prompt terminal `scanComplete` (178
  files persisted, 0 failed), no crash, no hang.
- **People pipeline / face-embedding backfill** — ran clustering once on the real DB: ArcFace/SFace
  embeddings went `533 → 5533` (exactly +5000 = `maxExtractionsPerRun`), clustered into 947 persons,
  `unmatchedFaces=0`, 52.6 s, no crash. Confirms the lazy backfill (`extractPendingPrints`) embeds a
  fresh window per run and clustering is stable. The DB has 46,079 detected faces (all with Vision
  `print_data`); embedding is the same incremental-per-run backfill as CLIP, so a large library needs
  several clustering passes to fully populate People — by design (`maxFacesPerRun`/`maxExtractionsPerRun`
  caps), documented as a UX item in NEXT, not a bug.
- **DB health** — `PRAGMA integrity_check` = ok and `foreign_key_check` empty after all the scans,
  clustering, apply, and heal operations (59,998 files, 50,518 phashes, 947 persons). **phash/dedup
  foundation** confirmed: byte-identical images get identical phash, a different image a different one.

**Perf assessment (M1 Pro):** decode is already capped at 512 px (`CGImageSourceCreateThumbnailAtIndex`,
EXIF from the same source); CLIP + faces run on the CoreML EP; first-scan CPU is genuinely maxed
(~875 %/1000 %) doing real pipeline work across 14 workers — near-optimal for this tier. The remaining
levers (faces ANE vs CPU+GPU compute-units, model size) are correctness-sensitive and stay ML-UAT
items, not blind flips. Adaptive scaling (PR #13) gives bigger machines headroom without touching the
M1 baseline.

## 2026-06-13 (later) — macOS adaptive hardware scaling (branch `perf/adaptive-scaling-2026-06-13`, commit `f80d768`)

The macOS engine was using M1-Pro-shaped constants for the two stages that should
scale with the machine. Now they're hardware-derived, with the M1 Pro tier left
byte-identical so the verified baseline is untouched:
- `VisionWorker.visionConcurrencyGate`: hardcoded `14` → `Hardware.workerCap`
  (M1 Pro 14; M-Ultra 32) so the Vision/ANE stage scales with cores instead of
  silently feeding a bigger ANE only 14-wide.
- `Hardware.MemoryTier` (low `<12`/balanced `12–48`/high `≥48` GB) mirroring the
  Windows `memory_tier`, driving `DBWriter.maxBatchFiles` (64/100/500). M1 16 GB →
  balanced → 100 (the proven value, unchanged).

On the dev M1 Pro all three reduce to the prior constants — the box is already
CPU-maxed (~875% of 1000% avg during a scan), so the headroom only manifests on
bigger silicon and is **unmeasured here**. Per-chip tuning recipe (gate, ANE
semaphore, worker-cap storage-bound caveat, batch/RSS) is in NEXT.md
(`2026-06-13 (later)`). `swift build` debug + release clean. Runtime memory-pressure
adaptation (F-3) is still spec-only; these tiers are static at startup.

## 2026-06-13 — "audit-2026-06-10" perfection campaign: 131 findings + 2 criticals fixed, 22 self-introduced regressions caught, clean on-hardware macOS scan (branch `fix/audit-2026-06-10`)

The deepest adversarial pass yet — run cross-platform off `main`, closed on this branch.

**Method.** Three adversarial audit workflows (WF-1 unit-correctness, WF-2 cross-platform parity,
WF-3 perf/memory-adaptive) → triage → **7 fix waves** → a **delta re-audit loop (2 rounds)** over
the campaign's own diff. **252 raw findings → 131 fixable + 15 rejected** (each rejection a
deliberate guard or a cited prior ruling). Full record in `shared/docs/audit-2026-06-10/`
(`findings.json`, `TRIAGE.md`, `reaudit-confirmed.json`).

**What landed, by wave:**
- **Wave R — 30 Rust-local engine fixes** (the FIX-LOCAL set, clippy/test-verifiable here):
  EP-guard breadcrumb clear + revision-keyed install sentinel, terminal phase/IPC events never
  droppable + outbound frame cap, trash restore-conflict + batch enumeration + recovery sidecar,
  anchor-strip exempting the semantic butler's moves, Deep Analyze CPU-EP honor / target scope /
  skip semantics, SleepGuard same-thread release, redaction home-username mask, COM apartment
  scoping, pptx member cap, ranged-resume progress, watchdog PID-reuse, parallelized discovery
  syscalls.
- **C2 — IPC contract parity, schema-first.** Every change landed in `ipc.schema.json` first, then
  mirrored across all four DTO targets (Rust → C# → Swift): optional `deepAnalyzeAll` fields,
  `cancelPrewarm.modelKind`, canonical error-kind vocabulary, Windows DA eta/path,
  `discoveryComplete` backstop, v16 `path_search` NFC symmetry.
- **C3 — 44 macOS-engine fixes**, including a **CRITICAL gate-trio**: an ungated `DELETE` meant a
  Vision/OCR/face **timeout silently wiped a file's tags / `person_id` / OCR** (now every
  destructive re-write is gated on the stage having actually run). Also the **face-clustering
  data-loss + determinism cluster** (port of the Windows S0 snapshot-under-lock, fixed-seed HNSW,
  `is_unknown`/verdict guards, union-find de-chaining) and the **butler restructure ENGINE port**
  (path_hash, B4 stale-plan guard, uniquify, sanitize, tiering, Windows-canonical naming).
- **C4 — 21 macOS-app fixes**, including a **CRITICAL dual-writer**: "Restart Engine" spawned a new
  engine without reaping the old one → two writers on one DB (now terminate-and-reap before spawn).
  Plus restructure single-flight apply, off-main merge/search, read-conn UAF guard, Deep Analyze
  staleness, model-download progress reset.
- **C5 — 12 Windows C# fixes**: read-conn drain dispose, single-flight apply, bulk-tag confirm+undo,
  selection/rename identity, Sankey single-parse, log-lock, CUDA toggle.
- **C6 — perf**: macOS discovery-time incremental skip-set (activated), decoupled DB commit, cached
  statements, streamed (bounded-memory) semantic search, Vision autoreleasepool, cancellable +
  progress-emitting restructure apply (both platforms).
- **Feature wave** — **F-2** macOS rename-heal (moved/renamed files keep tags/faces/OCR via APFS
  inode `file_ref`, old-path-gone-gated) and **F-C3-021-app** (route the macOS Restructure tab
  through the engine butler, retire the app-side classifier).

**The delta re-audit earned its keep.** Round 1 found + fixed **22 self-introduced regressions**
(`reaudit-confirmed.json`, R-01..R-22), incl. 2 HIGH: face clustering became a **silent no-op after
any scan-cancel** (a sticky scan-scoped cancel mirror gated the standalone cluster job), and the new
discovery **skip-set defeated the orphan sweep** (skipped files retained a stale `scanned_at`,
saturating the 5000-row prune-candidate cap → real orphans never pruned). Round 2 of the re-audit
loop is in progress.

**Local gates at HEAD:** `cargo clippy --all-targets -D warnings` clean · `cargo test` **336
passing** (was 292 baseline; **+44 regression tests**) · `swift build` debug + release clean (no
Xcode locally → `swift test` is CI-only). **CI green on all three workflows** (`macos.yml`,
`windows-engine.yml`, `windows-app.yml`) on the branch through the C6 batch.

**On-hardware macOS verification** (owner's external drive `Adlon`, `/Volumes/Adlon/TrueNAS`,
**62,746 files, READ-ONLY** — no permanent changes to corpus data):
- Full scan **clean**: 59,633 processed / 54 failed (**0.09 %**, all genuine "Could not decode image"
  on corrupt backup images), **~120 files/s** (Vision-only — no GPU models on this box), **peak RSS
  1,187 MB** (< 1.5 GB target), no crashes. Produced 307,666 tags + 26,678 files-with-faces.
- Incremental skip-set verified: rescan of a 311-file folder processed **0 files in 9.5 ms**.
- Restructure butler plan produced **13,131 moves** (photo/video/audio + GPS Places +
  `Documents/<year>`, Windows-canonical naming) with no crash.
- Mid-scan cancel terminated promptly with final status written, **no hang**.
- **Apply/move write-paths were NOT run on hardware** (would move corpus files) — covered by macOS
  unit tests + the proven Windows port. The on-Mac apply UAT is the remaining macOS gate (NEXT.md).

## 2026-06-10 — Production-readiness campaign closed: zero open findings, perf sweep, hardening live, T4/T5/T7 shipped (branch `fix/bug-audit-sweep`)

The full campaign that started with the 2026-06-09 sweep is **closed with zero open confirmed
findings** across every record. What landed on top of the entry below:

- **Merge reconciliation** — origin/main's 123-commit line (commercial-clean stack, its own
  audits) merged into the branch's 19-commit fix line; R0 merge-interaction review found + fixed
  7 semantic conflicts git couldn't see (`R0-findings.json`).
- **Deferred findings closed** — L1 (schema-conformance suites lock Rust + C# mirrors to
  `ipc.schema.json`; zero drift) and U4 (macOS IPC wire dup'ed off fd 2; MLX/Metal diagnostics
  go to `engine-stderr.log`, wire stays pure JSON — pinned by iterate.sh).
- **Security hardening live** — SHA256 manifest (`shared/models/manifest.json`, registry-locked
  both platforms, VLMs revision-pinned with sentinel), TLS CA-allowlist pinning (11 roots +
  rotation runbook in SECURITY.md), tokenizer DoS bounds. SECURITY.md rewritten to match.
- **macOS features** — T4 Finder-tag undo journal + tile tag dots, T5 bulk-rename perf hoist,
  T7 sign/notarize/DMG pipeline (`scripts/release.sh`; `--skip-notarize` dry run produces a
  92 MB hardened-runtime DMG that passes `codesign --verify` + DR check; real signing is
  owner-gated on the Developer ID cert).
- **Audit Sweep A** (R1–R6 fix-regression + N1–N3/N5–N8 depth lenses): 16 confirmed + 16 lows,
  all adversarially verified, all fixed — incl. the **v14 migration fork** (C12: chains
  re-unified, canonical 16-id list pinned by tests on both platforms), the C14 char-boundary
  truncate panic, NFC-insensitive search (v16_path_search), StablePathHash (SipHash-1-3, shared
  vectors), staging-orphan sweeps, byte-weighted predecode budget, video-thumb semaphore, and
  the L7 newer-DB downgrade guard (`db_newer_than_engine`, both engines).
- **Perf sweep** — 35 candidates from 6 lenses, 27 adversarially refuted, 6 landed (`b653d00`):
  EXIF read off the already-open CGImageSource (2–5 ms/image × 14–32 workers on NAS), COUNT(*)
  badge queries, single-pass restructure stats, arcface clone removal. 2 unproven candidates
  dropped on record (DECISIONS.md).
- **Closing Sweep B** (P1–P5 parity, N4 robustness, R7 delta, loop-until-dry): round 1 → 9
  confirmed fixed (deep-analyze/cluster duplicate-command parity, scanComplete-on-cancel,
  discoveryComplete, face_prints.excluded population, rankByCosine failed-filter, redaction
  parity ×2, sanitizer delegation, Apply default arm); round 2 → 7 fixed (cancelled-scan counts,
  **Windows queueState finally wired** — the SidebarQueueList was bound to an event the engine
  never emitted, command_decode_failed parity, /Volumes redaction both sides, PAR-111-mirror
  busy-bounce exemption on macOS); round 3 → **completely dry** (0 candidates, 22 verifiedClean).
  Record: `audit-2026-06-09-merge/sweep-b-findings.json`.

**CI epilogue:** the macOS workflow's first real execution of the C1 process suite (yesterday's
"green" run had silently never started it) caught one final engine bug — the shutdown IPC
command never exited the engine (`break` inside the command loop's switch broke the SWITCH;
stdin EOF was the only real exit, which the app's pipe-close masked). Fixed with a labeled
break (verified: 0.13 s exit on the shutdown frame with stdin held open), the C1 test harness
made un-hangable (a blocking waitUntilExit inside a task group had turned a slow exit into a
60-min CI hang), and macos.yml now caps `swift test` at 12 minutes with a survivor-process dump.

Local gates at HEAD: `swift build` clean 0 · `cargo clippy --all-targets -D warnings` 0 ·
`cargo test` 292 green · release dry-run DMG green · **CI green all three workflows** (macOS
92/92 incl. the C1 suite; Windows engine x64+arm64; Windows app). **Hardware UAT is the
remaining gate** — see NEXT.md for the checklist. "Zero known bugs" = every recorded finding
closed/accepted + gates green + UAT clean; the C#/.NET side and all runtime behavior verify in
CI/on-hardware only.

## 2026-06-09 — Full cross-platform bug-audit sweep (branch `fix/bug-audit-sweep`)

Ran a read-only multi-agent static audit across macOS (Swift), the Windows Rust engine, and the
Windows .NET app (18 scoped finders + adversarial verifiers): **88 raw → 73 confirmed** (2
critical, 13 high, 28 medium, 30 low) + 4 uncertain. Remediated **72 of 73 confirmed + 3 of 4
uncertain** on this branch (≈30 atomic commits); the lone deferral is the IPC ID-casing drift
(L1 — no runtime impact, needs a coordinated Windows-verified wire rename; see DECISIONS.md).

Highlights:
- **macOS (Swift)** — fixed: cancel/shutdown-during-scan **deadlock** (unbuffered AsyncChannel
  producer never cancelled); `INSERT OR REPLACE` rowid churn that cascade-deleted faces/embeddings/
  **manual person assignments** on every re-scan (now an id-preserving UPSERT + v12 FTS-sync
  triggers + change-detection skip); IPCSink progress-coalescing clobbering `scanComplete`;
  VisionWorker reused-VNRequest race; FTS5 MATCH injection-to-zero-results; person/FTS
  reconciliation on delete; rename apply/undo disk↔DB consistency; CLIPTextEncoder UI-thread
  freeze; download integrity (error propagation, size verify, no double-resume); +others.
- **Windows Rust** — fixed: restructure **data-loss** overwrite (now non-overwriting +
  disambiguation); pause→resume lost-wakeup **deadlock**; image-decode **OOM**; ArcFace **BGR→RGB**
  (cross-platform parity); VLM stderr-pipe **hang**; OCR-never-runs (uninit COM apartment);
  `planRestructure` dead SQL (illegal `GROUP_CONCAT(DISTINCT,sep)`); range-downloader permit
  **deadlock** + 416/stale-part recovery; per-download cancel registry; zip-bomb actual-bytes
  cap; ADS `:` rename guard; long-path moves; +others.
- **Windows .NET** — fixed: WinVerifyTrust egress/UI-block/handle-leak (cache-only revocation,
  off-thread); AppSettings split-brain (single canonical instance); OnProcessExited exit-code
  race; expected-exit latch; `Local\` single-instance (multi-user); install watchdog null
  dispatcher; path-redaction gaps (UNC/space/sibling-username); search debounce ODE; failed-file
  filter consistency; ReadStore leak; Sankey O(N²)→O(N) + debounce; People virtualized
  checkboxes; +others.

**Verification status:** macOS `swift build` (app + engine) is **green**; the swift-testing suite
can't run in this env (no Xcode — CommandLineTools only). Windows: the non-`cfg(windows)` Rust
**`cargo check` is green** (cfg(windows) code + .NET unverifiable on macOS). All Windows build/run
verification and the macOS UAT are pending on the user's hardware — see NEXT.md.

## 2026-06-04 (latest) — Six-workflow deep bug+perf audit: ~35 bugs fixed + 4 self-introduced regressions caught by re-audit (UNCOMMITTED on `main`)

Maximum-coverage adversarial sweep of the whole Windows app+engine off `main` (built on the prior uncommitted sweep). **Six serialized workflows** (concurrent fan-outs trip a server rate-limit → must serialize): (1) engine deep-correctness — 15 subsystems × 3-skeptic refute-by-default verify (18 confirmed); (2) app deep-correctness — 10 areas, UI-thread/async/lifecycle/leak lens (16 confirmed); (3) perf/memory/4 GB-target (7 confirmed); (4) security/data-integrity/concurrency (3 confirmed + the contested rechecks); (5) **re-audit of the fix diff** — caught 4 regressions MY fixes introduced; (6) focused re-audit of the regression repairs — clean. ~270 finder/verifier agents total. Per-finding record: `shared/docs/audit-2026-06-04c/` (engine/app/perf/sec + both re-audits + TRIAGE.md).

**~35 distinct fixes** (≈21 engine/perf/sec + ≈14 app), each batch re-greened. **Gates green:** engine `cargo clippy --all-targets -D` + `cargo test` (all pass; +3 new tests: HNSW determinism, anchor-strip ×2); app `dotnet build` 0/0 + App.Tests + IpcSchema.Tests + `dotnet format`. **NOT committed/pushed** (owner's call) — see `git diff` (39 files; this sweep + the prior uncommitted one).

**HIGH (data-loss / crash / hang):**
- **Face-clustering phase-3 DELETE+re-INSERT silently discarded People-tab edits** (rename/merge/mark-unknown) committed during its lock-free phase-2 window → permanent identity-edit loss. Fix: read the identity snapshot in phase 3 *under the persist lock* (not phase 1), so a concurrent edit is carried forward.
- **HNSW built with an entropy seed** → face clustering nondeterministic on >5k-face libraries (People identities/names hopped on every rescan). Fix: fixed `.seed()` + determinism test.
- **`EngineClient.ReadBoundedFrameAsync` O(n²)** (empirically 132 s for a 4 MiB frame) → multi-minute hang decoding a large `restructurePlan`. Fix: incremental scan offset + flat-chunk newline scan → O(n).
- **Restructure "Keep"/Anchor moves silently applied** despite the UI promising those folders stay untouched (Windows `classify()` always emits a canonical destination; macOS emits no proposals for anchor folders). Fix: engine drops Anchor-folder moves from the plan after counting (Keep tile count preserved) + tests.
- **Mid-scan GPU device-removal left image rows `failed=false`** → permanently stranded in the incremental skip-set. Fix: mark image/video failed when the GPU dies mid-ML.
- **`ModelInstallerService` raised PropertyChanged off the UI thread** (RPC_E_WRONG_THREAD class) → marshal via captured `_ui`.

**MED highlights:** GPU-death dropped Audio/Doc CPU tags (persist them); empty/rescan notice racily dropped on a <250 ms scan (single-shot guard + post-drain fallback); cancel couldn't interrupt an in-flight VLM request (select! on cancel); EP-variant chosen from override-blind `active_provider()` while BGE pins CPU (resolve for the bound EP); ep_guard `.ep_attempt` breadcrumb race (now an armed-EP *set*); WordPiece ASCII-only lowercasing → non-ASCII `[UNK]`; BGE pooling OOB panic on a malformed ONNX (bounds-validate); downloader http-downgrade redirect (https-only) + orphaned `.part` sweep; OCR missing COM init (silently produced nothing — it was the one shell module without `CoInitializeEx`); applyTags COM/sidecar writes + face-crop JPEG encode moved OFF the SQLite writer lock; Library refresh races (generation guard + in-flight counter); per-request thumbnail cancellation; ReadStore + 2 SqliteConnection leaks; FilePreview stale-nav guard; revertMerge wrong `file_count`.

**The re-audit earned its keep — caught 4 regressions in my own fixes (all repaired, gates re-green):** the async `DebugLog` sink lost the last <200 ms of forensic lines on a native fast-fail → **reverted to synchronous** (durability is load-bearing per CLAUDE.md; the perf opt needs a durable-async design); ep_guard's "first-arm-wins" breadcrumb recorded the WRONG EP under heterogeneous concurrent binds → **armed-EP-set breadcrumb** (disables every in-flight guarded EP on a stale crumb — over-disabling is recoverable, a crash-loop is not); the FilePreview stale-nav guard fell through to an unguarded `ShowPlaceholder` that clobbered the current sibling → guard the fall-through; the smart-rename pill reset was undone by a stale `DeepAnalyzeLast` → clear it on run-start.

**Deferred (real, documented, out of this pass — see NEXT.md / TRIAGE.md):** VLM server-death mid-batch CLI fallback; CLIP-tokenizer punctuation (ML-quality A/B); long-path trash manifest (build change); wipe-vs-bulk-handler interlock (benign, deadlock-risk); applyRestructure outbound chunking (narrow, file-move-path risk); AppSettings lost-update (settings-refactor); Sankey "Other" drill-down; startup-auth-on-UI-thread (contested); rename-heal `UPDATE OR REPLACE` FTS desync (narrow, FTS-schema risk). On-hardware verification (RTX 2060 / 4 GB DirectML) remains the gate for the runtime/GPU/COM paths.

## 2026-06-04 (later) — Five-workflow bug-audit sweep: 11 bugs fixed + 1 fix-introduced hang caught by the re-audit (UNCOMMITTED on `main`)

Exhaustive adversarial audit of the whole Windows app off `main`: four parallel find→refute-by-default→verify workflows (engine safety/concurrency/DB · app UI-thread/async/lifecycle/IPC · IPC-contract+perf · recent-diff regression), then a fifth refute-by-default RE-AUDIT + completeness-critic over the fix diff (~50 finder/verifier agents; the workflows had to be **serialized** — 4 concurrent tripped a server rate-limit that aborted every finder mid-task). **11 distinct confirmed bugs fixed; the re-audit caught a hang one of the fixes introduced (fixed + regression-tested).** Gate re-green: engine clippy `-D` + **267 tests** (+1 E4 test) + fmt; app build 0/0 + **131 App.Tests** + **38 IpcSchema.Tests** + format. **NOT committed/pushed** (owner's call) — 10 files (4 app + 6 engine). Per-finding record: `shared/docs/audit-2026-06-04b/`.

**HIGH (app, crash/hang):** People + Cleanup `RefreshAsync` raised `IsLoading`/`ErrorMessage` from the `ConfigureAwait(false)` thread-pool continuation → x:Bind drove `ProgressRing.IsActive`/`StatusText` off the UI thread → RPC_E_WRONG_THREAD native fast-fail on every People/Cleanup refresh (the V15.x DispatcherObject class) → `OnUi()` marshal mirroring LibraryViewModel. `EngineClient.OnProcessExited` could tear down a freshly-respawned engine when the OLD process's queued `Exited` ran after `StartAsync` reinstalled the fields (RestartAsync race) → `sender != _process` stale-exit guard. `restructurePlan` > 1 MiB (~3.5k moves) was silently dropped by the C# read-frame cap → empty Restructure tab on a large library → cap 1→32 MiB + a visible `ipc_frame_too_large` error on any oversize drop.

**MED:** `wipeLibrary` didn't interlock against the now-lock-free face-clustering PHASE-2 → a wipe could be followed by the persist re-inserting phantom `persons` (ghost People cards after a "wipe") → wipe waits on `face_cluster_active`. The new `face_clustering_busy` kind collided with the app's `Contains("cluster")` gate-release → wrongly cleared the auto-cluster single-flight on a busy bounce → exact-match `== "face_clustering_failed"`. SEC-5 junction-TOCTOU (`has_reparse_point_in_chain`) compared a raw parent vs a verbatim `\\?\` root → the ancestor walk broke after the leaf → normalize both via `strip_extended_length` (NOT canonicalize, which follows the junction).

**LOW:** single-file Deep Analyze reported a genuine failure as `cancelled:true` (suppressed the warning) → derive from the cancel flag; restructure new-group folders deduped on the pre-sanitized name → dedup on the sanitized name; merge-suggestions sheet flashed "No likely merges" over "Looking…" → drop the null-reset; BGE text encoder ran single-threaded on CPU on a GPU box (CPU-pinned but inherited the GPU EP's intra=1) → force CPU `p_cores`.

**Re-audit catch (the point of the fifth workflow):** the E4 sanitized-dedup `while` loop could spin forever when a group base name sanitized to ≥~200 chars (every `"{base} {n}"` truncates to the same string) — a NEW hang the fix introduced, invisible to clippy/tests. Fixed by reserving suffix room so each candidate is distinct + bounded; added the `sanitization_colliding_group_names_get_distinct_folders` regression test. The two app-thread HIGHs and the wipe race are the headline user-facing wins.

## 2026-06-04 — Suggested-merges hang fix + over-split tuning + exhaustive perf audit (branch win-face-fix-perf)

Built on `origin/main` (PR #10). Two bodies of work, headless-green, ready to merge to `main` (the push is the owner's).

**Face — suggested-merges hang + over-split (implements `shared/docs/PLAN-suggested-faces-fix.md`).** The People → Suggested-merges sheet hung for minutes because the engine's single `Arc<Mutex<Connection>>` serialized the read-only suggestion query behind the multi-minute clustering write-lock. Fixes: `db::open_read()` (ephemeral `SQLITE_OPEN_READ_ONLY` conn; `handle_find_merge_suggestions` opens its own read conn instead of `db.lock()`); `handle_run_face_clustering` restructured into load (lock) → `cluster()`+`consolidate()` (LOCK-FREE) → persist (re-lock), so the writer mutex is free during the multi-second compute; an engine-side single-flight guard (`face_cluster_active`) bounces a concurrent run; app `WaitForMergeSuggestionsAsync(30s)` with an actionable timeout; auto-cluster dropped on user-Cancel. Over-split: `AUTOMERGE_COS_DEFAULT` 0.85→0.75 (Balanced, env-overridable), the 12k-cluster consolidate no-op replaced by an HNSW centroid neighbor search (cap lifted, brute-parity test), Pass-3 floors exposed as `FILEID_FACE_PASS3_*` env knobs.

**Perf — exhaustive audit for the 4 GB / low-mem target.** 15-finder read-only audit over the whole Windows tree → refute-by-default verify → synthesize (33 confirmed / 7 refuted; the C# list-virtualization + LavaLamp/Win2D dimensions fully refuted — no waste). 21 safe (headless-verified) + 9 hardware-sensitive (applied conservatively, GATED so the 6 GB RTX 2060 reference box is byte-identical) findings applied. Safe highlights: borrowed-view RGB resize drops a full-frame clone per image on the primary RAM++ tagger + CLIP; three SQLite reads moved off the UI thread (fake-async M.D.Sqlite); SemanticSearch top-K lazy materialization; prepared-statement + VRAM/EP-probe caching; HNSW query-buffer reuse; shared thumbnail-cache key; IpcCoder span decode; query-embedding LRU. Hardware-sensitive (pending on-hardware confirmation): memory_tier wired into worker_count/pool/predecode (Low-tier only), VRAM-probe-None fails safe to pool=1, vision semaphore vision_cap=1 only at pool=1, BGE pinned to CPU EP, downloader streaming concat.

**Gates:** engine `cargo clippy --all-targets -D warnings` + 266 tests; app `dotnet build` (WinUI) 0 warnings + 131 App.Tests + 38 IpcSchema.Tests + `dotnet format`. Commits `d7b0159f` (face) + `c07f93e8` (perf). The obsolete local `windows-v16.22-v16.26` branch (RAM++ ONNX + drop Qwen-3B) is superseded by origin/main and dropped (see DECISIONS). On-hardware verification of the hardware-sensitive perf knobs + the 0.75 automerge default remains the owner's gate.

## 2026-06-04 — Face scanning "totally broken" root-caused + fixed (3-workflow audit → gap-verify → re-audit)

On-hardware report (RTX 2060): face scanning totally broken, "WAY too many similar faces",
suggested-merges too slow, and a `clip_text` install-stall toast. Three adversarial workflows: a full
face-pipeline audit (8 finders → refute-by-default verify → completeness critic; 34 findings, 17
confirmed, 2 blockers), a gap-verify of the 8 critic suspects (7 refuted — incl. an EMPIRICAL load of
the on-disk YuNet ONNX proving its 12 output names match `yunet.rs` and the decode math is OpenCV-exact,
so faces detect/embed/cluster correctly), and a re-audit of the fix diff (16 findings → 3 confirmed →
all fixed). **ROOT CAUSE (blocker): `scan.rs` hard-gated EVERY scan on the `clip_text` sentinel, but
`clip_text` (the CLIP *text* encoder) is query-time-only and never used by the scan/face chain — so the
user's stalled `clip_text` install (the toast) aborted ALL scanning with `models_not_installed` → zero
faces.** Removed it from the gate (`[mobileclip_s2, arcface]` only).

**Engine fixes:** clip_text gate (above); ABORT the scan when a pre-flight-required model passed its
sentinel but failed to LOAD (was warn-only → a corrupt/AV-quarantined model stamped every file
scanned-but-faceless and the timestamp-only incremental skip-set then stranded them forever); on a
mid-scan GPU TDR mark only image/video rows `failed=true` so they retry (docs already CPU-processed stay
visible); new verification-aware centroid auto-merge `consolidate()` (default 0.85, env
`FILEID_FACE_AUTOMERGE_COS`, `=1.0` disables) folding over-split duplicate clusters — blocked by BOTH
"different people" verdicts AND differing user names (stable across re-scan); merge-suggestion band
retuned `0.32..0.66 → 0.55..0.97` (drops impostor noise, surfaces stranded same-person fragments);
suggestion sweep releases the writer lock before its O(P²) compute; YuNet output-name contract checked
at load (loud fail vs silent zero-faces); orphaned face-crop JPEGs pruned post-commit on re-scan;
downloader `read_timeout` 120→60s so a stalled install self-heals before the alarm.

**App fixes:** install stall-guard now latches THIS kind's terminal (`Fraction >= 1.0`) via a
PropertyChanged subscription — fixes the false "clip_text stopped responding" toast under Install-All
(the shared progress slot was overwritten by other concurrent downloads); `PrewarmNoProgressTimeout`
90→120s; auto-clustering also fires on Failed/Cancelled scans (faces persisted before a non-Complete
terminal now surface); People grid hides `is_unknown` clusters (matches macOS; makes "mark as unknown"
actually prune); People Re-cluster awaits engine readiness + logs aborts (was a silent no-op).

Headless-green: engine clippy `-D` + **264 tests**; app build 0/0 + **App.Tests 108** + **IpcSchema.Tests
34** + format. Branch `win-face-cluster-merge-perf-2026-06-03`. Clustering thresholds + the 0.85
auto-merge need on-hardware calibration on the labeled `G:\TrueNAS` library (over-split philosophy
unchanged; auto-merge is conservative + env-disable). Deferred items (RAW decode, rotated video,
consolidate 12k cap, suggestions HNSW, content-keyed verifications) in NEXT.md.

## 2026-06-03 — Full-repo Windows bug audit (4 workflows) + production-hardening fix pass

Exhaustive adversarial audit of the whole Windows app (Rust engine + WinUI) via four
find→refute-by-default workflows: **78 confirmed bugs** (~70 distinct; verifiers rejected ~40 false
positives). Full inventory + per-item file:line + fix-status in [`AUDIT-2026-06-03.md`](AUDIT-2026-06-03.md).
Fixed the high-confidence set, THEN drove a fix-all workflow (8 file-disjoint cells) + a hand-built
IPC-contract change to close EVERY remaining deferred item, THEN ran a 3-pass refute-by-default
RE-AUDIT loop (5 → 7 → 1 confirmed) that caught 13 fix-introduced regressions — incl. a
`tags_evaluated` decode-failure/online-only gap, an off-UI-thread `IsLoading` write in Find-Similar, a
masked orphaned-test break, and a cancel that wedged the install slot — all fixed. ~70 distinct bugs
addressed. Headless-gate-green: engine clippy `-D` + fmt + **258 tests**; app build 0/0 + **App.Tests
108** + **IpcSchema.Tests 34** + format. Branch `win-prod-hardening-2026-06-03`, NOT yet merged —
review the branch. The user's flicker report is fully diagnosed + fixed (see below).

**HIGH fixed (engine):** face-clustering wiped every user-assigned name on every scan (snapshot +
member-majority re-attach); timeout/GPU-dead row wiped a file's auto-tags (added `tags_evaluated`
gate, mirroring faces/OCR/doc); restructure move/symlink missing `\\?\` long-path prefix; VLM CLI
stderr piped-not-drained deadlock; `cpu` EP override silently ignored (TDR-recovery escape);
`file_ref` cross-volume MFT collision collapsed two files into one row (heal now requires
old-path-gone for ALL matches); CLIP-text query bound a GPU EP outside the ep_guard window
(crash-loop); wipe-during-scan interleave (engine cancels+waits before truncate).
**HIGH fixed (app):** Library search wrote XAML off the UI thread (fast-fail) → `OnUi` marshal; Deep
Analyze stale `Complete` fought the live UI at 4 Hz on 2nd+ run → cleared on Starting + scan start;
`ModelInstaller.Reset` omitted RamPlus/Accelerator → stuck-spinner.
**MEDIUM/perf fixed:** downloader 200-vs-206 resume corruption + corrupt-part cleanup; rename-heal
LIMIT-1 orphan; pipeline strip blanked-to-grey on completion + 10 Hz redundant redraw + filled-dot
stroke; `ReadStore.RecentAsync` missing `failed=0`; WinVerifyTrust state-handle leak per spawn;
FilePreview rename silent-failure; HNSW per-query O(n²) scratch re-alloc (reusable `Searcher`);
brute-force kNN full-sort → bounded top-k; ram_plus empty-suppress alloc; bounded_read buffer reuse;
heic decode-cap; per-EP `ep_guard` reenable; cancel-flush.

**All previously-deferred items now FIXED** this pass: LibraryView trash false-success (await result,
remove only Ok tiles); Cleanup/People `MergeById` identity-stable merge (kills the ~1 Hz rebuild
flicker + preserves keeper/selection) + People select-mode; TreeDiff ItemTemplate; Sankey
debounce/flow-matrix/touch; ShimmerView/LavaLamp lifecycle + occlusion + live ReducedMotion; per-model
prewarm cancel (engine static registry + schema `modelKind` + C# wiring + slot reset-on-cancel) +
cancel-as-failure + progress-order; ORT_DYLIB_PATH override-aware pin + CPU-override thread-count +
rename no-clobber MoveFileExW; composite `(kind,scanned_at)` index (v14) + `created_at` capture;
schema-drift (`skippedStages`/`currentCaption`/`modelKind`); RuntimeProbe memoize + input-name cache +
Pass-2 centroid + ThumbnailDiskCache cap + watchdog + path-redaction + completed-count. **On-hardware
confirmation still wanted** for the visual flicker fixes (RTX 2060 build-and-look) + GPU/EP paths
(real NVIDIA/Intel box); engine is fully headless-verified. The ~6.5 f/s GPU ceiling is unchanged
(perf fixes target clustering/query, not the RAM++ tagger). **Note:** `FileID.IpcSchema.Tests` is NOT
in `FileID.sln` (a known gotcha that masked a test break this pass) — recommend adding it to CI.

## 2026-06-02 (later 7) — User-reported GPU-pack bugs + 18-bug sweep (PR #8)

Fixed two user-reported Windows bugs + an adversarial-hunt sweep, via a diagnose→hunt→fix workflow chain (38-agent read-only diagnose/hunt → 8-cell file-disjoint fix + 3 verifiers). All headless-gate-green (engine clippy -D + fmt + tests; app build 0/0 + format + 108 tests). Merged to main (PR #8, 420a5ce), all 5 CI jobs green.

- **GPU acceleration pack now installs ONLY on user action** (`CudaAutoInstaller.cs`): removed the NVIDIA auto-install on engine-Ready (the `TryInstallOrtCudaPack` + auto `PrewarmModelAsync(llama_runtime_cuda_x64)`); kept GPU detection so the Accelerator slot still shows status. Installs only via WelcomeSheet GPU button / Settings / Install-all. **OPEN PRODUCT DECISION:** the Intel/OpenVINO auto-install (`TryInstallOpenVinoPack`) was left intact — Intel has no explicit install button, so gating it would orphan Intel's only path. Decide: leave it, or gate it + add an Intel install entry point.
- **Download flicker fixed** (`ModelSlot.cs` + WelcomeSheet/SettingsView bindings): the GPU pack runs two sequential sub-installs into one slot, rewinding `Fraction` 1.0→~0 at the boundary → the bar jumped backward + `IsIndeterminate` re-flapped (marquee↔fill). Now publishes a MONOTONIC `Fraction` (`Math.Max` while Downloading) + sticky `HasStarted`; `IsStarting`/`ShowRateEta` gate on `HasStarted` across all 5 WelcomeSheet + 3 SettingsView rows. Added a per-row in-flight re-entry guard (no duplicate Prewarm on double-click). **Visual needs the RTX 2060 to confirm** (one smooth non-rewinding bar; no auto-download on launch).
- **18-bug sweep** (refute-by-default verified): brush-churn/`Resources[]` (MainWindow/PeopleView/DrillDownSheet → ctor-cache + GetBrushSafe); IPC silent-failure/timeouts (RestoreFromTrash/DeepAnalyzeFile/Prewarm → bounded result-await); lifecycle guards (LibraryView _unloaded + ThumbnailService dispose; FilePreviewSheet post-unload; RestructureView static deselect-set reset); UndoStack batch-id parse guard (no IndexOutOfRange); engine `prewarm.rs` (aggregate parallel-download errors + clean partial on sentinel-write fail + log register_dll_dirs Err) + `scan.rs` (actionable model-load-timeout EngineError). Corrected stale CudaAutoInstaller comments in registry.rs/main.rs.

## 2026-06-02 (later 6) — Verified "what's-left" audit + Windows ship-hardening + RAM++ 256 closure + on-hardware

Answered "what's left for v1.0" with a refute-by-default audit workflow (5 cells vs current main) — it found the persistence docs overstate remaining work; **the sole hard external blocker is the EV cert.** Then landed the high-value doable-here code, ran the on-hardware test (authorized), and definitively closed the 256 question.

- **PR #6 ship-hardening → main (138760c, CI-green all 5 jobs):** image-decode cap (deep_analyze.rs 50 MP); **IPC capital-ID casing aligned Rust+C#+schema** (~25 fields, both round-trip suites pass — closes the long-standing eng-ipc casing drift); per-monitor DPI `WM_DPICHANGED` handler; WiX `RollbackBoundary` (Burn `<Chain>`); single-source version (`VERSION`+`Directory.Build.props`→csproj/WiX/Cargo + drift-guard, kills the 5 hardcoded `0.1.0`). Headless-gated first (engine clippy -D + fmt + 255 tests; app build 0/0 + format + tests).
- **`windows-app.yml`** gained the source-URL allowlist scan (app-only PRs were bypassing the engine workflow's scan).
- **RAM++ 384→256 perf lever — CLOSED as a dead end (definitive).** The prior export had completed (`out256/`, `[1,3,256,256]`); I fp16-converted it to 660 MB and A/B'd tag-F1 vs the 384 model on 60 corpus images with the engine-faithful pipeline = **0.76**, well below the 0.90 gate. fp32-256 scored IDENTICAL 0.76 → resolution-inherent loss (lossy position-bias interpolation), NOT a fp16/threshold artifact. RAM++ stays at 384; the ~6.5 f/s ceiling stands. (Python 3.11 was already present — the real blocker was never the toolchain, it was quality.)
- **On-hardware (RTX 2060 / DirectML, fully ISOLATED state — real 24k-file library verified byte-identical/untouched):** the merged engine ran crash-free — 120 imgs/20 s ≈ 6 f/s, 1128 `source='auto'` tags (accurate concrete nouns), 218 SFace 128-d (512-byte) embeddings, 105 clusters with no mega-blob, peak RAM 4.2 GB at 120-file scale. Validates the IPC-casing + decode-cap changes on real hardware.
- **Record corrected:** `NEXT.md` "(later 6)" lists the ~10 audit-verified already-DONE items (SHA256 pinning + gate, release.yml, AutomationProperties, memory bounding, HNSW, USN, WS7, ARM64, WS6 DB-contract) so future sessions stop re-chasing them, plus the genuine remaining work by blocker (EV cert; Mac behavior-layer; Windows-HW soak/matrix; lower-priority doable-here).

## 2026-06-02 (later 5) — WS6 macOS lockstep: DB-contract half (epoch / tag-source / IPC) — PR #5, build-verify track

Tackled the macOS lockstep that "needs a Mac" by splitting it into the **persisted-bytes contract** (do-able + macOS-CI-build-verifiable from here) vs the **behavior-verifiable** half (needs a Mac). Implemented the former via a 10-cell file-disjoint Workflow + 4 adversarial verifiers, grounded in the **current** Windows engine source (the LOCKSTEP doc was stale on month-name and false-positive on vlm_model — verified each claim against code per the "verify directives" rule). Pushed `macos-lockstep` → **PR #5** (macOS CI `pull_request` building; the only gate, since no Windows source changed).

- **Timestamp epoch** 2001-ref → Unix(1970) across writer **and every reader** (DBWriter, DeepAnalyzeRunner, FaceClustering `persons.*` — a verifier-caught straggler, ReadStore incl. a pre-existing writer/reader mismatch, Restructure — dropped `+978_307_200`). **Scan tag source** `vision`→`auto` (writer + all readers) + rescan DELETE/REPLACE + trim-skip-empty; dropped orientation/capability extra tags; byte-faithful hyphen sanitizer. **IPC contract**: `startScan` reshaped to rootPath/rootDisplay?/rescan (unsandboxed model — no `.entitlements`), +`markPersonsDifferent`/`wipeLibrary` commands, +8 reply events/DTOs, +`EngineInfo.hardware`/`HardwareInfo`, +`EngineError.modelKind`, +`deepAnalyzeAll.tagsOnly`; both switches + round-trip test updated.
- **Reverted** the face-bbox JSON swap — it broke macOS clustering (`bboxArea` CSV-parse) and still wasn't byte-faithful (px vs normalized).
- **Deferred (need a Mac to behavior-verify; in `MACOS_LOCKSTEP_NOTES.md` Part 3):** face bbox coord-space + FaceAlign/landmark embeddings (Part 2 #1), RAM++ CoreML tagger (#3), content-hash + rename-heal, restructure-routing rewrite, VLM-tag gen. Found a pre-existing latent `ID`-vs-`Id` schema/Windows-wire casing drift (not a DB-round-trip blocker).
- **HONESTY:** edit-only; Swift not built here. macOS CI build-verifies compilation; the cross-platform DB round-trip that *defines* lockstep still requires the user's Mac — this is build-verified, not lockstep-verified.

## 2026-06-02 (later 4) — Production-hardening pass cont'd: WS1b/WS3/WS7/WS-CD (5 more verified merges)

Continued the v1.0 plan via investigate→implement→adversarial-verify→gate→merge workflows. All headless-gate-green (engine clippy/fmt/test + app build/format/test), pushed to `main`, CI green:

- **WS1b on-demand video thumbnails** (`91b637e`) — new `generateVideoThumbnail` command + `thumbnailGenerated` event (schema + Rust + C#, round-trip-tested). Engine handler runs `keyframe_25pct` out-of-process, fits-192 + JPEG + base64, echoes `modifiedAt` so the app writes ThumbnailDiskCache with the SAME key the tile computes; ThumbnailService correlates the response back to the awaiting tile (20s timeout). Restores video tiles for the EXISTING library (no rescan) without re-exposing the crash class. Verified by a 3-lens adversarial pass (cache-key round-trip, correlation lifecycle, engine panic-safety) — all clean.
- **WS3 ProposeRenames** (`cb208cd`) — the bound-but-ignored checkbox now functions: new `AnalyzeMode::CaptionAndTags` (caption+tags, rename gate excluded) chosen when `!tags_only && !proposeRenames`; `proposeRenames` threaded through schema/Rust/C#/view, default true (no regression).
- **WS7 18 medium/polish fixes** (`abc06a9`) — a fresh 6-lens audit of current main (refute-by-default, adversarially verified) → 19 findings, 18 fixed, 1 dropped as a false positive (SuggestedMerges "transitive dangling" — mergeClusters deletes the source, dest survives). ThumbnailDiskCache (.tmp-orphan, LRU race, LastAccessTicks×2); engine deep_analyze silent-returns + batch_clip `.expect()`; People mark-unknown silent-fail; WelcomeSheet persistence-not-awaited; remaining GoldBrush/style indexer reads → ThemeHelper; DeepAnalyze warm-up timeout; installer ready-timeout 30→75s.
- **WS-CD pt.1** (`50d73f9`) — `publish-bundle.ps1` signtool `$LASTEXITCODE` check (THE ships-unsigned-silently blocker) + `CI_RELEASE` skip-guards + per-MSI signature verify; `release.yml` tag-triggered Windows CD, ready-but-dormant until the EV cert. (PS parse-clean, YAML valid; CI doesn't build the installer so unverifiable beyond that.)
- **WS3 resumable-scan** — investigated + found ALREADY IMPLEMENTED (discovery skip-set on `scanned_at >= modified_at`); the planned `last_file_index` checkpoint is redundant and deliberately not built (DECISIONS 2026-06-02). WS3 complete.

**Plan status:** WS0/WS1(a,b,c)/WS2/WS3/WS4-a11y/WS5-mem/WS7/WS-CD-pt1 all merged + CI-green. Remaining is externally blocked or needs hardware/toolchain not in this env (NEXT.md "(later 4)"): WS5 256-export (Py 3.11–3.13), WS6 macOS lockstep (a Mac), WS-CD EV cert + WiX-build (RollbackBoundary/version) + network SHA256 population + push-verify CI-gate hardening; plus hardware-verify-only polish (per-monitor DPI, keyboard-E2E UI-automation, HNSW/perceived-speed perf, the optional scan-recovery banner).

## 2026-06-02 (later 3) — Production-hardening pass: 6 verified merges (plan `majestic-foraging-tome.md`)

Drove the approved v1.0 production plan via file-disjoint Workflow fan-outs + verified per-workstream merges. Each workstream: headless gate matching CI exactly (engine `cargo clippy --all-targets -D warnings` / `fmt --check` / `test` from the engine dir for the pinned 1.90 toolchain; app `dotnet build` / `format --verify-no-changes` / `test`), then merge to `main`, branch deleted, untracked strays kept out of every commit. Six landed, all green:

- **WS4 accessibility pt.1** (`7b2b799`) — 161 `AutomationProperties.Name/HelpText` across all six tabs + sidebar + sheets (8-agent fan-out, per-cell adversarial review). 28 WCAG-AA contrast flags deferred to WS7.
- **WS2 silent-failure elimination** (`b98becb`) — 20 callsites surfaced via new `EngineClient.WaitForBulkActionResultAsync` (mirrors WipeLibraryAndWait) + `SqliteErrorTranslator` (DB/IO jargon → actionable copy): Cleanup trash (was fire-and-forget + unconditional refresh — failed deletes looked successful), Restructure plan/apply, DeepAnalyze, Bulk rename/tag, People merge + SuggestedMerges, Settings cancel, onboarding; ReadStore/ClipSearch errors consumed into `LibraryViewModel.ErrorMessage` (UI-thread-marshaled — covers the OpenAsync-throws path that skipped RefreshAsync).
- **WS0 model download-integrity** (`51c3364`) — `check_size_plausible` (loose size-sanity in both download paths; catches truncation / HTML-error-page-as-model even with no pinned hash) + `.part-N` orphan guard (oversized stale part → discard, not "done") + 3 unit tests. Hash VALUES + the non-`None` CI gate deferred to WS-CD (need real artifacts; RAM++ hash not final until the WS5 256-export). Rationale in DECISIONS.md.
- **WS3 pt.1 data-integrity** (`6c608f6`) — engine `db::quick_check` at writer open → `db_integrity_check_failed` EngineError with wipe+rescan guidance (was: silently proceed on a torn-page DB); RestructureView per-file selection persistence across nav (static `_deselectedFileIds` — was reset on every tab switch, silently discarding the user's include/exclude choices).
- **WS1c sweep** (`ee8c680`) — 12 theme-brush (`TextFillColor*` / `SubtleFill*` / `CardStrokeColorDefault`) code-behind reads in imperative sheet-builds routed through `ThemeHelper.GetBrushSafe` — closes the remaining SuggestedMergesSheet `KeyNotFoundException` native-fast-fail shape. Framework styles + the custom GoldBrush (reliably present in the merged dictionary) left as-is.
- **WS5 memory bound** (`2f0d6b9`) — L1 BitmapImage cache re-expressed as a real ~128 MB byte budget (was 5000 entries ≈ ~550 MB of decoded bitmaps; the old "~25 MB" comment counted the encoded size). Holds the 50K-scroll working set bounded; LRU evicts the coldest, a miss just re-decodes.

**Remaining** (NEXT.md "(later 3)" has exact resume steps): WS1b out-of-proc video keyframe (restores video thumbnails — the crash itself is already fixed, this is feature-restore; IPC + engine + app); WS3 ProposeRenames (IPC-crossing) + resumable-scan (ship flag-gated, verify on hardware); WS4 per-monitor DPI + keyboard E2E test; WS7 polish + the 28 contrast flags; WS-CD (all CI/CD, the explicit final phase). Externally blocked: WS5 256-export (needs Py 3.11–3.13), WS6 macOS lockstep (needs a Mac), WS-CD EV cert.

## 2026-06-02 (later 2) — Scan-crash fix: in-proc shell VIDEO thumbnail provider fast-fail (merged to main)

User hit a hard crash mid-scan on the real `G:\TrueNAS\Users` library (~8300 files in). Root-caused from the logs + an adversarial diagnosis workflow (19 agents, 15 candidates, 1 prime suspect, 12 dismissed) and fixed; headless-verified (app build 0/0 + tests + `dotnet format` clean).

- **Root cause (diagnosed from logs, not guessed):** the **engine was innocent** — `engine.jsonl` shows it streaming `[TAGGING] ram_plus_summary` then `stdin EOF; entering shutdown → FileIDEngine exiting cleanly` (it only stopped because its parent died and closed the pipe). The **app died by native fast-fail** — `app.log` ends abruptly at 12:58:11 mid-`[THUMB]` churn with NO managed exception (no WER dump armed). The corpus was `.jpeg + .mov`; `ThumbnailService` excludes **audio** from the in-process shell `IThumbnailProvider` (the documented 2026-05-30 `.mp3`-art crash class — unpackaged WinUI has no DllHost/COM-surrogate isolation, so a flaky native handler's `RaiseFailFastException` tears down the whole process with no catchable exception) but **never added the symmetric VIDEO skip**, so every cache-cold `.mov` invoked the in-proc Media-Foundation video frame extractor — a flaky one fast-failed the app.
- **Fix:** added a `VideoExtensions` skip-set mirroring `AudioExtensions` and short-circuit it BEFORE the shell call in `RenderAsync` (`ThumbnailService.cs`) — video tiles now render the placeholder (a previously-cached keyframe still shows via the L2 disk read). The adversarial workflow **confirmed this as the sole prime suspect** and **dismissed** all 12 other hypotheses (TCS/DrainAsync thread-pool continuation, off-thread DispatcherObject, ItemsRepeater recycle race, disk-cache decode, ProgressEvent-burst subscribers, `Resources[...]` indexing) — verified safe: `RequestAsync` uses a `RunContinuationsAsynchronously` TCS, the `tile.Thumbnail` assignment is `DispatcherQueue.TryEnqueue`-marshaled, recycled tiles are `IsDetached`-guarded, and `RunBytesSetSource` fully `catch`-guards its `async void` body.
- **Follow-up (NEXT.md):** restore LIVE video thumbnails safely via an OUT-OF-PROCESS extractor (shell `IThumbnailCache`, or reuse the engine's scan-time keyframe) — the in-proc shell chain is still used for images (lower risk: WIC fallback + happy path), so out-of-proc is the durable fix for the whole class. WER dump arming (`build/enable-crash-dumps.ps1`) recommended for the next repro.

## 2026-06-02 (later) — Multi-workflow perf + bug + lockstep sweep (branch `perf-bug-lockstep-2026-06-02`)

Three orchestrated workflows (perf-lever analysis, adversarial bug-hunt, Windows↔macOS lockstep audit) + on-hardware measurement on the RTX 2060 against `G:\TrueNAS\Users` (13,277 images). Engine headless-green throughout: `cargo clippy -D warnings` clean + **246** tests; pinned 1.90. No C# edits this pass (the dotnet gate is unaffected). NOT yet committed/merged.

- **Perf — two safe wins landed; the throughput ceiling is honestly characterized.** (1) **RAM++ CPU preprocess hoisted out of the model-session Mutex + GPU permit** (`tagging.rs`/`ram_plus.rs`: new `preprocess_tensor` + `tag_prepared`; the lock now wraps only the GPU forward). (2) **Pre-decoded RGB read-ahead byte-budgeted** (~256 MB) instead of a flat `worker×2` frame count (`tagging.rs`) — bounds the 5.7 GB RSS problem + the pathological-frame case. Both verified non-regressing (0 panics, all files tagged). **Measured on the 2060 (CUDA, cap 400): ~6–8 files/s with ~25 % run-to-run variance — RAM++ swung 517→671 ms/file on IDENTICAL code between runs (GPU-clock/thermal), so these <5 % wins sit BELOW the measurement-noise floor; they are architecturally sound hygiene, NOT a measured throughput win.** New repeatable harness `build/perf_bench.ps1` (isolated state, file-capped, GPU-sampled, `[STATS]`-parsed).
- **Perf research (cited, adversarially verified) — the real levers.** **INT8 is a dead end on this stack** (DirectML quantized conv ~10× *slower* per microsoft/DirectML#282; CUDA EP can't consume INT8 nodes; TensorRT auto-INT8 ≈1.0× for Swin — "FP16 recommended"). **The shipped model is genuinely fp16** — verified by inspecting the ONNX (924 MB FLOAT16 vs 0.4 MB FLOAT32; the 882 MB is the baked `[1,4585,51,512]`+`[512,233835]` tag-embedding constants), so the registry comment is right and fp16 conversion is already done (`build/inspect_onnx.py`). **The one real throughput lever is a lower-res 384→256 re-export (~1.8–2.7×, works on the shipped DirectML EP, relieves VRAM)** — toolchain prepared (torch 2.12 + checkpoint downloaded, `export_ram_plus_onnx.py` gained `--image-size`, A/B harness `build/ram_ab.py` ready) but **BLOCKED in this env**: Python 3.14 forces transformers 5.x, and `recognize-anything`'s vendored BERT needs the old `transformers.modeling_utils` symbols (`find_pruneable_heads_and_indices` is gone in 5.x). Needs a Python 3.11–3.13 env (transformers ~4.25 + timm<1.0). Spec in NEXT.md.
- **Bug-hunt (10-cell adversarial workflow) → 3 confirmed; 1 fixed.** **eng-ipc-0 (high) FIXED:** `spawn_blocking` JoinError now emits a terminal event in `planRestructure` / `applyRestructure` / `embedTextQuery` / `embedImageQuery` (was: Restructure plan/apply hangs forever, search stalls 5 s) — mirrors the `face_clustering` PAR-111 precedent. **eng-ipc-1/2 (medium/low) SPECCED, deferred:** IPC field-name casing drift (`queryID`/`personID`/`sourcePersonID`/`batchID`/… serialized lowercase-`d`, violating `ipc.schema.json` → breaks macOS round-trip). Full ~25-field both-sides inventory in NEXT.md; deferred as ONE atomic, test-guarded Rust+C# PR (a partial edit breaks the live Windows app; it is NOT a live Windows bug). Note: the bug-hunt under-reported (capacity blips zeroed several cells) — a fuller re-run is queued.
- **Lockstep audit (56-agent workflow) → 39 confirmed divergences → [`LOCKSTEP-2026-06-02.md`](LOCKSTEP-2026-06-02.md).** The cross-platform DB round-trip is broken on multiple axes, almost all **macOS-side** (needs a Mac to fix+verify): **CRITICAL** macOS writes timestamps as 2001-reference epoch vs Windows UNIX epoch (~31 yr silent corruption; the fix must reconcile several internally-inconsistent macOS read/write sites) and `startScan` uses `rootBookmark:Data` vs the schema's `rootPath`; **HIGH** macOS `FaceAlign.align112` has zero callsites (embeds unaligned crops) + Apple Vision extracts no landmarks, 9 reply events + `wipeLibrary`/`markPersonsDifferent` absent on macOS, source token `vision` vs `auto`, rule-cascade month/category token + VLM-rename divergences. Doc lists each with file:line on both sides + the byte-faithful fix + a `win_verifiable` flag.
- **Infra:** capacity blips repeatedly killed freshly-launched subagent bursts; the workflow scripts were hardened with a 4-try `ra()` retry wrapper (spreads attempts across wall-clock) which got bug-hunt + lockstep through. Reusable workflow scripts saved under the session `workflows/scripts/`.
- **Merged to `main` (5196252) + CI GREEN** (Windows engine ✓ + macOS app ✓; Windows app workflow correctly skipped — zero C# changes in that commit). Then **consolidated branches → only `main` remains**: deleted the 4 fully-merged local + 3 merged remote branches; no open PRs. The stale `fix/win-installs-liborder-cleanup-preview` (d9a0bf4) was **triaged not blind-merged** — its install-flow rewrite (delete CudaAutoInstaller/Llama) is SUPERSEDED by main's all-vendor auto-install (a merge breaks the build: `App.xaml.cs` calls the deleted `*.Hook()`), and its Cleanup keeper/delete-safety is superseded by the accuracy sweep's "likely duplicates — verify before deleting". **Salvaged the two still-good, install-independent parts onto main:** engine `tagging.rs` decoder-thread graceful spawn (no mid-scan panic if the OS refuses a thread under handle/RAM pressure) + `ReadStore` newest-first ordering (`scanned_at DESC, id DESC` — macOS parity); dropped the rest + deleted the branch.

## 2026-06-02 — Audit fixes merged to main (CI green) + RAM++ batching DISPROVEN + accuracy/residual sweep (28 fixes)

Three things landed since the audit:

- **`audit-fixes-2026-06-01` merged to `main` — all 3 GitHub workflows GREEN** (Windows engine 10m45s, Windows app 4m25s, macOS 3m41s). Notably the **macOS lockstep Swift compiled + passed on the real macOS runner** (the previously-unverifiable v13 `face_verification_anchors` migration, the `DBWriter` ON-CONFLICT cascade rewrite, the `timeIntervalSince1970` epoch fix, the canonical `vlm_model` tokens). The cross-platform DB round-trip (db-incompat) is now reconciled on the engine side.
- **Batched RAM++ MEASURED on the RTX 2060 → DISPROVEN.** Built the infra (dynamic-batch ONNX export + `RamPlusBatchCoordinator`, env-gated) then profiled the real wall. **GPU is compute+VRAM SATURATED at batch=1** (util mean 73% / p50 87% / p90 97%; VRAM 5348/5955 MB = 90% full) — the single-image *pool* already fills the GPU. A/B (same ONNX/corpus): single-pool **2.1 f/s** vs batched=4 **1.6 f/s** = **~23% SLOWER**. Production fp16+pool = **6.2 f/s**, near this card's ceiling for Swin-L @384. The "GPU <1% utilized / batching is the only win" premise was **wrong**. Coordinator kept **opt-in OFF** for high-SM/VRAM cards (re-validate per card); false "throughput fix" comment corrected. Real levers = TensorRT EP or a lighter tagger. See [`DECISIONS.md`](DECISIONS.md).
- **Accuracy + residual-bug sweep (10-dimension workflow, 45 agents, adversarially verified) → 30 confirmed; 28 fixed (branch `accuracy-residual-fixes-2026-06-01`).** Headless-green: engine `clippy -D warnings` clean + **246** tests; app build 0/0 + `format --verify` clean + tests green. Highlights — **accuracy:** CLIP nearest→bilinear resize (parity + de-aliasing #1), empty-RAM++→CLIP scene fallback (#7), YuNet landmark-clamp removed (#8), cluster name collision disambiguation + sanitize (#2/#9), c-TF-IDF per-file dedup (#18), dim-mismatch embedding skip (#15), OCR line-bbox union (#30). **Data-loss / correctness:** stale `face_prints`/`ocr`/`doc` cleared via stage-ran flags (#5/#11), Deep-Analyze single-file error now emits terminal `Complete` (no stranded card #6) + single-in-flight gate (#10) + VLM transaction (#23) + temp-file RAII (#24), Cleanup >16 MB "likely duplicates — verify before deleting" (no false byte-identical claim #3), `embed` `query_id`-on-failure (no 5s stall #12), scan coordinator pause/cancel (#20), long-path trash/rename (#28/#29), tags-sidecar follows rename/restructure (#27), path-redaction fallback leak closed (#26). **Schema:** `action` pattern reconciled with the 8 real discriminators (#13). **Deferred:** CLIP tokenizer reference-regex (#16 — needs scene-matrix regen + threshold retune); Cleanup phash parity (#4 — exact-content kept by design for delete safety, divergence documented).

## 2026-06-01 (later) — Full top-to-bottom Windows audit (4-stream, ~675 agents) + 7 verified fixes (branch `audit-fixes-2026-06-01`)

A multi-workflow audit of the entire Windows app across **four adversarially-verified streams** — engine static (18 units, all 75 Rust files × 6 dimensions), app static (15 units, all C#/XAML, threading/fast-fail first), macOS parity (24 Windows↔Swift pairs), and a live on-hardware run — synthesized into [`AUDIT-2026-06-01.md`](AUDIT-2026-06-01.md). **618 raw findings → 153 adversarially confirmed** (engine 48, app 39, parity 66) + on-hardware telemetry. Headless-green throughout: engine `cargo clippy -D warnings` clean + `cargo test` **243** (+1 new); app `dotnet build` 0/0, `dotnet format --verify` exit 0, IpcSchema **34/34**, App.Tests **108/108**.

- **On-hardware (RTX 2060, real `G:\TrueNAS\iMac Documents`, isolated temp DB via `build/audit_onhw.ps1` — the real 24k-file library was never touched).** CUDA EP **binds AND completes cleanly** (`executionProvider=cuda`, pack + cuDNN load) — the long-"unverified" 3-5× path actually works; the prior "DirectML" reports were stale-pack state, not a code defect. Scan: 311 files / **0 failures**, 2,639 content-accurate tags, faces all **128-d SFace** (no stale ArcFace), restructure + merge-suggestions functional. **Perf is the real problem:** **4.9 files/s even on CUDA** (target ≥140); CLIP barely batches (avg ~1.5 img/dispatch); per-file wall ~1.5 s vs ~0.36 s active → serialization stall; peak RSS **5.7 GB** (vs 1.5 GB cap); clustering over-splits (176 persons / 624 faces). A first DirectML attempt aborted at the model gate from a *test-harness* bug (Models junction one dir level too high) — the synthesis's HW-1 "DirectML never completes" was reclassified **UNVERIFIED** (re-measure separately).
- **8 fixes landed + headless-verified.** Engine (data-loss/crash): `wipe_all` FK-leak scope guard (**ENG-2** — was leaking `foreign_keys=OFF` on the persistent writer on any error path); `file_ref` lossless `u64→i64` bitcast at all 5 binds + high-bit regression test (**ENG-18** — a high-sequence NTFS ref `> i64::MAX` aborted the whole scan batch via rusqlite `ToSql`); restructure no-op-check-before-uniquify (**ENG-42** — was renaming already-correct files to ` (2)`); `SFace.embed` 128-d assert (**ENG-69**); per-file read-buffer pre-alloc clamp (**ENG-71** — a bogus/huge stat size aborted all decoder threads via `Vec::with_capacity`). App: `UndoStack` lock (**APP-1** — cross-thread `LinkedList` corruption); `AutoTriggerFaceClustering` re-entrancy gate (**PAR-111** — a rescan's 2nd `ScanComplete` re-fired clustering, racing the engine); model-install watchdog ctor-captured UI dispatcher (**APP-2** — was `null` post-`ConfigureAwait`, silently inert).
- **Perf wave — root cause PROFILED (RAM++), not guessed.** Two hypotheses were tested on the RTX 2060 and **disproven** (CLIP fill-window 20→75 ms: no gain → reverted; DBWriter back-pressure: `out_tx` buffers 256). Permanent `[STATS]` instrumentation (`ramplus_us`/`vision_wait_us`) then pinned it: **RAM++ Swin-L @384 ≈ 670 ms/file on `pool_size=2`** (VRAM-clamped on 6 GB) → workers wait ~680 ms for the RAM++ pool; CLIP (~190 ms) is starved downstream. A candidate fix was then TESTED on hardware — a CUDA pool=3 (EP-aware VRAM sizing) — and it **REGRESSED** to 3.9 files/s (RAM++ 670→812 ms, RSS→7.6 GB): 3 Swin-L sessions over-subscribe the one GPU and thrash, confirming RAM++ is **GPU-COMPUTE-bound, not concurrency-bound** (reverted). The only real win left is **batched RAM++** (offline dynamic-axis ONNX re-export + a batch coordinator) or a lighter tagger — specced in NEXT.md.
- **Second fix wave (headless-verified).** Engine: **ENG-59** per-EP crash-disable markers (two packs can now both stay disabled; was one overwritten `.ep_disabled` file); **ENG-88** zip-bomb ACTUAL-decompressed-bytes cap via `Read::take` (was trusting the attacker-declared header size); **ENG-91/92** rename keeps `path_hash` in sync + no longer reports false success on a failed DB write; **ENG-97** path-redaction anchored on the real app root + canonical app-dir (was leaking any user path containing a folder named "FileID", username and all); **PAR-69/96** restructure filename sanitizer ported byte-faithfully from macOS `componentSafe` (Windows reserved names / trailing dots / replace-not-delete — was emitting NTFS-invalid folder names + cross-platform tree drift). App: **PAR-116** kind-filter pushed into ReadStore SQL (was a post-LIMIT C# filter → under-filled grids); **PAR-117** `failed=0` in semantic search. Plus permanent RAM++/vision-wait `[STATS]` instrumentation + 2 new regression tests (engine **245** tests). All headless-green (engine clippy + tests + fmt; app build 0/0 + format + 108/108).
- **Adversarial self-review of every fix (17-agent workflow) → 5 gaps closed, 0 regressions.** The review returned 10 correct + 7 concerns. Closed: **ENG-88** cumulative zip cap now charges ACTUAL decompressed bytes (declared-size accounting let a many-entry bomb evade the 2 GiB total); **PAR-116** kind-filter now threads through the PRIMARY text-search path (`ClipSearchService`), not just browse/find-similar; **ENG-91** path_hash also synced at the restructure-apply move site; **ENG-97** redaction prefix now requires a separator boundary (was passing `…\Local\FileIDBackup\…` through) + the new test is Windows-gated; **PAR-111** face-clustering JoinError now emits an error event so the auto gate releases on a clustering panic. Left intentionally: ENG-59 reenable-all (bounded/safe), RAMPP-POOL (tested + reverted). Re-verified green after closures.
- **Most serious OPEN issue: the cross-platform DB does not round-trip** (db-incompat, needs a Mac) — macOS missing the v13 migration; SFace 128-d-vs-ArcFace-512-d face-embedding + alignment mismatch; `source=`/`vlm_model`/timestamp-epoch token drifts. Full prioritized backlog in the report + [`NEXT.md`](NEXT.md). Not committed/merged pending review.

## 2026-06-01 - Windows: Wipe = reset-to-clean + Restructure macOS-parity overhaul (branch `windows/wipe-restructure-overhaul`)

Two user-reported Windows issues. Headless-green: app `dotnet build` 0 warn / 0 err, `dotnet format --verify` exit 0, `FileID.App.Tests` 108 (+6 new) + `FileID.IpcSchema.Tests` 34 passed. On `windows/wipe-restructure-overhaul`; the WinUI runtime path needs the RTX 2060 (see NEXT.md).

1. **"Wipe + Rescan" -> "Wipe" (the button "couldn't wipe").** Root cause: `RunWipeAsync` always called `TriggerRescanAsync()` after a successful wipe, so the library repopulated on the spot - the wipe looked like a no-op. Removed both rescan calls (engine-side `wipeLibrary` truncate + the stop/delete/restart fallback are unchanged). On success the app now resets to first-run state - `AppViewModel.FolderPath = null` nulls `LastFolderPath`/`LastFolderDisplay` and returns the sidebar to the empty picker - and shows a "Library wiped" confirmation. Downloaded models under `Models/` are kept (per the user's "reset to a totally clean state, keep models"). `SidebarFolderHeader.xaml(.cs)`.
2. **Restructure tab - recommendation-first + file-first (macOS parity).** Replaced the analytics-first UI (Anchor/Mixed/Junk count strip, confidence-tier chips, flat category list) with a port of macOS `RestructureView.swift`: stat hero (Staying / Tidying / Reorganizing) + a reworked Deep-Analyze nudge (real "Run Deep Analyze" button gated on caption fraction < 0.4) + Flow/Tree toggle + unified surface (Sankey hero + Keep/Tidy/Reorganize recommendation cards) + Staying-put expander + nothing-to-move card. Cards expand in place to the actual files (checkbox + "from <folder>") with per-file + per-group selection; "See all" reuses `DrillDownSheet` via a new `SetOutcomeFilter`. Pure app-side - the engine plan already carried `Tier`/`Confidence`/`Reason`/`FolderClassifications`. New VMs `RestructureOutcome` / `RestructureFileRowVm` / `RestructureRecommendationVm` + a shared `RestructureGrouping` (Tier->outcome, unit-tested, replaces the duplicated mapping in the view + DrillDownSheet). All lists are ItemsRepeater + DataTemplate over observable VMs (V15.x fast-fail-safe); the stat hero + hover cross-highlight are inlined into the view (no separate control/bus) and one DataTemplate is tinted from the VM (no selector).

## 2026-05-31 (later) — Windows: Suggested-merges crash fixed + faces/merge audit (merged to `main`)

User report: opening **People → Suggested merges** hard-crashes the app. Root-caused + fixed, then audited the whole faces/merge path. Headless-green: solution `dotnet build` 0/0, `FileID.App.Tests` 102 + `FileID.IpcSchema.Tests` 34 passed, `dotnet format --verify` exit 0; engine `cargo clippy -D warnings` clean, `cargo test` 242 passed, `cargo fmt --check` clean. Merged to `main`; the win-installs work (`d9a0bf4`) is intentionally NOT included — it stays on its own branch, still gated on hardware verification. GUI runtime still needs the RTX 2060 (see NEXT.md).

1. **The crash (P0).** `SuggestedMergesSheet` built each row imperatively in `Render()` — which runs in a raw `DispatcherQueue.TryEnqueue` callback with no try/catch — and indexed *theme-dictionary* brushes via `Application.Current.Resources["TextFillColorSecondaryBrush"]`/`["SubtleFillColorTertiaryBrush"]` (throws `KeyNotFoundException`; the XAML correctly uses `{ThemeResource}` for exactly these), plus rebuilt full `UIElement` subtrees as ItemsRepeater items per engine event (the V15.4 layout-pass fast-fail shape). Replaced with a `DataTemplate` over a new `MergeSuggestionVm` (mirrors `PersonCluster.AnchorImage`: lazy/cached `BitmapImage`, `DecodePixelWidth=80`), `{ThemeResource}` resolved natively, `_unloaded` guard. Both crash mechanisms gone.
2. **Merge hardening (P1, engine).** `handle_merge_clusters` now guards `source==dest` (was: delete the person row while its faces still point at it → orphaned faces) and recomputes the destination `representative_face_id` (highest-quality embedded face) instead of leaving it stale.
3. **"Different people" via IPC + survives re-cluster (P1).** Was a direct app-side `ReadWrite` SQLite write (violated single-writer; `SQLITE_BUSY` risk) keyed on `person_id` — which churns every re-cluster, so the verdict silently stopped suppressing. New `markPersonsDifferent` IPC command routes the write through the engine's single writer; migration **v13** adds `face_a`/`face_b` to `face_verifications` and the verdict + `findMergeSuggestions` filter now key on the *stable* anchor `face_prints.id` pair (legacy person-pair rows still honored). **macOS must mirror v13.**
4. **Suggestion speed + freshness (P2).** `findMergeSuggestions` replaced two per-person correlated subqueries with a single rep-face JOIN. After a merge the sheet also resolves sibling rows referencing the merged-away person.

Known gap (flagged, deferred): `revertMerge` has no UI caller and `handle_merge_clusters` records no merge history, so merges are effectively un-undoable — true undo needs a history record (out of scope for the crash fix).

## 2026-05-31 (audit hardening) — ETA fix + data-loss/crash fixes + security + perf/quality (merged to main, CI-green)

A workflow audit (81 agents: parity + ETA design + adversarially-verified bug/security/perf hunt) drove a multi-phase pass. **All landed work is headless-verified** (engine `clippy -D warnings` + 242 tests incl. 11 new; app build + `dotnet format` + IpcSchema 34/34 + App 102/102). **Merged to `main` (PR #3 → `3b11713`); all three CI workflows green** — Windows engine (x64 + arm64-native + arm64-cross), Windows app (.NET, x64 + arm64), macOS app (SwiftPM build + test + smoke). The macOS edits (B8/S5/S8) thus got their first real verification on the macOS runner.

- **Phase 0 — critical data-loss + crash fixes (engine, 7 new tests).** B1 rename-heal no longer collapses coexisting byte-identical copies (heal only on `file_ref` move or hash-match-with-old-path-gone). B3 restructure drops `MOVEFILE_REPLACE_EXISTING` + uniquifies colliding dests. B2 clustering modal-dim filter (no panic on legacy/corrupt embeddings). B4/B5/S6/S7 restructure stale-plan revalidation + corrected atomicity comment + durable recovery sidecar + source containment. B6 `ep_guard` arms the override-aware EP (`runtime::armed_provider`). B7 removed `panic="abort"`. C1/C2/C4 doc-extract zip-bomb caps + trash-log 1024-cap.
- **Phase 1 — the broken ETA (engine + Windows app + macOS, 2 new tests).** Root cause fixed: ETA divides remaining by a rolling wall-clock EMA, not the per-batch DB-flush rate ("13s for an hour" gone). Windows UI shows the active-stage-labeled ETA ("Tagging — 48m left", "Counting files…"). macOS **B8** rolling-rate reset per session. *Decision:* no IPC `stages[]` array — a scan has 2 live stages; faces/captions are separate jobs with their own ETAs (see DECISIONS).
- **Phase 2 — security.** S9/S12 path redaction in logs; S4 bounded C# stdout framing (1 MiB + resync); S5 bounded Swift IPC buffer; S8 macOS `blobToEmbedding` empty-guards. S2 verify-or-bail is wired but inert (all `registry.rs` `sha256: None`) — activation = fetch+hash artifacts (network step). S1 macOS in-process unzip deferred to Mac.
- **Phase 3 — perf (engine).** P2 Deep Analyze CLI VLM now passes `-ngl 99` (was CPU-only → 5–20× on GPU runtimes, quality-neutral). P4 OpenVINO `AUTO:GPU,CPU` device pin. P16 sargable BINARY-range rescan/deep-analyze prefix seeks (was non-sargable `LIKE 'root%'`; +1 new test). P3 EP-aware vision/CLIP concurrency (rises to pool size on CUDA/TensorRT; no-op on 6 GB by design; DirectML keeps the TDR floor).
- **Phase 4 — quality.** P18 widen merge-suggestion band (dedicated `MERGE_SUGGEST_COS_HIGH=0.66`, additive). P17 mutual-kNN Pass-1 gated behind `FILEID_FACE_MUTUAL_KNN` (default off, on-hardware A/B). P22 already env-tunable.
- **Deferred (verification-gated), specced in NEXT.md:** S2 hash population + S1 macOS unzip; P1 batch RAM++ (ONNX re-export, Python/HF); CUDA-bind 3–5× verify on the RTX 2060; P19/P20/P21 quality tuning; macOS parity EG1–EG5 (RAM++ port, FaceAlign wiring, content-hash rebind, SFace contract cleanup, doc-text/BGE); Windows UI parity UG1–UG5 (Deep Analyze status card, RAM-fit gating, Settings); P12/P13 ANN search index.

## 2026-05-31 (later) — OpenVINO pack assembled + hosted on HF (merged, CI-green)

The B3 OpenVINO handoff is DONE. Assembled `ort-openvino-win-x64-1.22.0.zip` verbatim from the
official PyPI wheels `onnxruntime-openvino==1.22.0` + `openvino==2025.1.0` (ORT 1.22 + OpenVINO
provider + the matched OV 2025.1 runtime DLLs + a `plugins.xml` + bundled MIT/Apache-2.0 license
texts), uploaded to `huggingface.co/Web-World-Wide/OpenVINO` (model card documents provenance +
license). `registry.rs` `ort_openvino_x64` now points at the real repo (was the
`fileid-ort-openvino` placeholder), ~40 MB download. Verified the hosted zip round-trips and
`onnxruntime.dll` is a valid PE @ ProductVersion 1.22.0 with the OpenVINO provider + Intel GPU
plugin present. Commercial-clean (MIT + Apache-2.0; no proprietary bits). Merged to main
(`4d201bd`), both Windows workflows green. **Only remaining OpenVINO gap: bind + perf verification
on a real Intel GPU** (none in the dev env) — safe regardless via ep_guard.

## 2026-05-31 — All-vendor HW acceleration auto-install + vLLM decision (branch `windows-allvendor-accel`)

Builds on the merged CUDA pack. Headless-verified (engine clippy+tests; app build+format+tests). On-branch.

- **vLLM vs llama.cpp — researched, KEEP llama.cpp.** vLLM is a server throughput engine (pre-allocates ~90% VRAM, NVIDIA/Linux-first, no Metal); FileID is single-user on-device on consumer GPUs (6 GB 2060) across Windows+macOS — llama.cpp's exact lane. No backend change. Full rationale + sources in DECISIONS.
- **B1 — EP crash-safety gate (`models/ep_guard.rs`), the linchpin.** Arms a `packs/.ep_attempt` breadcrumb around the first ORT session bind (scan.rs), disarms on success; a stale breadcrumb at next startup (main.rs `resolve_poison_at_startup`) → the bind crashed → persistent `.ep_disabled`, fall back to DirectML until re-enable (Verify install / pack reinstall / override). `detect()` treats a disabled EP as absent. Bounds auto-enable risk to one crash → auto-revert.
- **B2 — CUDA auto-install on NVIDIA.** `CudaAutoInstaller.TryInstallOrtCudaPack` now auto-fetches cuDNN + `ort_cuda_x64` (gated by the now-wired `DisableAutoInstallCudnn`), independent of the llama-cuda sentinel. Stale `CudnnAutoInstaller` comment fixed.
- **B3 — OpenVINO framework (Intel), Apache-2.0.** `ort_openvino_x64` registry entry (HF `Web-World-Wide/fileid-ort-openvino`); `ORT_DYLIB_PATH` pin generalized to the detected vendor's pack via `runtime::active_pack_dir` (NVIDIA→cuda, Intel→openvino); `CudaAutoInstaller` Intel branch + `DisableAutoInstallOpenVino`; Accelerator sentinel/routing/size wired. **HANDOFF:** assemble + upload the OpenVINO ORT 1.22.0 artifact, then verify on Intel HW — until then the auto-install 404s gracefully and Intel stays on DirectML (B1-safe).
- **B4 — QNN/Snapdragon: no hosted pack (proprietary SDK).** DirectML baseline; QNN used only if the device provides it. Settings copy updated.
- **Still pending your RTX 2060:** confirm CUDA auto-installs + binds (`ExecutionProvider=="cuda"`, 3-5x), and that a forced bad bind reverts to DirectML via B1 instead of crash-looping.

## 2026-05-30 (later 5) — Crash + grid arrow keys + tag noise + CUDA pack (branch `windows-scan-fixes`)

Four user-reported issues from a real ~2h scan of a 24k+ library on `G:\TrueNAS`. All
headless-verified (engine clippy+232 tests; app build+format+102 tests). On-branch, not yet merged.

- **>1h crash — DIAGNOSED + MITIGATED.** Engine was innocent (`engine.jsonl`: clean shutdown
  after the app closed the pipe). The APP died by **native fast-fail** (`last-session.txt`
  clean_exit=false, ran 22:58→01:00 ≈ 2h; nothing logged despite full UnhandledException/AppDomain
  handlers). Died on the UI thread mid-burst extracting `.mp3` album art via the **in-process shell
  IThumbnailProvider** — shell providers run in-proc, so a flaky audio art handler fast-faults the
  whole app. Fix: `ThumbnailService` skips the shell provider for audio exts (after the L2 disk read,
  so cached covers still show). `build/enable-crash-dumps.ps1` arms WER full-dump capture for the
  next repro to confirm. Diverges from macOS (QLThumbnailGenerator is out-of-process).
- **Arrow keys — IMPLEMENTED.** The Library grid is an `ItemsRepeater` (no built-in keyboard nav;
  9dd7785 only fixed the preview sheet). Added a focus cursor over `ViewModel.Items`: arrows (±1 / ±row),
  Home/End, PageUp/Down, Shift+arrows extend, Enter opens preview, Space toggles select — wired on
  `GridScroller` tunneling PreviewKeyDown (handledEventsToo) so the ScrollViewer can't eat arrows first.
- **Tag accuracy — DIAGNOSED + duration noise removed.** `tag_report.py` on the real 32,899-file DB:
  RAM++ content tags are solid (child 0.95, cake 0.97, birthday cake 0.985). Noise was score-0.000
  enrichment. Removed audio/video **duration** tags (`3 sec`/`1 min` — metadata, not content). `iPhone`
  (camera) + `Year_*` KEPT per user (useful filter facets). Weak generics (huddle 0.70, floor 0.795)
  left for optional on-hardware floor tuning.
- **Perf 3-5x — ROOT-CAUSED + CUDA pack built (NOT yet on-hardware verified).** Real [STATS]:
  ~1,273 ms/file (~5 files/s); engine log: "NVIDIA … CUDA pack not installed; using DirectML
  (~3-5x slower)". Cause: pyke ort's binaries ship base onnxruntime.dll + providers_shared but NOT
  `onnxruntime_providers_cuda.dll`, so the EP chain falls through to DirectML. Built the **CUDA
  Performance Pack**: registry `ort_cuda_x64` = Microsoft's onnxruntime-win-x64-gpu-**1.22.0** zip
  (MIT, github.com, version matched to the shipped onnxruntime.dll), `ORT_DYLIB_PATH` pinned to the
  pack's matched runtime (inert until installed), provider-specific detection, Accelerator slot +
  Settings install the provider+cuDNN. cudart/cublas already present (llama.cpp-cuda pack); cuDNN
  auto-installs. **The RTX 2060 must confirm the EP binds + the 3-5x — see NEXT.md.** All-vendor:
  AMD/Intel/Snapdragon keep DirectML (production path); OpenVINO/QNN packs follow the same pattern.

## 2026-05-30 (later 4) — Processing-stat flicker + preview arrow/Space keys (Windows runtime bugs)

Three bugs the user hit in the running WinUI app:

- **Tagged / Memory / ETA erratic during a scan — FIXED.** The earlier "later 2" fix
  clamped only the phase *label*; the *stats* still flickered. Root cause: the engine
  emits `ScanProgress` from TWO concurrent sources during the discovery↔tagging pipeline
  overlap — the discovery ticker (`scan_session.rs:240`: processed=0, eta=None, fps=0,
  its own RSS read) and the tagging emitter (`:400`: live processed/eta/fps). `EngineClient.Apply`
  replaced `LastProgress` wholesale on each, so the sidebar bounced N→0→N / real→"computing"→real
  / two RSS readings. Fix: gate the WHOLE `ProgressEvent` on the monotonic phase rank —
  drop any event whose phase is below the latch, so `LastProgress` only holds one phase's
  stats at a time. Tagging events carry the LIVE `discovered` count (`scan_session.rs:404`),
  so "Discovered" keeps climbing through the overlap.
- **Arrow keys dead on the preview sheet — FIXED.** The sheet is hosted in a `ContentDialog`,
  which owns keyboard focus once shown, so the sheet's own `PreviewKeyDown` never fired.
  Fix: the host wires the handler on the DIALOG via `AddHandler(PreviewKeyDownEvent, …,
  handledEventsToo:true)` — tunneling reaches the dialog (ancestor of the focused element)
  before focus-nav or a focused button can eat the key.
- **Space starts/pauses video+audio — ADDED.** Files load paused (`AutoPlay=False`); Space
  now toggles `PreviewMedia.MediaPlayer` play/pause via the same handler (guarded so typing
  in the tag box still types a space).
- **macOS lockstep: nothing to port — verified, not assumed.** The macOS engine has exactly
  ONE `ScanProgress` construction site (`FileIDEngineMain.swift:606 emitProgress()`) built
  from a single `cur` session snapshot, so the Windows dual-emitter race structurally can't
  occur there. The arrow/Space fixes are WinUI-`ContentDialog`-focus-specific (SwiftUI has no
  analog). All three fixes are legitimately Windows-only.
- **Verified headless:** `dotnet build` x64 0/0, `dotnet format --verify-no-changes` 0,
  IpcSchema 34/34, App.Tests 102/102. (Live-GUI confirmation — flicker gone, keys live — is
  the user's to eyeball; the headless engine path can't drive the renderer.)

## 2026-05-30 (later 3) — On-hardware verify + macOS lockstep + RAM++ lock-in + consolidate to main (CI GREEN)

**Final: all three GitHub CI workflows green on `main`@784cc7b** — Windows engine ✓,
Windows app (.NET) ✓, macOS app ✓. The consolidation merge first tripped two real
failures, both fixed forward: (1) macOS `FaceAlign.swift` had a latent closure-arity bug
(a `(Float,Float,Float)->Float` closure called on a tuple — Swift dropped tuple-splat
years ago; never compiled, only surfaced when macOS CI built the lockstep branch for the
first time) → closure now takes the tuple; (2) `CleanupViewModel.cs` was written UTF-8
*without* BOM, failing the `dotnet format --verify-no-changes` CHARSET gate (my headless
gate ran build+test but not format) → re-encoded with BOM, format gate now passes locally
+ in CI. Lesson recorded: add `dotnet format` to the headless gate; macOS Swift only gets
real verification from macOS CI, so merge-then-watch is the loop.


Closed out the scan/cleanup batch: on-hardware test on the RTX 2060, ported the safe fixes to
macOS, tuned RAM++ to "locked in," and consolidated all work onto `main` (branches removed).

- **On-hardware (RTX 2060, 100-photo sample from `G:\TrueNAS\Users`, seed 42).** Built via
  `sample_corpus.ps1`; scanned with the release engine; the test **backed up and restored the
  user's 24,305-file working library** around the run (RESTORE_OK + independently re-confirmed
  24305 rows / 167.5 MB). Results: 100/100 tagged, 0 failed, 974 tags; **`content_hash` set on
  100/100** (Cleanup exact-dupes path is live); **restructure planner SQL ran with no
  DISTINCT error** (D1 verified on real data); tag set **clean — no "catch", no animal
  misclassification**, high-confidence content tags (boy 0.94, child 0.94, basketball 0.97).
- **RAM++ locked in.** The floor raise (0.5→0.62) cut weak tags; the remaining "too generic"
  offenders on the sample were posture/clothing fillers (stand 47×, pose 20×, wear, lay, sit),
  so those + `catch` are now in the built-in `SUPPRESSED_TAGS` (unit-tested, case-insensitive),
  on top of the no-rebuild `ram_plus_suppress.txt` sidecar. `cargo test --lib` green.
- **macOS lockstep (unverified-until-Mac, per apple/CLAUDE.md).** Ported the two mechanical,
  obviously-correct fixes: the identical `GROUP_CONCAT(DISTINCT …)` crash in `Restructure.swift`
  → deduped correlated subquery; Faces-badge removal in `LibraryView.swift` (tile + detail row).
  Consciously NOT ported (documented in DECISIONS): RAM++ tuning (macOS uses Apple Vision, no
  RAM++), Cleanup exact-dupes (macOS engine writes only phash; `content_hash` has no writer +
  BLAKE3 needs a dep), and the phase clamp (macOS `ScanCoordinator` is already one-way).
- **Consolidated to `main`.** Merged `windows-e2e-correctness` (this whole session) and the
  standing `macos-lockstep` branch (commercial-clean SFace/ViT-B/32 swap) into `main`, then
  removed every other branch so only `main` remains. Final headless build green (engine
  clippy+test, app build + both test projects). STATE/NEXT/DECISIONS updated to record the
  on-hardware verify, the macOS lockstep ports, and the consolidation.

## 2026-05-30 (later 2) — Scan/Cleanup UX pass: flicker + RAM++ tags + Faces badge + restructure SQL + exact dupes

Same branch `windows-e2e-correctness`. Second batch of reported Windows issues (Processing
sidebar, tag quality, the gold Faces badge, a DISTINCT crash, Cleanup semantics). All
headless-verified; on-hardware + macOS parity follow-ups remain.

- **A — Processing sidebar flicker — FIXED.** Discovery + tagging `ProgressEvent`s interleave,
  and `EngineClient.Apply` set `Phase` on every one, so the phase label / `PhaseIcon` /
  pipeline dot flipped Discovering<->Tagging several times a second. Added a monotonic
  phase-rank latch (`_shownPhaseRank` + `PhaseRank()`): a ProgressEvent may only ADVANCE the
  shown phase, never regress; `PhaseChangedEvent` / `ScanCompleteEvent` stay authoritative and
  re-sync the latch; reset on StartScan / ClearPhaseAndError / ResetForWipe /
  SetOptimisticScanningPhase. Fixes every consumer with one change.
- **B — RAM++ tag quality — knobs + tuning loop landed (empirical tuning is on-hardware).**
  `models/ram_plus.rs`: new `ram_plus_suppress.txt` sidecar (one tag/line, case-insensitive,
  merged with the built-in const — no rebuild to extend; `#` comments + blanks skipped); added
  `"catch"` to the built-in suppress set; raised the precision floor 0.5->0.62 and made it
  env-overridable (`FILEID_RAMPLUS_PRECISION_FLOOR`, mirrors `FILEID_RAMPLUS_THRESHOLD`). New
  harness: `build/sample_corpus.ps1` (fixed N-photo sample) + `build/tag_report.py` (frequency
  + mean-score histogram + lowest-confidence-accepted list). The "lock in until perfect" loop
  runs against `G:\TrueNAS` (on-hardware).
- **C — gold "Faces" badge removed.** FilePreviewSheet (the pill + its two code-behind refs +
  the "Faces: Detected" metadata row) and the LibraryView tile face overlay. Text/OCR badge
  kept. Diverges from macOS (still shows it) -> macOS follow-up (DECISIONS/NEXT).
- **D1 — restructure "DISTINCT aggregates must have exactly one argument" crash — FIXED.**
  `commands/restructure.rs` used `GROUP_CONCAT(DISTINCT p.name, char(31))`, which SQLite rejects
  at run (separator arg illegal under DISTINCT) -> the Restructure planner threw "Couldn't read
  files table" (a GLOBAL toast, so it read like a Cleanup error). Replaced with a deduped+ordered
  correlated subquery, extracted to a `PLAN_FILES_SQL` const + a unit test that prepares AND runs
  it (the old form prepared but failed at run).
- **D3/D2 — Cleanup = 1:1 bit-identical + previews — FIXED.** `CleanupViewModel` grouped by
  `phash` with Hamming<=4 fuzzy clustering (perceptual near-dupes, not byte-identical; empty
  groups -> nothing to preview). Switched to exact `content_hash` (BLAKE3/composite BLOB, hex)
  + `size_bytes` grouping, O(n) dictionary, dropped the union-find. `DuplicateGroup.PerceptualHash`
  -> `ContentHash`; CleanupView.xaml `Tag` + refresh tooltip updated. Real byte-dupes now populate
  groups, so the existing `ThumbnailService` previews render. Diverges from macOS (phash) ->
  macOS follow-up. Caveat: `content_hash` is full BLAKE3 only <=16 MB (else head+tail+size
  composite) — equality + matching size is "virtually certain identical"; a true byte-compare on
  collision is a possible future hardening.
- **Verified headless:** engine `cargo clippy --all-targets -- -D warnings` exit 0, `cargo test
  --lib` 232/232 (incl. new restructure-SQL + suppress-sidecar tests); app `dotnet build` x64
  GREEN (0 warn / 0 err), FileID.IpcSchema.Tests 34/34, FileID.App.Tests 102/102. On-hardware
  (flicker hold-steady, RAM++ tuning to clean tags, Cleanup byte-dupe groups + thumbnails,
  Restructure tab no-toast) still to run on the RTX 2060 / `G:\TrueNAS`.

## 2026-05-30 — Windows end-to-end correctness pass (P1–P5 landed; UI polish + on-hardware remain)

Branch `windows-e2e-correctness`. Fixing the reported Windows issues: `ram_plus`
startup toast, wrong download modal, out-of-date Deep Analyze, "Wipe partially
failed", Settings cleanup.

- **P1 — `ram_plus` "not registered" toast — FIXED (committed).** Root cause: a
  STALE `FileIDEngine.exe` (running engine predates commit 674da1d which added the
  ram_plus registry arm); the current app sends prewarm("ram_plus") and the old
  engine returns Unknown. Code: prewarm.rs emits user-facing text + a distinct
  `models_dir_unavailable` kind; ModelInstallerService routes `unknown_model` /
  `models_dir_unavailable` to the install slot as "engine out of date — reinstall/
  rebuild". The LIVE toast clears only after a clean engine rebuild
  (build-all.ps1 -Clean -Run). Leaner guard than a build-stamp handshake (DECISIONS).
- **P4 — "Wipe partially failed" DB lock — FIXED (committed).** Cross-process race:
  app deleted fileid.sqlite right after engine exit (3x200ms retry too short). Fix:
  new `wipeLibrary` IPC — engine (sole DB owner) truncates all tables in-process via
  db::wipe_all (sqlite_master-driven, FTS5-safe, preserves grdb_migrations) + clears
  face_crops/thumbs + WAL checkpoint, replies `libraryWiped`; no file deletion.
  SidebarFolderHeader prefers it + auto-rescans; legacy delete path kept as fallback
  with exponential backoff. Schema + Rust ipc/mod.rs + C# DTOs/converters updated.
- **P2 — wrong download modal — FIXED (committed).** WelcomeSheet showed the old
  non-commercial models (ArcFace MobileFace/~13MB/InsightFace, MobileCLIP-S2) and
  had NO RAM++ row (onboarding could never reach AllInstalled, which gates on
  RamPlus; RAM++ downloaded invisibly). Now Face="YuNet + SFace" (Apache-2.0),
  CLIP="ViT-B/32", + new RAM++ row bound to ModelInstallerService.RamPlus; sizes
  bound to the slots.
- **P3 — Deep Analyze naming gate — FIXED (committed).** The gate hard-disabled
  "Analyze All" whenever any face cluster was unnamed; now advisory (macOS two-path):
  the banner suggests naming for sharper captions, but the user can analyze now and
  name later. Optional deferred polish (NEXT): status card with per-model
  not-yet-analyzed counts + ETA, RAM-fit badge, "Smart names -> Review and apply" card.
- **P5 — Settings model cards — FIXED (committed).** Same stale strings as the welcome
  modal: "ArcFace + SCRFD / ~120 MB" -> "Face models (YuNet + SFace) / ~39 MB";
  "MobileCLIP-S2 / ~210 MB" -> "CLIP ViT-B/32 / ~220 MB"; + new RAM++ card bound to
  Svc.RamPlus (Tag="ram_plus" routed through SlotFor). Settings already had logs
  access, recent scans, engine info, performance/NVIDIA, storage, and About — a full
  macOS-style Advanced-disclosure reorg was scoped out as high-risk cosmetic churn
  for this correctness pass (NEXT).
- **Verified headless:** engine `cargo check` + `cargo clippy --all-targets -D
  warnings` + `cargo test` GREEN; app `dotnet build` (x64) GREEN; FileID.IpcSchema.Tests
  34/34 (incl. new WipeLibraryIpcTests for the wipeLibrary/libraryWiped round-trip);
  FileID.App.Tests 102/102. All on branch `windows-e2e-correctness`, working tree clean.

## 2026-05-30 (later) — Butler restructure built (P1–P4) + macOS mirror + docs rewrite + condense

On `butler-overhaul` (off the merged commercial-clean `main`). Implements the butler
redesign from [`RESTRUCTURE.md`](RESTRUCTURE.md) end-to-end and rewrites the dev/docs surface.

- **Butler engine (Windows; verified: clippy `-D` + 230 tests).** `pipeline/restructure_semantic.rs`
  (P1): CLIP+tags+time fusion → density cluster (reuses `identity_clustering`) →
  learn-your-style folder prototypes → proposed moves, wired into `commands/restructure.rs`
  with a rule-cascade fallback. **P2:** c-TF-IDF distinctive-term group naming (live
  local-VLM naming deferred to a background pass — a per-call llama subprocess is too slow
  for an interactive plan). **P3:** per-move confidence bands (auto/review/ask) from
  folder-match strength + top-1−top-2 margin + cohesion, plus a plain-language reason;
  surfaced over IPC + a "What to apply" tier strip (selective apply that holds "ask" back) +
  a drill-down confidence pill + reason. **P4:** Sankey gets the Okabe-Ito CVD-safe palette +
  an "Other" long-tail node (no silent drop).
- **macOS mirror — engine port CI-verified; app-side UI pending.** `RestructureSemantic.swift`
  ports the engine faithfully (reuses `IdentityClustering`); `proposeAll` runs it + stamps
  confidence/reason; IPC `RestructureMove` gains confidence/reason. The macOS CI
  (`swift build --product FileIDEngine/FileID` + `swift test`) compiled the port and passed the
  new parity tests. The app-side UI wiring (reason display, confidence→Keep/Tidy/Reorganize
  mapping, Okabe-Ito Sankey) remains — documented in `platforms/apple/MACOS_BUTLER_NOTES.md`.
- **Docs rewritten from scratch** against verified source: all three `CLAUDE.md` +
  SHIP/PRIVACY/SECURITY/CONTRIBUTING/TESTING/COVERAGE/SYMBOLS/VISUAL-LANGUAGE/BUGS. Honest
  findings surfaced: model-download SHA256 is wired but inert (every `registry.rs` entry is
  `sha256: None`) — now the top open hardening item; the old "Phase 8 coverage gate" was fictional.
- **Condense pass** (engine, behavior-preserving): match-arm merges, if/else→match,
  loop→iterator, push-loop→`extend`.
- **Verified**: clippy `-D warnings`, 230 engine tests, `dotnet build`/`test` (133), `dotnet
  format` (headless), **and all three GitHub workflows green on `main`** — Windows engine,
  Windows app, and macOS (which compiled the Swift port + ran the parity tests). **Not yet**
  verified on-hardware (butler plan quality on `G:\TrueNAS`); the macOS *app-side UI* wiring is
  the remaining Swift work.

## 2026-05-30 — Accuracy tightening + UI fixes + docs refresh + butler-restructure research/design

On `polish-docs-ui-tests` (off the merged commercial-clean `main`).

- **Accuracy (precision bias).** RAM++ `max_tags` 12→8 + a 0.5 precision floor under
  the per-class thresholds — validated on `G:\TrueNAS` (345→243 tags on 27 photos,
  cleaner, still accurate). Deep Analyze CAPTION/RENAME prompts sharpened for
  specificity (decoding already greedy). RAM++ generic-tag suppress-list
  ("face"/"image"/"photo"/…) in the engine + a read-side filter in `ReadStore`
  (legacy DBs need no re-scan).
- **UI.** Root-caused the spurious "faces" chip to RAM++'s 4585-vocab (not C#) and
  fixed it. Sidebar toggle is correct end-to-end on current main (V16.29 fix present +
  wired); added a null-guard for the startup/teardown race. Preview path diagnosed
  sound + bounded (the one full-file-read fallback documented, not blindly rewritten).
- **Docs.** README, `platforms/windows/CLAUDE.md`, `ARCHITECTURE.md` refreshed for the
  commercial-clean stack (RAM++ / ViT-B/32 / YuNet+SFace / Qwen-7B; v1–v12; Apache-2.0).
- **Cleanup + tests.** Removed the unused `DotProductScalar`; +7 engine tests (RAM++
  suppress, registry URL/alias/sentinel invariants, SFace normalize). clippy `-D
  warnings` + 224 engine tests + app tests green; dotnet format clean.
- **Butler restructure.** 5-angle cited deep-research synthesized into
  [`RESTRUCTURE.md`](RESTRUCTURE.md) — cluster-then-name, learn-your-style folder
  prototypes (Dropbox Smart Move pattern), 3-tier confidence, augmented Sankey. 4-phase
  build plan in `NEXT.md`; **P1 (semantic + style engine) is the next build.**
- **Deferred (documented, own pass):** the `Scrfd` reference removal (tested/silenced),
  a comprehensive comment-condense pass, the butler build (P1–P4), and the macOS mirror
  of the faces-tag fix + accuracy tuning.

## 2026-05-29 — Commercial-clean (Apache-2.0) model stack + RAM++ primary tagger (Windows; on-hardware verified)

Branch `windows-ramplus-adopt` (off `main`/V16.29). Adopts **RAM++** as the primary in-scan
tagger and replaces every non-commercial weight with an Apache/MIT one, so the app ships
license-clean under a new root **Apache-2.0 `LICENSE`**. See DECISIONS 2026-05-29 for the why.

**Engine (Rust)** — 6 commits:
- **RAM++** (`models/ram_plus.rs`): Swin-L @384, 4585-tag ONNX (fp16, self-hosted
  `Web-World-Wide/ram-plus-onnx`), per-class thresholds, `FILEID_RAMPLUS_THRESHOLD` override.
  Primary tagger in `pipeline/tagging.rs`; CLIP scene tags gated to fallback. VRAM pool budget
  1500→2000 MB.
- **Faces** (`models/{yunet,sface,face_align}.rs`): YuNet (MIT) detect + SFace (Apache, **128-d**)
  embed + 5-pt similarity alignment to the 112×112 template. `arcface.rs` removed; `scrfd.rs`
  kept as reference. v12 migration wipes face tables. Cluster bands calibrated on-hardware
  (pass1 0.66 / pass3_min_mean 0.60, set in the measured gap between genuine clusters ~0.85+ and
  chained blobs ~0.50) — largest cluster on a 1475-face set cut 90%→7%, known single identity (27
  studio portraits) stays one cluster at mean cohesion 0.93.
- **CLIP** → OpenAI ViT-B/32 (MIT), 512-d (schema unchanged); scene-embedding matrix regenerated.
- **VLM**: Qwen-3B (research-only) dropped → Qwen-7B (Apache) recommended + Mistral-Small-3.2.
- `registry.rs` arms repointed (ids/sentinels kept as stable keys → no install/gate churn).

**App (C#)**: AppSettings v5 migration (default 7B, allowed-VLM allowlist), RAM++ installer
slot, "Face models (YuNet + SFace)" label, display sizes. `dotnet build`/`test`/`format` clean.

**Verify**: clippy `-D warnings` clean; **217 engine tests + app tests green**. **On hardware
(RTX 2060, DirectML EP) against `G:\TrueNAS`** via the new `build/iterate.ps1` + `scan_assertions.py`
harness: faces detect+embed (128-d/512-byte prints), single-person (27/27→1) and multi-person
(11→4, recurring subject grouped) clustering correct, RAM++ tags specific + accurate, HEIC
decodes + tags, all models bind the GPU. Bounded stability soak (2000 files) run.

**Open**: macOS lockstep (WS-MAC, Swift not yet written); rename-heal collapses coexisting
exact-duplicate files (pre-existing, see NEXT.md); throughput re-baseline (DirectML ~6–7 files/s;
CUDA Pack = 3–5× path); SFace cluster-band calibration on labeled faces.

## 2026-05-27 — V16.29 SmolVLM removal, tag-quality diagnostic + threshold + audio duration, sidebar + Deep Analyze fixes

Targeted response to a user-reported triple: (1) tag chips on images/videos/audio "still
suck" — only the year shows; (2) "remove all SmolVLM stuff"; (3) navbar toggle doesn't
collapse + Deep Analyze tab doesn't show downloaded models.

**Engine (Rust)**:
- **SmolVLM dropped**: `VlmModelKind::SmolVlm` enum arm gone in `pipeline/deep_analyze.rs`;
  registry arm in `models/registry.rs` removed; `model_kinds_have_unique_ids` test updated to
  the three remaining kinds (Qwen 3B / 7B, Gemma 3 4B); `size_estimates_increase_with_capability`
  rewritten to compare without SmolVLM's tier. CLIP scene tags become the canonical auto-tagger
  (the comment in `scene_vocab.rs` that called CLIP a "placeholder" is now factually accurate;
  the const docstrings updated to reflect that).
- **Tag-quality diagnostic** (`pipeline/tagging.rs:1244-1290`): `[TAGGING] scene_summary` info
  line per image/video with `scene_emit_count` + `max_score`, and a separate `scene_skipped`
  line when either the labeler or embedding is missing. Gives the user a way to grep the log
  and diagnose why their image cards came back year-only.
- **CLIP scene threshold tuned** (`scene_vocab.rs:128`): `SCENE_COSINE_THRESHOLD` 0.18 → 0.15.
  History on this lever in the file: 0.24 filtered everything → 0.18 showed some chips →
  0.15 biases harder toward recall now that scene tags are the *canonical* auto-tagger.
- **Audio duration chip** (`pipeline/audio_meta.rs`): symphonia exposes `n_frames` +
  `sample_rate` on the default track; emit a "12 min" / "1 h 05 min" / "30 sec" chip even when
  there's no ID3 / Vorbis metadata. Voice memos (`Evernote 20130505 211937.wav` and the like)
  now have a useful chip beyond the year fallback.

**Windows app (C#)**:
- **SmolVLM removed end-to-end**: `ModelInstallerService.Vlm` slot deleted (single VLM concept
  now — `DeepVlm`); `VlmSentinelIds` deleted; `UpdateVlmRecommendation` deleted; `_vlmModelKind`
  field deleted; switch arms in `SlotFor` + `SlotForErrorPath` cleaned. CudaAutoInstaller drops
  the SmolVLM-gated CUDA-defer; downloads run when NVIDIA + engine ready (the 8-concurrent HTTP
  semaphore in the downloader handles contention). EngineClient's post-scan VLM auto-advance
  chain (`AutoTriggerDeepAnalyzeAsync`, `WireVlmInstallWatch`, `OnVlmSlotStatusChanged`,
  `SmolVlmWeightsPresent`) removed — CLIP scene tags are emitted inline during the scan, so no
  separate background tagging pass is needed.
- **DeepAnalyzeView**: SmolVLM card → Gemma 3 4B card (third slot was previously dead UI for
  users who installed Gemma; the model-kind sentinel was tracked but no card existed). All
  card subscriptions + tap routing switched to the `DeepVlm` slot (which already tracked
  Qwen / Gemma installs).
- **WelcomeSheet**: SmolVLM-tagger row removed; the 4-row layout is now CLIP · ArcFace ·
  Qwen Deep Analyze · GPU pack. CLIP comment updated to acknowledge it powers both semantic
  search AND scan-time scene tags.
- **AppSettings v3 → v4**: `DisableAutoInstallSmolVlm` property dropped; `AutoChainDeepAnalyze`
  property dropped (post-scan VLM auto-chain is gone). `AllowedVlmKinds` no longer contains
  `"smolvlm"`. Schema migration v3 → v4 flips any leftover `SelectedVlmModelKind = "smolvlm"`
  to `qwen2_5_vl_3b` with a log line. Tests in `AppSettingsTests` updated (schema 3 → 4, the
  `DisableAutoInstallSmolVlm` assertion removed).
- **Settings view**: "Tag automatically with AI after scans" toggle removed (the underlying
  AutoChainDeepAnalyze setting is gone). Sentinel-based VLM-installed migration switched to
  DeepVlm slot + drops smolvlm from the sentinel-id list.
- **Sidebar collapse fix** (`MainWindow.xaml.cs::ApplySidebarVisibility`): `SidebarColumn`
  XAML defines `MinWidth="240" MaxWidth="320"`; setting `Width = 0` to collapse was being
  silently clamped to 240px by MinWidth. Now clear `MinWidth = 0` BEFORE `Width = 0` on
  collapse, and restore `MinWidth = 240` BEFORE `Width = 260` on expand.

**macOS app**:
- `AIModelKind.smolvlm` enum case dropped from `apple/shared/.../AIModels.swift`; switch arms
  exhaustiveness preserved everywhere; `safeDefaultFor(ramGB:)` fallback now Qwen2.5-VL 3B.
  Engine-side `DeepAnalyze.swift::vlmConfig` + `gpuCacheBudgetMB` arms removed. Package.swift
  comment + CLAUDE.md model table + `wipe_local_state.sh` doc updated.

**Docs**:
- Current-state docs (ARCHITECTURE.md, MODELS.md, README.md, both CLAUDE.md, PHASES.md) lose
  SmolVLM from the model lineup tables and prose.
- Historical entries in DECISIONS.md, NEXT.md, STATE.md left intact — they document the V16.X
  architecture as it was at the time, per the append-only convention.

### Build/test (local, in-agent)
- `cargo clippy --all-targets -- -D warnings` clean.
- `cargo test --lib` → **212 passed, 0 failed**.
- `dotnet build` → 0 warnings, 0 errors.
- `dotnet format FileID.sln --verify-no-changes` → clean (pre-push gate per V16.28 memory).
- `dotnet test FileID.App.Tests` → **101 passed, 0 failed** (V16.28 was 102; -1 for the
  SmolVLM InlineData entry in WelcomeSheetModelSizeTests).
- `dotnet test FileID.IpcSchema.Tests` → **31 passed, 0 failed**.

### On-hardware verify (gated on user)
- Rescan a folder of mixed kinds. Grep engine log for `[TAGGING] scene_summary` — every image
  should have `scene_emit_count >= 1` (with threshold 0.15 most photos clear it). Cards should
  show scene chips, not just year. If you still see year-only, check `scene_skipped` lines —
  they'll tell us whether the embedding or labeler is missing.
- Audio cards should show a duration chip (`12 min`, `1 h 05 min`) even on voice memos.
- Click the title-bar hamburger — the sidebar should collapse all the way to zero width.
- Deep Analyze tab now shows three cards: Qwen 3B (recommended), Qwen 7B, Gemma 3 4B. Install
  any of them; the card should flip to "Installed" once the download lands.

## 2026-05-26 — V16.28 hardening pass: OCR overflow, thumbnail-cache LRU, bulk-select batching, tile hover (Windows)

Targeted security/perf/parity pass on top of V16.27. No new features; the goal was to land concrete
fixes for issues surfaced by a code audit while pushing back on the audit items that turned out to
be wrong (`restructure_apply.rs` "unwraps" are test-only; `platform.rs:389` already uses
`unwrap_or`; `LibraryView.swift:506` is the kind-filter chip animation, not the tab switcher —
tab crossfade already matches at 0.22s).

**Engine (Rust)**:
- **OCR dimension overflow defense** (`engine/src/shell/ocr.rs`): `recognize` now caps each side
  at 16384 before any multiplication, so `width * height * 4` cannot overflow u32 and
  `SoftwareBitmap::CreateCopyFromBuffer`'s i32 dim parameters stay in range. Added 3 unit tests
  (zero dim, oversize dim, short buffer); all early-bail before any Windows API call.
- **Keyword extractor tidy** (`engine/src/util/keywords.rs:44`): replaced
  `u32::try_from(phrase.len()).unwrap_or(u32::MAX)` with `phrase.len() as u32`. Phrase length is
  bounded by the doc-extract 16 MB cap upstream; the saturating-cast defense was dead code.
- **OCR public-API comment** (`engine/src/shell/ocr.rs:13-25`): `OcrResult.lines` /
  `OcrResult.locale` / `OcrLine` are populated but not yet consumed. Replaced bare
  `#[allow(dead_code)]` with a one-line comment naming the future consumer (per-line OCR overlay)
  so the next maintainer knows why the surface is intentionally fat.

**Windows App (C#)**:
- **ThumbnailDiskCache: in-memory LRU index** (`FileID.App/Services/ThumbnailDiskCache.cs`): the
  previous sweep walked `EnumerateFiles("*.bin", SearchOption.AllDirectories)` on every cap trip
  — O(N) disk IO on libraries with 10K+ cached thumbnails. Replaced with a
  `ConcurrentDictionary<string, CacheEntry>` index seeded once at startup by `Prime()`. Reads
  touch `LastAccessTicks` in memory (no more `SetLastAccessTimeUtc` syscall per cache hit);
  writes update the index and recompute `_cachedBytes` by delta. On cap exceed, sort the
  in-memory index by ticks and delete oldest until under headroom — zero filesystem walks after
  startup. Eviction policy is factored into a pure `SelectEvictions(...)` helper covered by 4
  unit tests in `Tests/FileID.App.Tests/ThumbnailDiskCacheTests.cs`.
- **LibraryViewModel: bulk-selection batching** (`FileID.App/ViewModels/LibraryViewModel.cs` +
  `Views/Library/LibraryView.xaml.cs`): `OnTilePropertyChanged` was firing two PropertyChanged
  events + a `SelectionRegistry` republish on every per-tile `IsSelected` toggle. Ctrl+A on 10K
  tiles burned 20K notifications and 10K `_selected.ToList()` allocations (`SelectedItems`
  getter). New `BulkSelectionScope()` IDisposable wraps the three bulk-mutation sites in
  `LibraryView.xaml.cs` (Ctrl+A / `OnSelectAllClicked`, shift-click range select, plain-click
  clear-all). Per-tile handler still updates `_selected` but defers notifications under the
  scope; on dispose, fires one batch. `SelectedItems` now caches the list snapshot and
  invalidates on real change. `ClearSelection()` rewired through the same scope.
- **Tile hover stroke animation** (`Views/Library/LibraryView.xaml` + `.xaml.cs`): macOS tiles
  ramp their white stroke 0.08 → 0.18 opacity over `easeOut(0.18s)` alongside the existing
  scale (LibraryView.swift:676-680). Windows tiles were animating scale only. Replaced the
  Grid's themed `BorderBrush` with an inline `SolidColorBrush` per tile (so each instance owns
  an animatable opacity), and added `ApplyTileStrokeOpacity` — a `Storyboard` + `DoubleAnimation`
  with `CubicEase EaseOut` that runs alongside the scale spring. Shadow opacity animation
  (0.18 → 0.45, blur 5 → 14) is deferred since it needs per-tile `Composition.DropShadow`
  plumbing with cleanup on tile recycle.
- **ReadStore.cs: pre-existing Span-in-async fix** (`FileID.App/Services/ReadStore.cs:303`): the
  V16.27 in-flight work had introduced `MemoryMarshal.Cast<byte, float>` inside an `async`
  method, which is a C# 13 preview feature unsupported under .NET 8's stable language version
  (CS8652). Extracted the cast into a sync `BlobToFloats(byte[]) -> float[]` helper at the
  same level as `DotProduct`. Pre-existing blocker, not a regression from this session — the
  V16.27 build was broken on disk until this fix.

**ReadStore search query audit** (B3, audit-only): `SearchAsync` at `ReadStore.cs:144-166`
OR-joins six branches. The `ocr_fts` / `doc_fts` MATCH branches are fast (FTS5-backed). The four
`LIKE '%x%'` branches (`f.path_text`, `f.vlm_proposed_name`, `f.vlm_description`, `tags.tag`,
`persons.name/first_name/last_name`) are non-sargable — any one of them forces SQLite into a
files-table full scan, and indexes won't help leading-wildcard LIKE. The real fix is a migration
v8 that extends `doc_fts` (or adds a new `text_fts`) covering `path_text`,
`vlm_proposed_name`/`description`, `tag`, and `person_name` so the query becomes MATCH-only. Out
of scope this session — needs the user's real library to validate the migration. Surfaced as a
NEXT.md follow-up.

**Comment surgery** (D2, narrow): cleaned the LibraryViewModel header (was mangled with a stray
"The shape is the same:" run-on), trimmed the redundant "detach listeners" prose in `Dispose`,
and compressed the per-tile-PropertyChanged-forwarding comment to keep just the WHY (the "VM's
SelectedCount stayed silently stale" bug rationale). Other V16.27 files (`tagging.rs`,
`doc_extract.rs`, `audio_meta.rs`, `ReadStore.cs`) were inspected; no slop worth churning over —
the comments there are WHY-style technical notes (cross-references to SwiftUI line numbers,
performance pitfalls, invariant statements) that map cleanly to CLAUDE.md's keep-WHY rule.

### Build/test (local, in-agent)
- `cargo +1.90 check` clean. `cargo +1.90 clippy --all-targets -- -D warnings` clean.
- `cargo +1.90 test --lib` → **212 passed, 0 failed** (V16.27 was 209; +3 OCR overflow tests).
- `dotnet build src/FileID.App/FileID.App.csproj` → 0 warnings, 0 errors.
- `dotnet test Tests/FileID.App.Tests/` → **102 passed, 0 failed** (V16.27 was 98; +4
  `ThumbnailDiskCacheTests.SelectEvictions_*` tests).
- `dotnet test Tests/FileID.IpcSchema.Tests/` → **31 passed, 0 failed**.

### On-hardware verify (gated on user)
Same gates as V16.27 still pending. Additionally:
- Scroll a library with 10K+ thumbnails; the previous 30s "cache sweep" pause should be gone (no
  more directory walk after startup).
- Ctrl+A in a 10K-tile library: selection should land instantly, not over multiple seconds.
- Hover a Library tile: stroke should brighten from a faint 0.08 to a clear 0.18 over 0.18s,
  matching the macOS tile hover affordance.

## 2026-05-26 — V16.27 scan-pipeline single-read finalization + UI parity polish (Windows)

Pipeline I/O consolidation on top of V16.26, paired with two surgical UI-parity fixes the macOS
audit surfaced.

**Engine (Windows, `pipeline/tagging.rs` + `doc_extract.rs` + `audio_meta.rs`)**:
- **EXIF ghost-read fix**: `run_decoder_thread` now seeds `exif_data = Some((None, None, None))`
  on every successful image `read_to_end`, so the worker's `parse_exif_blocking` fallback is
  unreachable for images. Every non-EXIF format (PNG, GIF, screenshots, etc.) skips one wasted
  re-open + re-fail per file. `parse_exif_blocking` deleted as dead code.
- **Doc / PDF / Audio single-read**: extended the image-style pre-read pattern to Doc/Pdf/Audio
  kinds (files ≤ `FULL_HASH_MAX_BYTES` = 16 MB). The decoder thread reads once, hashes from the
  buffer, and threads `Option<&[u8]>` into the kind-specific extractor. `doc_extract::extract`
  and `audio_meta::extract` now accept `bytes: Option<&[u8]>` and dispatch internally:
    - `doc_extract`: zip helpers refactored to generic `<R: Read + Seek>` inner functions that
      take either a `File` or a `Cursor<&[u8]>`; plain-text path uses `String::from_utf8_lossy`
      on the buffer when supplied.
    - `audio_meta`: tiny `BytesMediaSource` adapter wraps `Cursor<Vec<u8>>` with symphonia's
      `MediaSource` trait (declares seekable + byte_len).
  Worker's `content_hash` fallback is unchanged — still fires correctly for video (codec API
  needs a path), unrecognized kinds, and the > 16 MB long-tail.

**Windows UI parity**:
- **ApplyBar hover spring** (`RestructureView.xaml.cs`): wired four `PointerEntered`/`PointerExited`
  handlers on `ApplySymlinkButton` + `ApplyMovesButton` via the existing `SpringEasing.AnimateScale`
  helper, mirroring macOS `RestructureApplyBar.swift:114-117` (response: 0.28, dampingFraction: 0.7,
  scale 1.02 on hover-while-enabled). The XAML comment had promised this; now it matches.
- **TagChip Kind brushes** (`Theme.xaml`): defined `TagChipKindForegroundBrush` (#FFFFFF) and
  `TagChipKindBackgroundBrush` (#808080 @ 0.30) so `TagChip.xaml.cs:74-75` no longer silently
  falls through to hardcoded values. Latent footgun closed.
- **TagChip.FormatTag macOS-parity fix** (`TagChip.xaml.cs:135`): the C# port used
  `ToTitleCase(ToLowerInvariant(...))`, which mangled internal capitals — `iPhone-14` → `Iphone 14`
  vs macOS `LibraryView.swift:646-652` `first.uppercased() + dropFirst()` → `IPhone 14`. Rewrote
  to match the Swift implementation exactly: pre-formatted space-bearing labels pass through, only
  the leading character of the final segment is uppercased, internal model-number casing is
  preserved. Adds an early `Contains(' ')` guard so `"Has TEXT"` stays as-is (previously it would
  have title-cased to `"Has Text"`). Test `FormatTag_MatchesMacParitySpec(iPhone-14, IPhone 14)`
  now passes — was failing on HEAD.

**Repo hygiene**:
- `.gitignore`: stray `onnxruntime.dll` / `onnxruntime_providers_shared.dll` under
  `src/engine/` (fetch-runtime-deps.ps1 sometimes drops them next to the binary for local dev).
- Staged `scene_embeddings_precomputed.rs` (real source — `scene_vocab.rs:35` includes it). The
  include is now wrapped in `mod scene_embeddings { ... } pub use scene_embeddings::SCENE_EMBEDDINGS;`
  with `#[allow(clippy::excessive_precision)]` so the precomputed CLIP rows stay byte-faithful with
  the source notebook without spamming 5 884 lint suggestions.
- `downloader.rs` SHA streaming: heap-allocated the 64 KB chunk buffer (`vec![0u8; 65536]`
  instead of `[0u8; 65536]`) so the async future doesn't balloon to ~67 KB and propagate
  `clippy::large_futures` errors through `prewarm.rs` callers. Pure quality fix; preserves the
  user's in-flight streaming-SHA logic exactly.
- `xml_text_runs` in `doc_extract.rs`: collapsed the nested-`if` into a match guard
  (`Ok(Event::Text(t)) if depth > 0 =>`) to silence the new `clippy::collapsible_match` lint.

### Build/test (local, in-agent)
- `cargo +1.90 check` clean. `cargo +1.90 clippy --all-targets -- -D warnings` clean against
  the full working tree (engine + user's in-flight edits). `cargo +1.90 test --lib` →
  **209 passed, 0 failed** (up from V16.26's 204 — added bytes-vs-path equivalence tests for
  `doc_extract` (txt + docx) and `audio_meta`, plus a sanity test for the new
  `BytesMediaSource` adapter). `dotnet build FileID.sln -c Debug` → 0 errors, 0 warnings.
  `dotnet test Tests/FileID.App.Tests/` → **98 passed, 0 failed** (the
  `FormatTag_MatchesMacParitySpec(iPhone-14, IPhone 14)` regression that was failing on HEAD
  is now green).  `dotnet test Tests/FileID.IpcSchema.Tests/` → **31 passed, 0 failed**.

### On-hardware verify
- Scan a library with PNG + GIF + JPG + docx + mp3 + pdf + a > 16 MB file. JPEGs surface
  camera/GPS in the preview metadata; PNGs scan without crash; docs/audio surface keyword chips
  / artist+album tags; > 16 MB file exercises the composite-hash fallback successfully.
- Restructure tab: hover the gold "Apply as shortcuts" and outlined "Convert to real moves" —
  both spring up to ~1.02× and settle. Disabled state stays at 1.0.
- Library Kind chips render visually identical to before (theme brushes match the previous
  hardcoded hex).

## 2026-05-22 — V16.26 no-self-host policy + hanging-feature sweep + PDF / HNSW / BGE unhang

Hardened-policy pass on top of V16.25: every artifact the engine downloads must already exist on
a public upstream (HuggingFace, ggml-org GitHub releases, NVIDIA developer CDN). No FileID-hosted
files. Plus a sweep that wires three previously-dormant modules.

**Removed (would require self-hosting; legal + sustainability exposure)**:
- **RAM++ integration** — `models::ramplus`, the scan-pipeline block, `ModelStack.ramplus`, the
  registry arm, `shared/scripts/convert_ramplus_onnx.py`, the `MODELS.md` entry. No public RAM++
  ONNX exists — only the official PyTorch `.pth` on `xinyu1205/recognize-anything-plus-model`.
  Image tagging stays on the V16.21 VLM tagger (SmolVLM / Qwen2.5-VL / Gemma 3) exactly as shipped.
- **Performance-Pack registry arms** (`cuda_pack_x64`, `openvino_pack_x64`, `qnn_pack_arm64`)
  plus the `LookupResult::NotYetAvailable` variant + `not_yet_available()` helper they used. The
  engine still picks up the matching execution providers when the user has the SDK DLLs on the
  loader path (system CUDA toolkit via `runtime::system_cuda_toolkit_dir`; user-installed Intel
  OpenVINO redist; Snapdragon's bundled QNN runtime). cuDNN + llama.cpp runtimes remain bundled
  (both publicly redistributable: NVIDIA developer CDN + ggml-org GitHub releases).
- **YAMNet (Phase 5b)** — same hosting blocker as RAM++ (no public general ONNX). Documentation
  removed.

**Unhung (modules previously gated behind `allow(dead_code)` now have real callers)**:
- **HNSW into `face_clustering`** above 5 k faces — turns O(n²) all-pairs cosine into O(log n)
  per query. Uses `instant-distance` (pure-Rust); the brute-force path still wins ≤ 5 k.
- **PDF text extraction** added to `doc_extract` via the gated `pdfium-render` binding (same
  binding `deep_analyze` already uses for rasterization).
- **BGE-small text embeddings** (`models::bge_text`) registered + loaded in `ModelStack` +
  invoked in `process_file_predecoded` for doc text + persisted into `text_embeddings` (new
  migration v11). The pure-Rust WordPiece tokenizer is now live via BGE.

**Tagging promise vs V16.21 — strictly better-or-equal, never worse**:
- Images: same (VLM tagger).
- Documents: strictly new (RAKE keyword chips + FTS5 + BGE semantic search; was zero before).
- Audio: strictly new (artist / album / title / genre / year chips; was zero before).
- Faces: same accuracy, faster above 5 k.
- Rename/move: tags preserved (was orphaned).

### Build/test (local, in-agent)
- `cargo +1.90 clippy --all-targets -- -D warnings` clean; `cargo +1.90 test --lib` → **204
  passed, 0 failed**. C# `dotnet build FileID.sln -c Debug` → 0 warnings, 0 errors.

### Documented follow-ups (in-policy; no self-hosting needed)
- USN reader (`FSCTL_READ_USN_JOURNAL`) + scan-skip-set integration.
- Whisper.cpp subprocess transcription (whisper.cpp binaries on ggml-org GitHub + GGUF Whisper
  models on HuggingFace — fully publicly downloadable).
- Florence-2 inference: 4 ORT sessions + Rust autoregressive generation loop + `tokenizers`
  crate + Deep Analyze backend `modelKind: "florence2_base"`.
- General image multi-label tagger: hold pending a public, clean-licensed, general-purpose ONNX
  (WD-Tagger family is anime-trained → bad for typical user photos; RAM++ has no public ONNX).

## 2026-05-22 — V16.25 research-implementation Phases 3–7: identity, docs, audio, variants, Florence-2

Five phases land on top of V16.24 (Phases 0–2 + content_hash brick from earlier today).

**Phase 3 — identity / USN / vector index.**
- **Rename/move heal**: BLAKE3 `content_hash` + Win32 MFT `file_ref` columns (migration v8),
  computed in discovery/tagging, dbwriter does a pre-INSERT lookup + `UPDATE OR REPLACE` so a
  renamed/moved file re-binds to its existing row instead of orphaning tags / embeddings / faces /
  OCR.
- **USN journal foundation**: `util::elevation::is_elevated` + `pipeline::usn::query_journal`
  (`FSCTL_QUERY_USN_JOURNAL`) + v9 `usn_state` cursor table. Scan-driver integration is Phase 3b;
  the default scan stays on the verified jwalk + timestamp-skip path.
- **Vector index**: pure-Rust HNSW via `instant-distance` — no C/C++ build dep (`usearch` rejected
  for that reason). `util::hnsw_index` build/search wrapper + tests; face_clustering integration
  above ~5 k faces is Phase 3c.

**Phase 4 — document content pipeline.**
- Pure-Rust text extraction (`pipeline::doc_extract`) for txt / md / docx / pptx / xlsx via the
  existing `zip` + new `quick-xml` 0.36. PDF text extraction is Phase 4b (re-uses the gated
  `pdfium-render` binding).
- RAKE-style keyword extraction (`util::keywords`) → `source='auto'` tag chips, no ML model.
- Migration v10: `doc_text` + `doc_fts` (FTS5) — same shape as `ocr_text` / `ocr_fts`.

**Phase 5 — audio metadata.**
- `pipeline::audio_meta` reads artist / album / title / genre / year via `symphonia` (pure-Rust,
  MPL-2.0, no system ffmpeg) → `source='auto'` chips. Audio libraries get real content-style tags
  today. YAMNet sound-event tagging + Whisper transcription are Phase 5b (both need offline ONNX
  conversion, same Python-3.14 constraint that gated RAM++).

**Phase 6 — per-vendor quantized variants.**
- Framework landed in Phase 1 (`models::variants` + pack-presence gating). This phase = explicit
  documentation that per-model accelerated variants (`_int8` for OpenVINO/Intel-NPU, `_qnn.bin` for
  Snapdragon HTP) ship alongside each model's base hosting; the resolver falls back to fp32 when
  the variant file is absent, so untested NPU hardware safely runs on DirectML/CPU.

**Phase 7 — Florence-2 foundation.**
- `models::florence2` skeleton + a real registry arm for `onnx-community/Florence-2-base` (4 ONNX
  files + tokenizer + config, ~440 MB total, MIT). Users can install today; the inference wiring (4
  ORT sessions + Rust autoregressive generation loop + `tokenizers` crate for the BART tokenizer +
  Deep Analyze backend `modelKind: "florence2_base"`) is Phase 7b — the plan ranked it last and
  defer-able since SmolVLM / Qwen / Gemma + RAM++ + Windows.Media.Ocr cover everything except
  phrase-grounded OD.

### Build/test (local, in-agent)
- `cargo +1.90 clippy --all-targets -- -D warnings` clean; `cargo +1.90 test` green across the full
  suite. 10 migrations applied (`v1`–`v10`); new tests: HNSW round-trip + composite hash edges +
  RAKE keywords + doc_extract OOXML + audio_meta dedup + florence2 paths + v8/v9/v10 schema spot-checks.
- **Needs user hardware:** Phase 0 long-path / OneDrive online-only / file-lock retry; CPU
  multi-threading uplift (Phase 1); rename-heal across a real move; doc/audio tag chips render.

### Documented follow-ups (foundation present; full integration deferred)
- **Phase 3b**: USN reader (`FSCTL_READ_USN_JOURNAL`) + scan-skip-set integration.
- **Phase 3c**: HNSW into `face_clustering` above ~5 k faces.
- **Phase 4b**: PDF text extraction (re-use existing pdfium binding); BGE-small text embeddings for
  semantic doc search; GLiNER NER for entity tags.
- **Phase 5b**: YAMNet sound-event tagging + Whisper transcription (both need offline ONNX hosting).
- **Phase 6 hosting**: per-model `_int8` (OpenVINO) + `_qnn` (Qualcomm AI Hub) variant files.
- **Phase 7b**: Florence-2 inference (4 ORT sessions + generation loop + `tokenizers` dep + Deep
  Analyze grounded-OD backend).
- **RAM++ activation**: run `shared/scripts/convert_ramplus_onnx.py` on **transformers 4.x / Python
  3.11–3.13** to produce + host the ONNX (Python 3.14 / transformers 5 blocked locally).

## 2026-05-22 — V16.24 research-implementation Phase 2: RAM++ tagging (+ Phase 3 kickoff)

- **RAM++ wrapper + pipeline** (`models/ramplus.rs`): 384px ImageNet-norm input → per-tag logits →
  sigmoid + per-tag calibrated threshold → `(tag, score)` (`source='auto'`). Wired into the scan
  fast pass right after the CLIP embed as the **primary scan-time tagger when installed**, gated
  behind the existing "model missing → stage skips" path — **zero regression**: the VLM tagger stays
  default until RAM++ is present. Single VRAM-bounded Session (batch-coordinator perf is a noted
  follow-up). I/O tensor names read from the session (robust to re-export). Supersedes the CLIP
  zero-shot scene labeler. Variant-aware load via `models::variants` (Phase 1).
- **Offline conversion**: RAM++ has no first-party ONNX. `shared/scripts/convert_ramplus_onnx.py`
  exports the `generate_tag` image→logits path (opset 17, einsum-vectorized) + copies the tag list +
  thresholds; `MODELS.md` + `DECISIONS.md` document hosting. Registry arm `"ramplus"` is
  `not_yet_available` until hosting lands; a locally-converted `ramplus.onnx` in `Models\ramplus\`
  is picked up directly.
- **Local conversion attempt — blocked (documented)**: the only local interpreter is Python 3.14,
  which forces transformers 5.x; the 2023 RAM++ stack targets transformers 4.x. The script's bundled
  compat shims clear all imports + reach model construction, but full v5 support isn't worth chasing.
  Run the script on **transformers 4.x / Python 3.11–3.13** for a clean export. App behavior is
  unchanged meanwhile (RAM++ gated off). Toolchain (torch/transformers/timm/scipy) was installed into
  the user Python; RAM++ source + weights are cached under `%TEMP%`.
- **Phase 3 kickoff**: `util::content_hash` — BLAKE3 content identity (full ≤ 16 MB; head+tail+size
  composite above) for rename/move rebind. `blake3` dep added (pure-Rust, no C/C++ build).

### Build/test (local, in-agent)
- `cargo +1.90 clippy --all-targets -- -D warnings` clean; `cargo +1.90 test` → **184 passed, 0
  failed** (177 after Phase 1, +3 RAM++ wrapper, +4 content-hash incl. composite-path edge cases).

## 2026-05-22 — V16.23 research-implementation Phase 1: ML/hardware foundation

Shared plumbing every later phase builds on. Engine-only; no new dependencies.

- **`runtime::active_provider()`** — cached (`OnceLock`) single source of truth for which EP this
  process binds, driving the two helpers below.
- **`runtime::configure_session_builder()`** — replaces the hardcoded `.with_intra_threads(1)` in all
  four model wrappers (ArcFace / SCRFD / MobileCLIP / CLIP-text). Graph-opt Level3 everywhere except
  QNN (Level1/Basic — the HTP partitioner rejects ORT's aggressive fusion); intra-op threads =
  performance-core count on the **CPU EP** (CPU-only boxes were single-threaded before — a real
  throughput uplift) while staying 1 on GPU/NPU EPs.
- **`models::variants::resolve_model_path()`** — per-EP quantized-variant selection (`_int8` for
  OpenVINO/Intel-NPU, `_qnn.bin` for Snapdragon HTP) with **fp32 fallback when the variant file is
  absent**, so untested hardware always runs the universal graph (DirectML → CPU) rather than failing.
  Consumed by the Phase 2+ models.
- **`models::wordpiece_tokenizer`** — pure-Rust BERT WordPiece (no `tokenizers` crate) for the
  upcoming GLiNER + BGE text models.
- **QNN HTP backend** — `execution_providers_for_chain` now binds `QnnHtp.dll` for the Snapdragon NPU
  (falls through to DirectML/Adreno if the pack is absent). OpenVINO's NPU `device_type` hint + INT8
  variants are deferred to Phase 6 (need NPU detection; can't regress Intel-GPU users untested).

### Build/test (local, in-agent)
- `cargo +1.90 clippy --all-targets -- -D warnings` clean; `cargo +1.90 test` → **177 passed, 0
  failed** (+10: 4 variant-resolution incl. fp32 fallback, 6 WordPiece).
- **Needs user hardware:** confirm CPU-only inference now uses multiple threads (faster scan where no
  usable GPU); QNN/OpenVINO NPU paths await Snapdragon/Intel hardware + the Phase 6 variants.

## 2026-05-22 — V16.22 research-implementation Phase 0: robustness + doc accuracy

First slice of the approved multi-phase plan to implement the "local high-accuracy file tagging"
research (`~/.claude/plans/i-want-to-implement-radiant-sunset.md`). Phase 0 is engine-side robustness
+ the report's pitfall fixes; no new dependencies.

- **Long paths (>260).** The engine `.exe` has no long-path manifest, so deep directories were
  invisible to the scan and deep files failed to open. `discovery` now walks a `\\?\`-verbatim root
  (children inherit it; jwalk traverses past MAX_PATH), stores normal-form paths (verbatim stripped on
  emit — DB / UI / cross-platform parity preserved), and reconverts to extended-length at the FS-access
  sites (image decode + EXIF). New `util::path_safety::{to_extended_length, strip_extended_length}`
  (+ 4 round-trip tests).
- **OneDrive / cloud placeholders.** Discovery flags `online_only` from the file attributes
  (`OFFLINE` | `RECALL_ON_OPEN` | `RECALL_ON_DATA_ACCESS`); the decoder skips content reads for those
  files (metadata-only row) so scanning never silently hydrates a multi-GB cloud download — both a perf
  and a no-telemetry-egress concern.
- **File-lock resilience + AV-friendliness.** Image opens go through `open_image_file`: 3-attempt
  retry-with-backoff on `ERROR_SHARING_VIOLATION` / `LOCK_VIOLATION`, opened with
  `FILE_FLAG_SEQUENTIAL_SCAN`.
- **Doc accuracy.** `platforms/windows/CLAUDE.md` no longer claims "Phase 0 ships only the engine"
  (everything it listed as deferred shipped by V16.21); MSRV corrected 1.78 → 1.90. Fixed a pre-existing
  `useless_conversion` clippy warning in `shell/tags.rs`.

### Build/test (local, in-agent)
- Engine: `cargo +1.90 clippy --all-targets -- -D warnings` clean; `cargo +1.90 test` → **167 passed,
  0 failed** (+4 long-path round-trip tests). App: `dotnet build FileID.sln -c Debug` → 0/0.
- **Needs user hardware:** a real scan over a >260-char path tree and a OneDrive online-only folder
  (confirm deep files get analyzed + stored with normal-form paths; online-only files get metadata-only
  rows and trigger no download).

## 2026-05-22 — V16.21 welcome models, discrete-GPU forcing, tag quality, progress flicker

Six Windows fixes spanning the WinUI app + Rust engine:

- **No more silent SmolVLM download.** Deleted `SmolVlmAutoInstaller` and its `App.xaml.cs` hook +
  `EngineClient` re-arm — model downloads are now strictly user-initiated (welcome screen / Deep
  Analyze tab). First-scan auto-tagging still resumes the moment SmolVLM is installed (the
  `WireVlmInstallWatch` path is unchanged).
- **Welcome screen offers a hardware-tiered Deep-Analyze model.** Split the single VLM row into two:
  the SmolVLM **tagger** row and a new **Qwen** Deep-Analyze row sized to the box
  (`ModelInstallerService.DeepVlm` slot + `UpdateDeepVlmRecommendation`: ≥16 GB RAM **or** ≥8 GB
  VRAM → Qwen 7B, else 3B). Installing it persists `AppSettings.SelectedVlmModelKind` so the Deep
  Analyze tab agrees. `Install all` now covers both VLM rows; `SlotFor`/sentinels split smolvlm→Vlm,
  qwen/gemma→DeepVlm.
- **Better image tags.** `"Has Location"`/`"Has Text"`/`"Has Faces"` capability tags are no longer
  emitted (`push_enriched_extras`) — they read as content but described a capability and crowded out
  real tags. `TAG_PROMPT` rewritten for 1–2 specific concrete tags; `parse_vlm_tags` caps at 2 and
  drops a generic-token stop-list (`photo`/`object`/`location`/…).
- **Discrete GPU forced.** `probe_gpu_vendor` now returns the DXGI adapter index of the highest-VRAM
  non-software adapter; `execution_providers_for_chain` pins DirectML to it via `with_device_id`
  (the scan path: CLIP/ArcFace/SCRFD). CUDA stays default (the iGPU isn't CUDA-visible). For
  llama.cpp (Deep Analyze) a best-effort `--list-devices` probe pins `--device VulkanN` only when a
  clearly-dominant (≥2 GiB) discrete device exists — no-op on CUDA builds / single-GPU boxes.
- **Download progress no longer flickers.** Welcome + Settings model rows now use one `ProgressBar`
  (indeterminate → determinate at first byte) instead of swapping a `ProgressBar`↔`ProgressRing` on
  every `Fraction`-crosses-0; the sidebar scan bar latches `IsIndeterminate=false` once the file
  total is known.

### Build/test (local, in-agent)
- Engine: `cargo +1.90 clippy --all-targets -D warnings` clean; `cargo +1.90 test` → **163 passed, 0
  failed** (new tests: `parse_vlm_tags` cap/stop-list, `parse_best_vulkan_device`). (Running clippy
  from the repo root picks `stable` 1.95 and surfaces unrelated toolchain-drift lints — use `+1.90`.)
- App: `dotnet build FileID.sln -c Debug` → **0 warnings, 0 errors**.
- **Needs user hardware:** discrete-GPU forcing (verify dGPU load in Task Manager during a scan +
  llama.cpp device log), the welcome flow end-to-end, and that tags read as 1–2 descriptive words.

## 2026-05-22 — V16.20 push V16.16–V16.19 + clear two pre-existing CI reds

Committed and pushed the session's work (CLIP split, crash fix, Deep Analyze gating, preview
nav/video, Restructure auto-gen, Cleanup thumbnails, docs trim) to `origin/main`. Two pipelines
were already red before this push and are fixed here:
- **Engine** `Privacy — source URL allowlist scan` (x64) had failed since `models/vlm_server.rs`
  landed — it formats `http://127.0.0.1:{port}` for the local llama-server and `127.0.0.1` wasn't
  allowlisted. Fixed by exempting loopback hosts in the scan (loopback is never egress; see
  DECISIONS V16.20). arm64 was always green (the scan is x64-only).
- **App** `Format check` (x64) had failed on `Add braces to 'if' statement` (IDE0011); the brace
  fix was already in this session's tree, so `dotnet format --verify-no-changes` is clean now.

### Build/test (local, pre-push)
- Engine: `cargo +1.90 fmt --check` + `clippy --all-targets -D warnings` + `test --all-targets`
  all green; URL-allowlist scan replicated locally → PASS.
- App: `dotnet build -c Release -p:Platform=x64` → 0 errors; `dotnet format --verify-no-changes` clean.

## 2026-05-21 — V16.19 macOS parity: Restructure auto-generates + Cleanup thumbnails

- **Restructure auto-generates** (macOS RestructureView.swift `.task`/`.onChange`): no manual
  "Generate plan" click. `RestructureView.OnLoaded` renders an already-computed plan (cached on
  the engine across tab switches) or, if none, auto-runs `PlanRestructureAsync` when a library
  folder is scanned; it also re-generates on `DeepAnalyzeComplete` so the People/<name> buckets
  reflect newly-named clusters. The Generate button stays as a manual re-gen.
- **Cleanup shows thumbnails** (macOS CopyTile parity): each duplicate group is now a
  horizontal strip of 132-px thumbnail tiles (thumbnail + filename + size + Keep radio) instead
  of text rows. Tiles load lazily through `ThumbnailService` via the members ItemsRepeater's
  `ElementPrepared`/`ElementClearing` (cancel + release on recycle) — the same
  virtualization-friendly pattern LibraryView uses. `DuplicateMember` gained `Thumbnail` +
  `ShowPlaceholder` + recycle guards.

### Build/test
- C# `dotnet build` 0/0, `dotnet format` clean, BOM intact.
- **User on hardware:** open Restructure with a scanned folder → a plan appears without
  clicking Generate; open Cleanup → each duplicate group shows file thumbnails.

## 2026-05-21 — V16.18 preview: arrow-key navigation + video player hardening

User-reported: arrow keys didn't move between items in the preview, and the video player was
buggy. `FilePreviewSheet`:
- **Arrow-key nav fixed.** The ←/→/Esc handler existed but only fired with keyboard focus
  inside the sheet — and the host ContentDialog (no default button) left focus on the dialog
  chrome, so keys never reached it. The sheet now grabs focus on `Loaded` and uses tunneling
  `PreviewKeyDown`, so ←/→ navigate siblings from anywhere in the sheet (overriding a focused
  video's seek), while the tag `TextBox` keeps ←/→ for its cursor. Esc closes.
- **Video player hardened.** Switched to `MediaSource.CreateFromStorageFile` (the StorageFile
  broker — same path the thumbnail loader uses) instead of a raw `file://` URI, which is more
  reliable for arbitrary local paths. The `MediaSource` is now disposed on navigation and the
  `MediaPlayer` is disposed on close — pause+null alone left audio playing and the file handle
  pinned. A generation guard drops stale async loads when arrow-navigating quickly through clips.

### Build/test
- C# `dotnet build` 0/0, `dotnet format` clean, BOM intact (UI behavior is the user's check).
- **User on hardware:** open a preview → ←/→ move between files (incl. over a video); play a
  video then close → audio stops + the file isn't locked; arrow through several clips → no glitch.

## 2026-05-21 — V16.17 CLIP scene-tagging OFF; CLIP kept for semantic search

SmolVLM is the sole tagger; CLIP must not emit tags — but free-text semantic search is kept
(user asked to keep it). CLIP (MobileCLIP-S2) did two independent jobs sharing the per-file
image embedding: scan-time scene tags (`source='auto'`) and the Library's semantic-search
embedding. Scene tags are now off; the search embedding stays. (SmolVLM is a generative VLM,
not a dual-encoder, so it can't do embedding search itself — CLIP runs alongside it for that.)

- **Engine.** `ENABLE_CLIP_SCENE_TAGS = false` → the `tagging.rs:954` scene-scoring block is
  skipped, so no `source='auto'` tags. `ENABLE_CLIP = true` keeps the MobileCLIP image encoder
  loading + the per-file embedding (stored in `clip_embeddings`) for semantic search.
  `load_default` builds the scene labeler ONLY when BOTH flags are on, so the ~21 s
  scene-matrix build is skipped (it's tags-only). SmolVLM (`source='vlm'`) is the sole tagger;
  `ReadStore` already orders vlm ahead of auto. The `commands/embed.rs` `!ENABLE_CLIP`
  short-circuit + the C# empty→null guards stay as harmless defense.
- **App.** Library semantic search works as before (MobileCLIP query embedding → cosine over
  `clip_embeddings`); the "install CLIP for search" banner, the MobileCLIP install card
  (Settings + Welcome), and CLIP in onboarding (`InstallAll`/`AllInstalled`) all stay. Settings
  diagnostic now reads "Tags: SmolVLM; Semantic search: MobileCLIP-S2."
- **Net:** no CLIP tags (SmolVLM only), semantic search preserved. To drop CLIP entirely
  (search → FTS5), flip `ENABLE_CLIP = false`.

### Build/test
- Engine on the pinned **1.90** toolchain: `clippy --all-targets -D warnings` clean, `test
  --lib` 158/0, `fmt --check` clean. C# `dotnet build` 0/0, `dotnet format` clean, UTF-8 BOM
  intact (incl. a BOM added to `WelcomeSheet.xaml` per `.editorconfig`).
- **User on hardware:** re-scan → tags are SmolVLM-only (`SELECT DISTINCT source FROM tags`
  has no `auto`); free-text search ("a dog at the beach") still returns semantic matches;
  `clip_embeddings` populates on new files; engine log shows no ~21 s scene-matrix build.

## 2026-05-21 — V16.16 mid-scan crash root-caused + fixed; Deep Analyze gating honest

The "click a page mid-scan → crash" bug was misattributed (the V16.5c DetailHostView
async-race theory). Three crash dumps from today (pid 19792, 12:03:21/23/32) were
identical: `NullReferenceException at RestructureView.OnVisualizationModeChanged` — a
`<ComboBox SelectedIndex="0" SelectionChanged=…>` raising SelectionChanged during
`InitializeComponent()`, before the `Sankey`/`TreeDiff` fields exist. It fired every time
the Restructure tab opened; `App.OnUnhandledException` (e.Handled=true) softened it to a
half-built tab, not a hard kill.

- **Crash fixed.** `RestructureView.OnVisualizationModeChanged` null-guards its siblings +
  wraps in `DebugLog.SafeRun`. Audited the init-fire pattern repo-wide — only this site crashed.
- **Settings EP-override clobber fixed (same pattern).** `SettingsView.OnProviderOverrideChanged`
  fired during `InitializeComponent` (before `HydrateToggles`/`_initializingToggles`),
  resetting the GPU EP override to "auto" on every Settings open. Now `!IsLoaded`-guarded.
- **ViewModel teardown race hardened.** People/Cleanup/Library `RefreshAsync` now create the
  linked `CancellationTokenSource` INSIDE the try, so a `Dispose()`-race
  `ObjectDisposedException` (from `_disposalCts.Token`) is caught as a clean no-op instead of
  escaping to the caller — that was the empty-message "OnLoaded refresh threw" log noise.
- **Deep Analyze gating honest** (`commands/deep_analyze.rs`): weights-gate FIRST →
  `vlm_model_missing` ("install it from the Deep Analyze tab") instead of a misleading
  runtime error / N silent per-file failures; one `llama_cpp_missing` when no backend can
  run the present weights. The engine source was already correct (registry pinned **b9254**,
  persistent llama-server is the default backend); the user's `llama_cpp_missing` is a STALE
  on-disk runtime (b4475, no llama-mtmd-cli.exe) + uninstalled Qwen weights — a rebuild +
  reinstall, not a code bug.
- **Audits + hygiene.** Dead code: all 32 engine `#[allow(dead_code)]` sites are deliberate
  (functional structs, a documented test fixture, non-Windows cfg-stubs, a parity primitive,
  future hooks) — nothing safe to remove; clippy confirms no *unmarked* dead code. Standards:
  `cargo fmt`/`clippy -D warnings`/`dotnet format`/analyzers all clean, BOM intact. Comments:
  conservative condensation of the verbose history blocks in the high-traffic views/services
  (ThumbnailService, sidebar controls, DeepAnalyzeView) — the load-bearing invariant/forensics
  comments CLAUDE.md flags are kept deliberately.
- **Docs.** STATE/NEXT/DECISIONS trimmed to a lean baseline; PACKS.md + DB-RESEARCH.md
  retired (refs fixed); PHASES checkbox/label + stale Phase-N notes corrected.

### Build/test
- C# `dotnet build FileID.sln -c Debug -p:Platform=x64` green (0/0) + `dotnet format` clean.
  Engine `cargo check`/`clippy --all-targets -D warnings`/`test --lib` (158/0)/`fmt --check`
  all green. (These gates run headlessly in the agent env now — see auto-memory.)
- **User, on hardware:** rebuild engine → relaunch (auto-reinstalls the b9254 runtime) →
  install Qwen2.5-VL-3B → open Restructure mid-scan (no crash) → scan → SmolVLM tags + Deep
  Analyze captions. Per NEXT.md V16.16.

## 2026-05-21 — V16.15 face crops fixed + 1-2 word tags + download jitter + dead code

- **Faces (root-caused + fixed).** SCRFD emits bbox as `[x1,y1,x2,y2]` corners
  (`scrfd.rs`, rescaled to original-image px by `detect()`), but `tagging.rs` fed it to
  `crop_and_resize_face` + stored it as `[x,y,w,h]` — so the crop ran from the face's
  top-left to the image's bottom-right ("not a face"/blank), and that smear was also fed
  to ArcFace (corrupting clustering). Now converted corners→xywh once at the
  detect→`DetectedFace` site → real face crops, meaningful embeddings, correct persisted
  bbox. (`validate_face_geometry` was already correct.) Follow-up: landmark-aligned
  ArcFace chips for better cluster accuracy.
- **Tags are 1-2 words.** `parse_vlm_tags` drops 3+-word fragments (was >3); the SmolVLM
  TAG_PROMPT already asks for 1-2 words.
- **Deep Analyze model reality (verified).** Qwen3-VL-4B has **no GGUF** (ggml-org has
  only Qwen3-VL 2B/30B; macOS uses MLX), and Qwen2.5-VL-7B (~4.7 GB) OOMs on the 4 GB
  card at `-ngl 99`. So Deep Analyze stays **Qwen2.5-VL-3B** (strongest Qwen that fits +
  already a card + full descriptive captions). Gemma-3-4B card swap + 7B-with-VRAM-aware
  `-ngl` flagged as follow-ups (need blind-unverifiable C# x:Name work / an engine change).
  See DECISIONS.
- **Download "freaking out" fixed.** `ModelSlot.UpdateRate` no longer zeroes rate/ETA at
  every per-file fraction reset in a multi-file bundle (carries the prior rate) — that was
  the 0-blip / "Stalled" flicker; sample interval 500→250 ms. `downloader.rs` progress
  throttle 100→50 ms + progress channel 256→512. (Already 12-way parallel range-GET; true
  throughput is near-capped.)
- **Dead code.** Removed the unused `run_ocr_blocking_arc` (live path is
  `run_ocr_blocking`). Remaining engine `#[allow(dead_code)]` are deliberate (test helper
  `ModelStack::empty`, non-Windows cfg-stubs, the pool-path CLIP `embed`). A broad
  slop-comment purge is **deferred** — much of the codebase's verbosity is the
  load-bearing institutional memory the CLAUDE.md says not to strip; touched code is
  WHY-focused.

### Build/test
- Engine `cargo clippy --all-targets -D warnings` clean + `cargo test --lib` **158/0**
  (toolchain 1.90). C# (`ModelSlot`) `dotnet format` clean + BOM intact. WinUI compile is
  the user's VS build. Verify faces/tags/downloads on hardware per NEXT.md V16.15.

## 2026-05-21 — V16.14 small-screen / anti-clipping UI pass

User reported laptop UI content getting cut off. XAML audit (read-only — can't render
here) + conservative responsive fixes to the clear overflow patterns:
- **Deep Analyze action row** (7 controls: Whole library / Selected / Current / Skip
  toggle / Propose renames / Cancel) wrapped in a horizontal ScrollViewer (the
  PeopleView/CleanupView header pattern), so its right-hand controls can't clip on a
  narrow window — the most likely "cut off" culprit.
- **Oversized modal sheets shrunk to fit a laptop** (each already has an inner
  ScrollViewer for overflow): `FilePreviewSheet` 1080×720 → **880×520** (the worst —
  720-tall didn't fit a 768-px screen once title bar + taskbar are subtracted);
  `PersonDetailSheet` 480→440 H; `SuggestedMergesSheet` 520→440 H; `DrillDownSheet`
  700×520 → 640×440; `MainWindow` WelcomeOverlay MinWidth 660 → 580.
- Left as-is (degrades gracefully, doesn't hard-clip): Settings storage path
  (TextTrimming + tooltip), PersonDetail name fields (tight but fit), FilePreview
  toolbar (the `*` filename column absorbs the squeeze before buttons clip), sidebar
  (260 px with a Ctrl+Shift+S toggle).

All 6 edited `.xaml` parse as well-formed XML + BOM intact. **Not render-verified**
(no WinUI build/display here) — the user must eyeball on the laptop and report any
remaining clipping (which view + element).

## 2026-05-21 — V16.13 model-load timeout fix + tagging/Deep-Analyze split (first on-hardware run)

The build finally ran on the user's box (NVIDIA **~4 GB VRAM / DirectML**) after they
installed the VS WinUI PRI component (the CLI can't build WinUI here). First scan failed
with a false "models took >30 s / corrupted" — root-caused from the engine log to the
**21.5 s CLIP scene-matrix build** blowing the 30 s `load_default` timeout. Fixed, plus
the user's model-role ask.

- **Scene-label matrix is disk-cached** (`scene_vocab.rs`): build once (~21 s, first
  launch), reload ~instantly after (raw LE f32 + content-hash-keyed header under
  `Models/clip_scene_cache/`; the hit path also skips loading the 253 MB text session).
  **Model-load timeout 30 → 120 s** (`scan.rs`) so the one-time build can't false-fail.
  → first launch slow once, later launches <10 s. Immediate workaround for the user: a
  second "Start Scan" in the same session already worked (matrix cached process-static).
- **Tagging vs Deep Analyze split.** Auto-tag hardwired to **SmolVLM**
  (`EngineClient.AutoTriggerDeepAnalyzeAsync`, gated on SmolVLM weights present); **Deep
  Analyze defaults to Qwen 2.5-VL 3B** (`AppSettings.SelectedVlmModelKind` default → qwen
  + v2→v3 migration off the leaked smolvlm). SmolVLM auto-installs; Qwen installs
  on-demand from the Deep Analyze card.
- **Deep Analyze cards now honest** (V16.12.1): `DeepAnalyzeView.SyncCards` checks each
  model's gguf on disk instead of mirroring the shared "any VLM" slot — Qwen no longer
  falsely shows "Installed".
- **Hardware tailoring confirmed from logs:** DXGI vendor probe (NVIDIA), VRAM probe
  (3935 MB), EP chain cuda→tensorrt→directml→cpu, pool clamped to 1 to fit 4 GB, per-vendor
  runtime auto-install (Vulkan + SmolVLM + CUDA llama runtime + cuDNN all present). Open
  gap: ONNX runs on **DirectML** (the `cuda` ORT pack is `not_yet_available` → ~3-5×
  slower); the VLM path already uses CUDA. Sourcing the ORT CUDA EP DLLs is a follow-up.

### Build/test
- Engine `cargo clippy --all-targets -D warnings` clean (toolchain 1.90, the CI pin).
- C# (`AppSettings`, `EngineClient`, `DeepAnalyzeView`, build-all.ps1 SDK fix) —
  `dotnet format` clean + UTF-8 BOM intact; full WinUI compile is the user's VS build (the
  dotnet CLI here lacks `Microsoft.Build.Packaging.Pri.Tasks.dll`).
- Verify on hardware per NEXT.md V16.13.
