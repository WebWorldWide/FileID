# FileID — State

> Snapshot of what's working and where we left off. Update at the end of every working session.
>
> **How to read this file:** newest entry at the top. Each entry is a one-day-or-one-release summary of what landed. For *why* a decision was made, see [`DECISIONS.md`](DECISIONS.md). For *what's next*, see [`NEXT.md`](NEXT.md). For *user-visible release notes*, see [`/CHANGELOG.md`](../../CHANGELOG.md).
>
> Older entries below V15.0 are historical context — load-bearing for archaeology, not for current state. Skim if you want the journey; skip if you want the destination.
>
> **Trimmed to a lean baseline (2026-05-21).** Only the most-recent entries are kept here; everything older lives in `git log`.

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
- **macOS mirror (unverified — needs a Mac build).** `RestructureSemantic.swift` ports the
  engine faithfully (reuses `IdentityClustering`); `proposeAll` runs it + stamps
  confidence/reason; IPC `RestructureMove` gains confidence/reason; parity tests added.
  App-side UI wiring + Sankey palette documented in `platforms/apple/MACOS_BUTLER_NOTES.md`.
- **Docs rewritten from scratch** against verified source: all three `CLAUDE.md` +
  SHIP/PRIVACY/SECURITY/CONTRIBUTING/TESTING/COVERAGE/SYMBOLS/VISUAL-LANGUAGE/BUGS. Honest
  findings surfaced: model-download SHA256 is wired but inert (every `registry.rs` entry is
  `sha256: None`) — now the top open hardening item; the old "Phase 8 coverage gate" was fictional.
- **Condense pass** (engine, behavior-preserving): match-arm merges, if/else→match,
  loop→iterator, push-loop→`extend`.
- **Verified headless throughout**: clippy `-D warnings`, 230 engine tests, `dotnet build`/`test`
  (133), `dotnet format`. **Not yet** verified on-hardware (butler plan quality on `G:\TrueNAS`)
  or on a Mac (Swift).

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
