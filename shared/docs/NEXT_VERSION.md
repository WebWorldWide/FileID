# FileID next version — implementation ledger

## 2026-10-08 related-take proposal checkpoint

IPC v1.11 and the Mac/Rust engines now propose related groups from current visual embeddings and nearby file creation dates among explicit search-result IDs. The Mac review window lets a user adopt a proposal before naming a group and its desired outcome. Exact copies, derivative exports, and already grouped files are excluded. These are uncalibrated **visual proposals**, not automatic detection of a hit, catch, gift, or other event. Next: rights-cleared positive/negative/uncertain group fixtures; calibrate visual similarity separately for each embedding model, measure misses from absent embeddings or unreliable file dates, move catalog-wide discovery to a durable background job, then connect dense timestamped outcome evidence and native GTK/WinUI review.

Accepted direction: a local catalog connects files, people, events, temporal evidence, document passages, and derived versions. Develop and validate macOS first, then mirror each milestone into Windows and Linux. Preserve native interfaces, the existing palette, springs, and LavaLampBackground. No telemetry; inference is offline. User-initiated Hugging Face model downloads remain the only application network feature.

## Current implementation

The foundation is on main through merged [PR #186](https://github.com/WebWorldWide/FileID/pull/186). The stable People follow-up was merged; Adlon Windows bootstrap is [PR #188](https://github.com/WebWorldWide/FileID/pull/188). These are bounded milestones; the complete release remains unfinished.

| Area | Implemented | Remaining |
|---|---|---|
| Example-data safety | Swift/Rust path guards for Adlon, existing aliases, fail-closed dangling symlinks, and managed media originals; guards on database creation, file mutations, and selected cache/model/log writes | Whole-application write tracing, companion-file tests, race-resistant descriptor-based mutation, Windows volume identification |
| Catalog | Canonical v21/v22/v23 SQL, Swift/Rust migrations, provenance, coverage, chapters, passages, tracks/observations, events/takes, derivatives, jobs, recipes, corrections, operations, local chat records | Populating most new entities, native Windows/Linux-host migration acceptance, revision changes for same-size/same-mtime replacements |
| Interfaces | IPC v1.10 catalog/tools/chat and scoped folder/destination bookmark DTOs mirrored in Swift, Rust, and C#; Linux inherits Rust DTOs. Best-take group/feedback/read and Undo actions use the typed catalog envelope. | Full hybrid search, person editing, typed conversion operations, capability reporting, portable generation, port UI |
| TestFlight distribution | Store-only sandbox/bookmark flow, signed-package/upload script, registered `com.fileid.app`, App Store Connect macOS record `6818813859`, internal group `FileID Internal Testers`; local signing assets and strict signed-package validation. Team API access is approved. | Apple one-time team API-key download failed to reach local storage; upload/processing, tester installation, sandbox runtime acceptance. No TestFlight build is available. |
| Search | Persistent incremental FTS plus native engine-owned CLIP HNSW snapshots/deltas; Library hybrid/similarity IPC, timestamp/page evidence, stale/failed exclusion; existing document/OCR FTS snippets now appear in Swift/Rust catalog results | Text/catalog semantic indexes, richer exact/person/event filters, offline availability, end-to-end 100,000-file benchmark |
| Chapters | Native macOS Search & Moments panel; playback seeking, manual markers and summaries, durable correction history and Undo; draft chapters from confirmed transitions between sampled captions, guarded by file revision and protected from overwriting user edits; timestamped on-device speech passages are stored as searchable evidence; engine-backed Rust equivalent | Reliable dense scene/activity chapters, synopsis/storyboard/cast, speaker attribution, subtitles/chapter/FCPXML export, Windows/Linux UI |
| Timeline | macOS durable frame jobs with pause/resume/cancel and recovery; bounded isolated frame and signal workers; one-second low-resolution visual-change signals scanned in cancellable 60-second chunks with overlap and per-chunk coverage; strongest change guides sparse visual-model sampling with uniform fallback; source/model provenance and reusable captions; low-confidence draft chapters; local timestamped speech with verified-chunk resume | Evaluated shot/activity proposals, dense candidate analysis, alignment, face tracks, passage correction UI, stronger per-file errors, timeline quality evaluation, portable worker |
| Renaming | Concise evidence-based stems, 60-character limit, abstention on generic/unchanged suggestions, same-directory collision suffixes; Swift/Rust formatting | Preference settings/learning, date-first style, companion groups, blinded preference review, operation unification |
| Faces | Shared alignment fixtures, overlap/ambiguity-safe landmark matching, one pixel render per image, finite geometry checks, deterministic Rust HNSW; actual SFace weight/processing/source cache keys, bounded native refresh and model-separated catalog observations; whole-pass model/revision/vector guards and persist revalidation; stable identity reuse, correction-aware partitions, retained unknown/offline records and transactional updates | Durable cache rebuild/backlog, incremental namespace partitioning, identity aliases for explicit merges, held-out calibration, incremental assignment, trusted exemplars, video tracking, persistent correction UI |
| Local AI | Existing model stack retained; updated research shortlist; shared RAM headroom checks and cancellable native model residency gate | Benchmarks, pinned candidate packs, portable generation adapter, hardware capability probes, memory-budgeted multi-model routing |
| Scheduling | Persistent timeline state/checkpoints; queued interactive chat priority on the macOS major-job queue | Durable CPU/I/O/GPU/NPU budgets, fairness, multi-model residency/eviction, parallel admission, restart recovery for general operations |
| Best takes | Event/take records; IPC v1.10 manual groups, goal/outcome/preference review, source-stale evidence, correction/Undo, outcome-first recommendation with tie/abstention; native Mac review window and shared Rust service | Automatic related-attempt grouping and goal-conditioned detection, timestamped highlights, native GTK/WinUI review UI, eight-category held-out evaluation |
| Conversion/tools | Native macOS Tools panel; typed preview/execute/history/Undo; source fingerprints, collision-safe new exports, original/export relationships; isolated cancellable photo/video workers, native H.264/AAC SDR video export and chapter JSON/WebVTT exports; Rust photo/chapter adapters with explicit limitations | Portable video, audio-only/trim/remux/proxy/RAW/HDR/document/archive/data/ebook/CAD adapters, stabilization, tiled enhancement, temporal consistency, subject tracking/reframing, port UI, Rust decoder isolation/cancellation, general restart reconciliation |
| Chat | Native macOS panel; contextual keyword/evidence retrieval including extracted document/OCR text; exact confirmed-People and user-edited event-title filters; explicit `MM:SS`/`HH:MM:SS` search over existing timestamped evidence; local history/clear; bounded loaded-model streaming and Stop; untrusted-content prompt boundary; Rust history/retrieval | Hybrid/semantic search, automatic event/timeline population, typed reversible execution, grounding and latency evaluation, portable generation, native Windows/Linux panels |

Sampled video captions are **unverified** and can miss an entire action; low-resolution visual-change signals only guide which moments receive model analysis. The catalog retains incomplete coverage; it must never convert absence of a sampled observation into a claim that an event did not occur. Manual markers are user assertions. Names come from confirmed People records, not guessed real identities.

## October 7, 2026 scope audit and implementation plan

Local implementation follow-up: macOS has an explicit significant-moments mode using overlapping four-second sequences, motion-peak placement, and quiet-interval fallback. One bounded child decodes up to eight frames; repeated low-fps timestamps are deduplicated before inference. Window coverage is persisted for resume; low-confidence drafts preserve user edits and are revision/job-state guarded. Invalid model JSON leaves coverage incomplete. IPC v1.8 mirrors `timelineMode` across DTOs. The Rust VLM adapter accepts ordered multi-image requests, but shared automatic timeline execution still rejects because its decoder/job worker is absent. Windows manual search, timestamp playback, chapter editing and Undo are coded and type checked against SDK metadata; full native build/runtime acceptance is pending. Linux UI, real-model recognition quality, tracks/cast and best-take ranking remain open. Photo enlargement is opt-in in both engines through IPC v1.9; omitted recipes retain downsizing. Native WinUI/GTK toolbox controls are implemented, with Windows runtime and Linux-host validation still pending. GTK internal-fixture enlargement and Undo passed on this Mac. Neural enhancement remains unimplemented. The approved pinned 4B model is verified in an isolated evaluation cache; loading is currently refused by the existing memory guard. This is uncommitted source work, not a release or hosted-CI claim.

The five requested features are **partially implemented, not complete**. This source audit used local commit `b1d00681` and inspected the native tools, timeline/chapter generation, naming, face-cache/clustering paths, and shared Rust capabilities. It did not run inference, measure recognition quality, certify the installed application, or recheck hosted CI. The installed release can differ from this source checkout.

| Original request | Source evidence | Completion status |
|---|---|---|
| Broad best-take detection | `catalog_take_scores` stores outcome and quality separately; catalog retrieval can read existing records. No production goal-conditioned grouping/ranking pipeline was found. | Foundation only. |
| Conversion, batch fixes, stabilization, upscaling, reframing | Swift `MediaTools` and Rust `commands/tools.rs` implement photo/chapter exports. Swift has limited native video export. Both explicitly report `videoEnhancement` unavailable; Rust also reports video export unavailable. | Basic conversions only; requested enhancement work remains. |
| Significant moments and a personal-movie cast/catalog | Swift `CatalogWorkbench`, `TimelineAnalysis`, `TimelineSpeechTranscription`, and `TimelineChapterSuggestions` provide manual markers, local speech passages, sampled captions, and low-confidence chapter drafts. Draft transitions use caption-word overlap, not dense activity recognition. | Partial moments workflow; reliable automatic moments, synopsis/storyboard/cast remain. |
| Better smart filenames | Swift/Rust naming rejects generic suggestions and shortens stems to at most 60 characters, with collision handling. | Concision fixes implemented; preferences, distinguishing evidence, companion handling, and human preference validation remain. |
| Accurate, fast faces | Shared alignment fixtures, cache provenance guards, conservative clustering and identity-preserving persistence are implemented. `FACE_CACHE.md` explicitly leaves durable refresh, incremental assignment, tracking, and calibration open. | Correctness improvements implemented; accuracy/speed improvement is not yet established by a held-out benchmark. |

### Shared design

Reuse the existing catalog instead of building five independent analyzers. The engine owns a small interface for requesting analysis, reading timestamped evidence, recording corrections, and previewing/executing reversible operations. Platform adapters implement decode, inference, and export. Native views consume those results; they do not own inference or write the catalog.

The common evidence record must distinguish file revision, media interval or photo, model/processing version, observed subjects/objects/actions, confidence, analysis coverage, and user corrections. An observation is not an inferred outcome; an inferred outcome is not a confirmed identity. Reuse existing entities where they fit, adding append-only migrations only for missing semantics. Every new command/payload starts in the canonical IPC schema with Swift/Rust/C# mirrors and wire fixtures.

Keep quick library scanning separate from deep analysis. Reuse decoded frames, embeddings, audio passages, and person/object tracks across features; invalidate only affected revisions/namespaces. Queue bounded work with persisted progress and memory/CPU/I/O/accelerator admission. Interactive search and review must remain usable during background analysis. New dependency/model proposals require approval and registry/license review before installation or integration.

### M0 — establish baselines and recovery guarantees

Build rights-clear internal-drive fixtures plus an evaluation manifest covering people, event intervals/outcomes, takes, names, and conversion properties. Split by recording session and identity where appropriate so adjacent frames/takes cannot leak across training/calibration and test sets. Include failures, uncertain examples, unsupported formats, and absent events. Do not use or mount Adlon example data.

Measure model-backed scan throughput, face extraction and assignment timings, cold/warm People loading, timeline recall, naming preference, export speed/disk usage, and foreground responsiveness on identified hardware. Report distributions and coverage, not just averages or synthetic writer rates. Mark benchmark targets as proposed until measured.

Complete source revision checks, operation publication reconciliation, disk-space estimates, quota cleanup, isolated decoding/cancellation, and resumable per-file batches. Recovery must distinguish completed outputs from staging files after a crash; cancellation must never publish an incomplete result. First deliverable: a reproducible baseline report and passing interruption/recovery fixtures.

### M1 — People accuracy and speed, plus concise naming

**People:** implement a durable model-separated refresh backlog with explicit missing/offline/failed states. Assign newly scanned faces incrementally against compatible trusted exemplars; do not repeatedly recluster the complete library. Retain user names, unknown/ignored records, negative constraints, and correction history. Preserve identity aliases through explicit merges. Add persistent merge/split/reassign/ignore review and uncertain-match queues. Small/blurred/profile/occluded faces should remain unknown when evidence is insufficient; proximity or clothing alone cannot establish identity.

Measure detection recall separately from correct identity assignment and wrong merges. Evaluate children across age changes, lookalikes, crowded images, varied skin tones, poor lighting, and cross-platform embedding spaces. Establish the held-out precision/recall gate already below, report each difficult subset and rejected-face coverage, and compare before/after latency on the same hardware/library. A new model is a benchmark candidate, not an assumed fix.

**Names:** introduce saved short/descriptive/date-first styles, a user-controlled length budget, and optional confirmed-person/event/date tokens. Prefer one distinguishing subject/action/reference over a caption sentence; reuse existing catalog evidence before making another model call. For example, `sam-baseball-hit.mov` requires both confirmed Sam evidence and a verified hit; otherwise use the supported description or retain the original. Offer editable batch previews with original/proposed/reason, learn explicit style corrections locally, and preserve extensions, Live Photo/RAW/JPEG/sidecar companion relationships. Handle Unicode, case-insensitive collisions, reserved names, and filesystem byte limits through the reversible operation path. Completion requires a blinded preference comparison against the current naming output and companion-safe apply/Undo.

### M2 — dense temporal evidence

Extend the current visual-change hints into evaluated shot/activity candidates. Combine low-cost motion/scene signals, on-device speech/audio cues, and periodic fallback coverage; motion alone misses quiet significant events and camera shake can create false candidates. Inspect candidate windows as frame sequences with enough temporal detail for the action, bounded overlapping chunks, and adaptive follow-up when the outcome is unclear. Capture lead-in, action, and aftermath. Persist examined intervals and failures so unexamined footage remains explicitly unknown.

Add person/object tracks with occlusion and shot-boundary handling; identity attribution uses compatible face evidence and confirmed People records. Keep speaker labels independent until a user or reliable evidence establishes the association. Measure event interval precision/recall, timestamp error, latency, memory, and missed fast actions on continuous recordings and separate clips. Deliver a reviewable timeline with evidence thumbnails and exact seeking before downstream automated rankings.

### M3 — Moments and the personal-movie catalog

Turn temporal evidence into reviewable chapters, significant-moment cards, synopsis, storyboard, and cast lists with each person's appearance intervals. Photos attach to event collections as stills rather than receiving invented video timestamps. Examples include gift opening, cake/candles, a catch, a speech, a performance section, and a pet doing a trick. Support user-defined searches such as “when the blue gift is opened” with timestamped evidence and explicit uncertainty.

Allow accept/edit/delete/Undo and lock user-edited moments against reanalysis. Add searchable transcripts and captions with correction; extend existing chapter JSON/WebVTT exports to actual speech subtitle exports and editor interchange only when those adapters exist. A synopsis must link back to evidence. Acceptance includes short actions, silent scenes, multiple simultaneous activities, overlapping speech, long recordings, and chunk-boundary events. Native Linux moments UI follows the macOS behavior; Windows delivery remains scheduled for the owner's PC session.

### M4 — broad, goal-conditioned best takes

Support an open vocabulary of user goals alongside tested starter categories. “Best” must express intent: a successful hit, a complete catch, an uninterrupted speech, a sharp group portrait, or a meaningful reaction. No finite model or category list establishes reliable coverage of literally every event. Show support/uncertainty and permit arbitrary goals, manual grouping, and corrections.

Group candidate takes using capture time/session, content similarity, subjects, and event evidence; proximity alone is insufficient. Separate alternate viewpoints, excerpts, and true repeated attempts. Compare event occurrences within long recordings as well as whole clips. Infer desired-outcome evidence from temporal sequences, keep aesthetic/technical quality separate, and rank successful outcomes before quality when success is the selected goal. Offer alternate “best quality” and “most meaningful” preferences. A sharp miss must not outrank a visible hit under a hit goal.

Return ties or “not enough evidence” instead of forcing a winner. Explain each recommendation with the relevant interval and evidence; allow correcting group membership and preferred takes. Corrections inform local preferences without corrupting the held-out evaluation. Generate optional highlight excerpts with adjustable lead-in/out and retained originals; never automatically delete lower-ranked clips.

Evaluate at least eight categories: sports outcomes, gifts/celebrations, performances, speeches/interviews, pets, demonstrations/tutorials, action/tricks, and portraits/group photos. Each includes successful, unsuccessful, ambiguous, partially occluded, and out-of-view examples. Report grouping quality, top recommendation accuracy, interval recall, false-success rate, ties, and abstention coverage by category. Use the proposed 90% clear-outcome target below; difficult and unknown categories remain visible in the report.

### M5 — a reliable batch conversion and repair toolbox

Extend the existing preview/execute/history/Undo machinery with per-platform capability discovery, saved recipes, destination/size estimates, per-file progress, pause/cancel/resume, and actionable failures. Never route unsupported recipes through a fallback that changes their meaning. Export new derivatives by default with source lineage and full recipe/model versions.

| Delivery tier | Work | Fidelity gate |
|---|---|---|
| Common media | Portable video transcode, trim, lossless remux where possible, proxies, audio extraction/conversion, rotation/orientation, conventional resize/compression | Reopen outputs; check streams, timing/sync, aspect/orientation, quality, and metadata policy. |
| Repair | Diagnose container/index problems, rotation metadata, interlacing, audio levels/noise, and recoverable corruption; show an exact repair plan | State what was recovered or lost. A damaged source cannot always be repaired; preserve it. |
| Advanced media | RAW and color-managed images, HDR-preserving output or explicit SDR tone mapping, multi-track/subtitle preservation, alpha and animation | No silent HDR-to-SDR, ICC loss, track removal, flattening, or frame-rate changes. |
| Broader formats | Prioritize document/PDF/OCR and archive/data conversion, then ebook and 3D/CAD adapters by demand and supported pair | Define semantic/layout fidelity per format; conversion is not universal editability or losslessness. |

Audit portable media options against the locked stack and native APIs first. FFmpeg is a candidate requiring a separate dependency/build/license/distribution decision, not an approved addition. Its official [legal page](https://ffmpeg.org/legal.html) documents LGPL/GPL build-dependent obligations; its [filter documentation](https://ffmpeg.org/ffmpeg-filters.html#vidstabdetect) states that the two-pass vidstab filter requires libvidstab. Do not assume a generic downloaded binary provides the required filters or an acceptable distribution configuration. This audit installs no package or weight.

### M6 — stabilization, enhancement, and social reframing

**Stabilization:** estimate camera motion independently of moving subjects, smooth intentional pans conservatively, preview crop/zoom strength, reset at shot cuts, and allow overrides. Test walking/handheld footage, tracking pans, low-texture scenes, rolling shutter, severe blur, and frame edges. Decline unsupported severe damage rather than label it fixed.

**Photo/video upscale:** compare conventional resampling to vetted on-device enhancement candidates. Start with tiled photos and bounded memory; add video only with temporal consistency across frames and scene resets. Provide scale/strength controls and before/after crops. Gate on changed faces/text, hallucinated detail, halos, flicker, memory, and throughput; do not mistake larger dimensions for recovered detail. Record processing as enhancement and retain the original.

**Reframing:** offer 9:16, 16:9, 1:1, and 4:5 output recipes with subject/person selection, multi-subject safe areas, smooth tracked crop paths, and editable keyframes. Detect lost subjects and use fit/padding when a crop would remove essential content. Reuse person/object tracks and compute stabilization/reframing transforms in compatible coordinates. Each aspect variant derives from the original or a verified common master, avoiding repeated lossy conversion. Vertical-to-horizontal export can crop or pad; it cannot recover scenery outside the source frame. Generative scene extension would be a separately labeled future feature.

Validate crop jumps, cut handling, occlusions, moving groups, subtitles/text safe areas, and audio sync. Deliver portable execution and the native review UI with the capability, rather than announcing enhancement from schema fields alone.

### Additional improvements worth including

| Priority | Improvement | Reason and completion condition |
|---|---|---|
| Required foundation | Evidence/coverage indicators and correction history | Users can see what was actually examined and correct an inference once across names, moments, search, and ranking. |
| Required foundation | Resumable queues, quotas, disk estimates, unavailable-drive handling | Large libraries should survive interruptions without rescanning everything or filling the drive. |
| Same release | Original/derivative families and companion-aware operations | A video, its portrait export, thumbnail/proxy, RAW/JPEG pair, and sidecars remain connected; hide derivative duplicates without deleting them. |
| Same release | Event collections, timelines, and natural-language moment search | Combine photos and clips of the same occasion; reuse existing catalog/search/chat and make result intervals seekable. |
| Same release | Subtitle/transcript review and audio cleanup | Reuse timestamped speech; captions and normalized audio make exported highlights more useful. |
| Same release | Local correction/style preferences with reset/export | Explicit names, rejected face matches, preferred takes, and naming styles persist and are auditable. No cloud training or telemetry. |
| Same release | Before/after preview, keyboard review, and accessibility | Make large-batch decisions quick, readable, and reversible in each native app. Preserve palette, springs, and LavaLampBackground. |
| Later | Highlight assemblies, best-photo burst selection, export bundles | Build on proven take ranking and derivatives; keep user ordering and review before export. |
| Later | Library backup/restore and portable catalog export | Include identities, corrections, recipes, and lineage; verify restore on another platform and reconcile unavailable paths. |
| Later | Deeper document/archive/data/ebook/3D/CAD conversion | Expand by measured demand and a supported-format matrix after core media fidelity is accepted. |

### Next implementable slice and delivery discipline

The next focused code slice remains **evaluated timeline candidates and dense follow-up evidence**: internal fixtures with labeled significant intervals; candidate generation with fallback coverage; bounded sequence inspection; persisted provenance/coverage; a reviewable native result. It should demonstrate both a fast sports action and a quiet gift-opening moment, retain uncertainty for a hidden outcome, survive cancellation/restart, and preserve user edits. That unlocks M3/M4 and evidence-aware naming; People backlog/calibration and basic conversion recovery can advance as separate workstreams.

These milestones are dependency order, not date promises. Baseline results and approved runtime/model choices determine estimates. Implement macOS as the reference and carry slices into both native ports. The active owner goal now requests implementation on all platforms and supersedes the earlier Windows feature-work deferral; Windows runtime/GPU acceptance still requires Windows hardware. Count a feature complete only after its backend, native UI, correction/recovery flow, quality evaluation, hardware behavior, and existing release gates pass. No commit, push, merge, release, model download, or protected-data access is implied by the planning audit.

## Adlon policy

`/Volumes/Adlon` is read-only example data. Never place a database, cache, thumbnail, tag, sidecar, log, model, export, or temporary output there; never rename, move, repair, or delete its contents. Resolve existing aliases and missing destination suffixes before admitting writes. Protect project-managed originals from automatic mutation. Run modification tests only on internal-drive fixtures. Current unit tests use temporary protected roots and stored example paths, without creating anything on Adlon. Application-wide zero-write certification is still a release gate.

## Delivery sequence

1. Finish safeguards, dependency/license inventory, accuracy labels, and measured baseline fixtures. Route remaining legacy UI database mutations through the engine.
2. Complete revision/evidence storage, persistent model-separated indexes, and the resource scheduler. Recover interrupted work without publishing partial outputs. Add capability reporting and reject unsupported operations explicitly.
3. Improve People accuracy and loading, hybrid search, the local chat panel, and naming preferences. Implement preview/execute/Undo through typed engine operations; permanent deletion and overwriting originals require explicit confirmation.
4. Extend timeline analysis with timestamped speech, scene changes, bounded overlapping chunks, tracks, dense event analysis, synopsis/storyboard, chapters, and exports. Share the resulting evidence across search and naming.
5. Implement language-conditioned best takes and media tools. Keep desired outcome separate from quality. Export new versions by default; preserve color, timing, audio synchronization, metadata policy, and original/derived relationships.
6. Add broad format adapters and saved recipes, then finish native port behavior, hardware acceptance, distribution obligations, signed packaging, privacy scans, and hosted CI.

Each milestone must work on macOS and then in both ports before the full release. A Rust backend contract does not establish WinUI or GTK runtime parity.

## Acceptance gates

Targets below are proposed release criteria, not current measured claims.

- Adlon: zero application writes, including symlink aliases, companion outputs, and invalid destination paths.
- Catalog: migrations preserve files, names, identities, corrections, and undo; actual cross-platform database round trips pass. Changed revisions invalidate observations while retaining user corrections.
- Jobs: interruption resumes safely; cancellation terminates workers and leaves no published partial output. Disk estimates, bounded caches, per-file errors, and resumable batches work.
- Faces: at least 98% identity precision and 90% recall on a held-out usable-face corpus; report coverage separately.
- Best takes: at least eight event categories; 90% correct top recommendations for clearly visible unambiguous outcomes, with abstentions and difficult cases reported separately. Never delete rejected takes automatically.
- Moments: fast actions, long recordings, scene cuts, silence, overlapping speech, and chunk-boundary events pass.
- Search/chat: warm retrieval p95 at most one second on 100,000 files; warm chat first response p95 at most three seconds on a 16 GB M1 Pro during background work. Validate changes against resolved selections.
- Names: every applied name passes filesystem/collision checks; a blinded preference review beats current proposals. Preserve original extensions and established companions.
- Outputs: reopen each conversion; verify streams, duration, synchronization, orientation, color, metadata, and format-specific fidelity. Report unrecoverable data honestly.
- Enhancement/reframing: test subject loss, crop jumps, flicker, changed faces/text, handheld footage, and safe areas. Offer fit/padding when crops lose essential content. Vertical-to-horizontal export cannot recover unseen scenery.
- Performance: preserve the fast-scan baseline and pursue at least 140 files/s independently of deep analysis.
- Hardware: Apple Silicon, CPU-only, NVIDIA, AMD, Intel, Windows ARM64; advertise NPU paths only after model/device validation. CPU fallback remains functional, with different speed and quality tiers.
- Distribution: offline network-capture tests, model hashes/licenses/acceptance policy, dependency obligations, signing, no telemetry strings, and green native/IPC/packaging CI.

Benchmark replacement models on identical fixtures and hardware. Promote only after all quality gates and either a meaningful accuracy gain or at least 20% lower latency without material quality regression. Published benchmarks do not establish FileID task performance.

## Validation commands

- macOS: `FILEID_TEST_ENGINE_PATH=/tmp/fileid-next-build/debug/FileIDEngine swift test --build-system native --scratch-path /tmp/fileid-next-build --jobs 4` from `platforms/apple`. Internal scratch space avoids the Desktop FileProvider's resource-bundle signing interference. The native SwiftPM build system also avoids the default SwiftBuild stale-module failure seen during overlapping checks. Run validation sequentially against one build directory.
- Rust: `cargo test`, `cargo clippy --all-targets -- -D warnings`, and `cargo fmt --check` from `platforms/windows/src/engine`.
- C#: IPC project build/tests and formatting; full WinUI app checks require a Windows environment.
- Shared: `python3 shared/scripts/check_catalog_schema.py`, documentation/model-license/privacy policy checks, and wire conformance on each platform. `check_catalog_roundtrip.py --swift-engine <binary> --rust-engine <binary>` verifies actual engine interoperability and Undo using an internal temporary catalog; the host-macOS round trip passed.
- Hardware: actual UI/model processing, scheduler contention/memory pressure, protected-source write tracing, output fidelity, accuracy datasets, and release packaging. Test outputs belong on the internal drive.

The isolated decoder can be exercised with `FileIDEngine --sample-video <internal-source> <seconds> <internal-output.png>`. It reports actual frame time, duration, size, and modification time as JSON and rejects protected outputs before opening a source.

## Initial lexical retrieval measurements

October 1, 2026, development binaries on the 16 GB M1 Pro: 100,000 synthetic unique file records, 1,000 chapter records, 1,000 page-passage records, and 100 warm IPC requests per engine. These measure keyword retrieval and serialization only, with no model-based semantic/person retrieval or application background analysis. They do not satisfy the full hybrid-search or chat release gate, and they do not compare model quality.

| Engine | Median | p95 | Maximum |
|---|---|---|---|
| Swift | 22.98 ms | 113.07 ms | 137.89 ms |
| Rust | 34.15 ms | 238.22 ms | 435.01 ms |

Reproduce using `shared/scripts/benchmark_catalog_search.py --runtime swift|rust --engine <native-binary>`. The script creates and removes its own internal temporary catalog; it never creates the simulated source media. Validate loaded-model contention and representative real-data retrieval separately.

## Export milestone

See [TOOLS.md](TOOLS.md) for the exact supported format matrix, metadata/color limits, worker/cancellation differences, persistent history, Undo recovery, and remaining restart gates. Actual Swift/Rust cross-engine preview/execution/Undo passed using internal PNG fixtures. Full release scope remains unfinished.

Face alignment follow-up: overlapping/unique landmark correspondence, one rendered pixel buffer per image, finite geometry and malformed bbox rejection, and shared Swift/Rust fixtures are implemented. Existing cached embeddings remain; versioned rebuild/incremental assignment and measured identity accuracy remain pending.

Face comparison has conservative whole-pass provenance/revision/vector guards and persist-time revalidation. It does not yet provide durable complete-library refresh, incremental stable-ID assignment or held-out calibration; incompatible legacy/offline caches can defer clustering (FACE_CACHE.md).

### GTK manual catalog follow-up — 2026-10-07

The Linux native Search & Moments panel now exposes the existing Rust manual chapter/search/recovery APIs. Actual GTK widget smoke testing on this Mac verified create/update/delete/Undo and timestamp evidence search against an internal synthetic photo; the updated chapter retains its ID and user provenance. Selecting chapter evidence opens that marker for editing. Preview decoding is bounded and late callbacks are ignored. Linux-host validation, video playback, automatic portable moment jobs, and cast/event/take editing remain pending.

### Native face refresh follow-up — 2026-10-07

macOS refresh now advances through successive batches rather than stopping after its first 5,000 faces. Canonical v24 failure records survive restart, prevent early failed faces from starving newer rows, and become inapplicable after source/bbox/model changes. Successful persistence clears the delay while keeping People and manual corrections. Cancellation leaves unfinished work eligible. This is cache scheduling/recovery work, not calibrated recognition accuracy; portable refresh and progress/coverage/retry controls remain pending.

### Smart-name subject-prefix follow-up — 2026-10-07

Matching Swift/Rust regressions now cover full confirmed-name prefixes, generated first-name pairs, conjunctions, and surname/event overlap. Names preserve `Jones Beach` when a confirmed person is `Alex Jones`; two-person prefixes do not repeat their names. This formatting correction does not complete preferences, companion-file rename groups, or human preference testing.

### Imported face orientation follow-up — 2026-10-07

Native refresh now maps explicit raw-pixel boxes through source EXIF orientation into ImageIO's upright decoded coordinates. It preserves native normalized boxes and rejects rotated dimensionless legacy pixel boxes instead of guessing. Native processing provenance advances to v3 to refresh older vectors safely. Actual JPEG fixtures under all eight orientations verify the selected crop region, independently of the box math. Portable upright decoding/rebuild and real recognition-quality acceptance remain pending.
