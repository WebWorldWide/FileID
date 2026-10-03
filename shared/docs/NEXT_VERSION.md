# FileID next version — implementation ledger

Accepted direction: a local catalog connects files, people, events, temporal evidence, document passages, and derived versions. Develop and validate macOS first, then mirror each milestone into Windows and Linux. Preserve native interfaces, the existing palette, springs, and LavaLampBackground. No telemetry; inference is offline. User-initiated Hugging Face model downloads remain the only application network feature.

## Current implementation

The foundation is on main through merged [PR #186](https://github.com/WebWorldWide/FileID/pull/186). The stable People follow-up was merged; Adlon Windows bootstrap is [PR #188](https://github.com/WebWorldWide/FileID/pull/188). These are bounded milestones; the complete release remains unfinished.

| Area | Implemented | Remaining |
|---|---|---|
| Example-data safety | Swift/Rust path guards for Adlon, existing aliases, fail-closed dangling symlinks, and managed media originals; guards on database creation, file mutations, and selected cache/model/log writes | Whole-application write tracing, companion-file tests, race-resistant descriptor-based mutation, Windows volume identification |
| Catalog | Canonical v21/v22/v23 SQL, Swift/Rust migrations, provenance, coverage, chapters, passages, tracks/observations, events/takes, derivatives, jobs, recipes, corrections, operations, local chat records | Populating most new entities, native Windows/Linux-host migration acceptance, revision changes for same-size/same-mtime replacements |
| Interfaces | IPC v1.7 catalog/tools/chat and scoped folder/destination bookmark DTOs mirrored in Swift, Rust, and C#; Linux inherits Rust DTOs | Full hybrid search, person/event/take editing, typed conversion operations, capability reporting, portable generation, port UI |
| TestFlight distribution | Store-only app sandbox entitlements, scoped library/output bookmarks, retained helper access across engine restarts, container model-cache variant, signed-package/upload script; registered `com.fileid.app`, App Store Connect macOS record `6818813859`, internal group `FileID Internal Testers`; local profile/app/installer signing assets; strict local package validation | Team API key creation awaits owner confirmation because it grants access to every team app; upload/processing, tester installation, sandbox runtime acceptance |
| Search | Persistent incremental FTS plus native engine-owned CLIP HNSW snapshots/deltas; Library hybrid/similarity IPC, timestamp/page evidence, stale/failed exclusion | Text/catalog indexes, richer exact/person/event filters, durable cancellable index jobs, offline availability, end-to-end 100,000-file benchmark |
| Chapters | Native macOS Search & Moments panel; playback seeking, manual markers and summaries, durable correction history and Undo; draft chapters from confirmed transitions between sampled captions, guarded by file revision and protected from overwriting user edits; timestamped on-device speech passages are stored as searchable evidence; engine-backed Rust equivalent | Reliable dense scene/activity chapters, synopsis/storyboard/cast, speaker attribution, subtitles/chapter/FCPXML export, Windows/Linux UI |
| Timeline | macOS durable frame-sampling jobs, pause/resume/cancel, interrupted-job recovery, immutable model provenance, reusable frame captions, bounded isolated decoder process, parent-death termination, abandoned-frame cleanup; sparse chapter drafts are explicitly low confidence and may miss brief events; video audio is transcribed locally in overlapping bounded chunks with per-chunk coverage, verified-chunk resume, and incremental job progress | Shot/activity proposals, dense candidate analysis, alignment, face tracks, passage correction UI, stronger per-file errors, model-based timeline quality checks, portable worker |
| Renaming | Concise evidence-based stems, 60-character limit, abstention on generic/unchanged suggestions, same-directory collision suffixes; Swift/Rust formatting | Preference settings/learning, date-first style, companion groups, blinded preference review, operation unification |
| Faces | Shared alignment fixtures, overlap/ambiguity-safe landmark matching, one pixel render per image, finite geometry checks, deterministic Rust HNSW; actual SFace weight/processing/source cache keys, bounded native refresh and model-separated catalog observations; whole-pass model/revision/vector guards and persist revalidation; stable identity reuse, correction-aware partitions, retained unknown/offline records and transactional updates | Durable cache rebuild/backlog, incremental namespace partitioning, identity aliases for explicit merges, held-out calibration, incremental assignment, trusted exemplars, video tracking, persistent correction UI |
| Local AI | Existing model stack retained; updated research shortlist; shared RAM headroom checks and cancellable native model residency gate | Benchmarks, pinned candidate packs, portable generation adapter, hardware capability probes, memory-budgeted multi-model routing |
| Scheduling | Persistent timeline state/checkpoints; queued interactive chat priority on the macOS major-job queue | Durable CPU/I/O/GPU/NPU budgets, fairness, multi-model residency/eviction, parallel admission, restart recovery for general operations |
| Best takes | Event/take records with separate outcome and quality fields | Goal-conditioned grouping/ranking, eight-category evaluation, ties/abstention, highlights and corrections |
| Conversion/tools | Native macOS Tools panel; typed preview/execute/history/Undo; source fingerprints, collision-safe new exports, original/export relationships; isolated cancellable photo/video workers, native H.264/AAC SDR video export and chapter JSON/WebVTT exports; Rust photo/chapter adapters with explicit limitations | Portable video, audio-only/trim/remux/proxy/RAW/HDR/document/archive/data/ebook/CAD adapters, stabilization, tiled enhancement, temporal consistency, subject tracking/reframing, port UI, Rust decoder isolation/cancellation, general restart reconciliation |
| Chat | Native macOS panel; contextual keyword/evidence retrieval; exact confirmed-People and user-edited event-title filters; explicit `MM:SS`/`HH:MM:SS` search over existing timestamped evidence; local history/clear; bounded loaded-model streaming and Stop; untrusted-content prompt boundary; Rust history/retrieval | Hybrid/semantic search, automatic event/timeline population, typed reversible execution, grounding and latency evaluation, portable generation, native Windows/Linux panels |

Sampled video captions are **unverified**. Sampling every ten seconds can miss an entire action. The catalog retains incomplete coverage; it must never convert absence of a sampled observation into a claim that an event did not occur. Manual markers are user assertions. Names come from confirmed People records, not guessed real identities.

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
