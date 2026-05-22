# Next Up

> Top priorities, in order. Each has explicit acceptance criteria.

---

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
