# macOS (Swift) Parity/Correctness Audit — 2026-07

Static review of `platforms/apple` against the Rust engine reference
(`platforms/windows/src/engine`). Produced during the cross-platform
production-hardening pass on a Windows dev box, so **every code finding here is
UNVERIFIED-UNTIL-MAC** in the runtime sense — the code paths were traced by hand
but nothing was compiled or run. `macos.yml` CI is the compile gate; embedding
parity and runtime behavior need a Mac.

One finding was applied as a conservative Swift change (F4 — a dim guard mirroring
the existing SFace pattern; only compile-checks here). F5 was investigated and
**deliberately NOT applied** — it would have introduced cross-platform divergence
(see F5). F1 is the one real behavioral fix and is **deliberately left for an on-Mac
session** because it touches the content-hashing pipeline across several arms and a
blind edit could double-read images or skew dedup. F2/F3 doc reconciliation was
applied.

Prior audits of this tree produced many false positives; this pass explicitly
re-verified each item against the code and **overturned two planned assumptions**
(see "Dissolved" below) — do not reintroduce them.

---

## F1 — [CORRECTNESS, macOS-local] `content_hash` computed for images only → Cleanup "Exact" misses non-image duplicates
**Left for on-Mac session (top priority).**

`ContentHash.compute(...)` has exactly one call site: `Pipeline/Tagging.swift:241`,
inside `processImage`. The sibling pipelines — `processVideo`, `processPDF`,
`processDoc`, `processAudio` (`Tagging.swift:103`), `processModel`
(`Tagging.swift:145`), and the `.other` arm (`Tagging.swift:66`) — build
`TaggedFile` without `contentHash`, so it defaults nil and `DBWriter.swift:603`
writes NULL.

The Windows reference computes `content_hash` for **every content-bearing kind**:
`pipeline/tagging.rs:1034–1103` covers Image, `Doc | Pdf | Audio`, and the
catch-all `_ =>` arm (video/model). Only OneDrive online-only placeholders are NULL.

macOS Cleanup's exact-duplicate query groups by `content_hash` with no kind filter
(`Database/ReadStore.swift:235–248`, `:565–599`), and the tab advertises "Exact
groups byte-identical copies" — so byte-identical duplicate PDFs, documents,
videos, audio, and 3D models are invisible in Exact mode on macOS while Windows
surfaces them. This is a within-macOS behavioral gap, not merely cross-platform.

**Fix (on Mac):** mirror the Windows `match file.kind` arms. Cleanest is the single
choke point `processFile` (`Tagging.swift:74–78`, where `fileRef` is already
stamped for all kinds): compute `ContentHash.compute(url:size:)` for every
content-bearing kind when `tagged.contentHash == nil` — avoids double-reading
images (which hash inline at 241) while covering doc/pdf/audio/video/model.
`ContentHash.compute` already handles the ≤16 MB full vs >16 MB composite split.

## F2 — [DOCS-DRIFT, applied] Commercial-clean model swap has LANDED and is primary; docs said "pending"
The code is source of truth and shows the swap is merged + wired as the primary
stack, prewarmed at startup (`FileIDEngineMain.swift:669–676`):
- Faces: `FaceEmbedderKind` is **sface-only** (`shared/…/AIModels.swift:147–176`,
  128-d, Apache-2.0); `ArcFaceService` (legacy name) loads/embeds SFace with an
  output-dim guard (`ArcFaceService.swift:241–246`).
- 5-point alignment ON by default (`FaceAlign.swift:28–30`, "validated on a Mac").
- Tagging: RAM++ Swin-Large 4585-class primary (`Tagging.swift:259–268`), Vision a
  fallback.
- CLIP: OpenCLIP **ViT-B/32** 512-d (`MobileCLIPService.swift:40–49`,
  `clip_vitb32_image.onnx`).

Reconciled `apple/CLAUDE.md`, `MODELS.md`, `SHIP.md` to say the stack is merged +
primary, with on-hardware embedding-parity confirmation the only remaining Mac task.

## F3 — [DOCS-DRIFT, applied] "macOS scan writes content_hash/file_ref NULL" is false; one stale in-code comment
`file_ref` (APFS/HFS `st_ino`) is computed at discovery (`Discovery.swift:267,
324–345`), propagated, and written (`DBWriter.swift:604`). `content_hash` is
computed for images (F1) and written (`DBWriter.swift:603`); both are indexed and
content_hash drives macOS dedup. Values are SHA-256 (`ContentHash.swift:4–10`), so
they simply don't match Windows BLAKE3 cross-platform. `DBWriter.swift:858–860`
carries a stale comment ("content_hash isn't computed by the scan path") that
contradicts `DBWriter.swift:552–556` and the actual `Tagging.swift:241` call — the
file_ref-only heal is a defensible conservative choice, but the stated reason is
wrong. **On Mac:** fix that comment. Docs' "both NULL" claim corrected here.

## F4 — [PARITY/hardening, applied] CLIP `embedImage` lacked the output-dim guard the SFace path has
`ArcFaceService.embed` bails on `count != sface.embeddingDim`
(`ArcFaceService.swift:241–246`, mirroring Windows `sface.rs` ENG-69), but
`MobileCLIPService.embedImage` only guarded `count > 0` — a wrong/substituted CLIP
ONNX with a different output width would be normalized and persisted as an
off-dimension `clip_embeddings` blob, silently poisoning semantic search +
restructure clustering. Added a `guard out.count == expectedDim` bail, symmetric
with the SFace guard. Reachable only via a corrupt/substituted model (low
likelihood), but the guard makes it fail cleanly. **UNVERIFIED-UNTIL-MAC.**

## F5 — [COSMETIC, NOT applied — would have made it worse] Stored CLIP model string "mobileclip_s2" is stale on BOTH platforms
The audit flagged `DBWriter.swift:841` storing `model = "mobileclip_s2"` while the
actual model is ViT-B/32, and worried a cross-platform consumer comparing the tag
would mark identical 512-d spaces incompatible. On verification the **Windows engine
stores the same `"mobileclip_s2"` literal** (`pipeline/dbwriter.rs:453`) — so the tag
is *consistent* across platforms, and changing macOS in isolation would CREATE the
very divergence the finding feared. No query gates on the `model` string
(grep-confirmed both sides), so it's decorative. **Left unchanged on both.** If it's
ever corrected to the real model id, it must be done on both platforms together
(coordinated, low-priority, no data migration since dims are unchanged). Same for the
legacy class/dir names (`MobileCLIPService`, `ArcFaceService`, `mobileclip_image/`).

## F6 — [PARITY-ONLY] Face bbox: two deliberate divergent contracts — do NOT unify
- **macOS writes** normalized `[0,1]`, bottom-left origin, CSV `"x,y,w,h"`
  (`VisionWorker.swift:182–183`; Vision is normalized bottom-left).
- **Windows writes** pixels, top-left origin, JSON `{x,y,w,h,roll,yaw,pitch}`
  (`FaceBBox.swift:3–5`).
- macOS read-tolerance converts both to its canonical (normalized bottom-left) via
  `FaceBBox.parseNormalized` (`FaceBBox.swift:29–48`; tests in `FaceBBoxTests.swift`).

Within-macOS this is consistent (write normalized-CSV, read normalized-CSV) — **not
a bug**. This is the divergent-but-correct state a prior JSON-unification broke and
was reverted. One cross-platform edge: the "different people" verdict anchor
resolver (`FaceClustering.swift:705–719`) matches `face_prints.bbox` by string
equality, so a Windows-authored `face_verifications.bbox_*` (JSON pixels) never
string-matches a macOS `face_prints.bbox` (CSV normalized). But macOS has no verdict
**write** path, so in a pure-macOS library those columns are NULL and it falls back
to legacy face ids — harmless. No fix; documented only.

---

## Dissolved on inspection (planned assumptions that were WRONG — do not reintroduce)

- **"Restructure is computed app-side; engine confidence/reason ignored" — FALSE.**
  The engine computes the plan: `FileIDEngineMain.swift:446–462` handles
  `.planRestructure` via `Restructure.proposeAll(...)` → `.restructurePlan`. The app
  only **renders** it (`RestructureView.swift:1023, 1030–1041`). No app-side plan
  computation exists (grep for `RestructureEngine`/`func compute` finds only
  `SankeyFlowView.computeLayout`, pixel geometry). Confidence/reason/tier are carried
  and used (ask-band moves start deselected, `RestructureView.swift:1047–1049`). The
  `FileIDEngineMain.swift:481–484` "accepted for wire parity and ignored" comment
  applies **only** to the `useSymlinks` bool, not to `applyRestructure` — the engine
  does real filesystem moves (`:493–502`). No divergence risk; there is no second plan.

- **"Sankey palette diverges (macOS gold+orange vs Windows Okabe-Ito)" — FALSE.**
  macOS also uses the Okabe-Ito CVD-safe palette for destination nodes/ribbons
  (`SankeyFlowView.swift:30–40`, `658–668`). `Theme.gold` is only brand-accent chrome
  (focus halos/strokes). No divergent category palette.

- **`RamPlusModelInstaller.swift:169` `print()` "bypasses redactPathForLog" — NON-ISSUE.**
  It's `#if DEBUG`-only (never ships), logs an error description with no user path,
  and there's no other logger in the app to route to. Left as-is.

## Swift hygiene — clean (re-verified)
Zero `fatalError`/`try!`/`as!` in engine + app. 3 `precondition` (constructor
invariants on programmer error). ~13 force-unwraps, all guarded/idiomatic
(`.urls(...).first!`, `raw.baseAddress!` behind `count > 0`, `best!` behind
`best == nil ||`, `Character.unicodeScalars.first!`, dictionary-key-by-construction).
Actor isolation: `nonisolated(unsafe)` counter is `NSLock`-guarded;
`@unchecked Sendable` Vision boxes are documented single-owner. No new risks.

## On-Mac action order
1. **F1** — the real behavioral fix (non-image Exact dedup). Straightforward once you can build.
2. **F3** — fix the stale `DBWriter.swift:858` comment (F2/F3 docs already reconciled).
3. Confirm **F4/F5** compile + behave (they were applied blind here).
4. On-hardware embedding-parity check for the commercial-clean stack (the last real Mac task from the lockstep).
