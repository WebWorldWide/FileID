# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

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
