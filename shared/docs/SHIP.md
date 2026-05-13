# FileID — Ship Plan to 1.0

> The finish-line plan. Locked 2026-04-29.
> Goal: open-source release that feels like a team built it. Five-star App-Store-class polish without the App Store.
> No scope expansion. Anything not on this list is v1.1.

---

## Vision

A macOS-native, fully-local AI photo + file organizer that makes you feel like a friend with deep ML experience reorganized your entire library while you slept. Beautiful, fast, accurate, respects your existing organization decisions, never sends a byte off your Mac, free and open source.

Quality of a Tweetbot, Rogue Amoeba, Panic, or Sequel Pro release.

---

## Quality bar (the bar each track is held to)

For every shipped feature:

1. **Works on first run, every time.** No "click twice if it didn't work."
2. **Empty / loading / error states are designed**, not afterthoughts.
3. **No dev-tool affordances visible** — no debug log paths, no internal IDs, no "phase: clustering" jargon in user-facing copy.
4. **Accessible**: VoiceOver reads every meaningful element; keyboard nav works for the primary flow; meets WCAG AA contrast.
5. **No silent failures.** If something can fail, the failure is surfaced, dismissible, and explains what to do.
6. **Animation + transition states** match macOS Sequoia design language.
7. **Documented**: feature appears in README + has at least one screenshot.

For overall product:

- **Crash-free for 1 hour of normal use** on a fresh DB on a 50K-file library.
- **Memory bounded**: peak RSS < 2 GB during scan; idle RSS < 400 MB after scan.
- **Cold start to first usable UI**: < 2 s on M1.
- **Signed + notarized + stapled** DMG, downloadable from a public GitHub release.
- **README + LICENSE + CONTRIBUTING + screenshots + demo video** in the repo.
- **Code of Conduct** + issue templates + at least one tagged "good first issue".

---

## Tracks (ordered, capped)

Order matters: each track unblocks the next. We do them sequentially.

### T1 — Fix the face-clustering "one cluster" bug **[blocks T2-T5]**

**Problem.** After M5 landed, `runFaceClustering` produces 1 person where it should produce hundreds.

**Approach.**
1. Run the diagnostic script (`scripts/diagnose_arcface.py`) against the live DB + .mlpackages to find which of these is true:
   - All ArcFace embeddings are nearly-identical (model degenerate)
   - Embeddings are distinct but cosine threshold collapses them anyway (CW config wrong)
   - Embeddings differ by tiny amounts (preprocessing wrong, e.g., channel order, normalization range)
2. Apply the targeted fix.
3. Add a "diversity check" to `scripts/convert_arcface.py` so the conversion fails fast if outputs aren't discriminative across distinct inputs.
4. Re-run on real library; assert person count > 50 on a library known to have many people.

**Definition of done.**
- 60K library produces ≥ 50 distinct persons after clustering.
- Hand-spot-check 20 person cards: at least 90% are coherent (one identity per cluster).
- Diversity check in conversion script.

---

### T2 — Automated test corpus + iteration loop **[unblocks confidence on every later track]**

**Problem.** We keep regressing and don't notice until the user notices. We need a real test that runs end-to-end on a known-good corpus and asserts on the result.

**Approach.**
1. Build `Tests/Corpus/` — 200 representative real-world files (mix of photos with faces, screenshots, PDFs, videos, audio, docs). Use a known free / public-domain set; check it into the repo (or pull at test time). Includes:
   - Multiple photos of the same person across lighting / age (for face cluster validation)
   - Some near-duplicates (for Cleanup validation)
   - Some mis-organized folders + some well-organized folders (for Restructure validation)
2. `scripts/iterate.sh` that wipes DB, scans the corpus, asserts:
   - Tagging throughput in expected band
   - Person count in expected band
   - Cleanup duplicate count matches a hand-counted truth
   - No crash reports in `~/Library/Logs/DiagnosticReports/` since launch
   - Memory ceiling not crossed
   - DB FK / orphan integrity
3. Run on every code change before declaring done. Wire into a git pre-push hook (optional but nice).
4. A second script `scripts/soak.sh` that runs the iterate loop 10 times back-to-back to catch resource leaks.

**Definition of done.**
- `bash scripts/iterate.sh` runs in < 5 minutes and emits a green/red summary.
- `bash scripts/soak.sh` runs in < 1 hour and asserts memory stable across runs.
- Both scripts pass on `main`.

---

### T3 — Restructure: Assistant Mode **[the headline feature]**

**Problem.** Current Restructure is a steamroller — every file goes through `People/Mom/2018 / Places/x_y/2019 / Documents/2023 / Photos/y/m / Misc`. That destroys the user's existing meaningful organization.

**The vision (verbatim from user):**
> "Acts like an assistant. Takes inspiration from existing folders. If a folder is labeled to a person it might stay the same but the contents inside get organized so people's stuff doesn't move around. Hard to get right."

**Design.**

Three-tier folder classification + per-tier strategy:

| Tier | Definition | Strategy |
|------|-----------|----------|
| **Anchor** | Folder has a meaningful, identity-carrying name (matches a known person, place, event, or known year) AND content is reasonably homogeneous (≥ 60 % share dominant signal). | **Keep the folder, organize internals.** Sort files by date or sub-event. No moves out of the anchor. |
| **Mixed** | Meaningful name but content is heterogeneous (a "Hawaii 2019" folder with 1000 trip photos and 5 random screenshots). | **Keep the folder, dissolve outliers.** Outlier files get re-bucketed; the trip photos stay. |
| **Junk** | Generic name (`untitled folder`, `Pictures (1) (copy)`, `Camera Roll`, `DCIM`) OR content is fully heterogeneous. | **Dissolve.** Files re-bucket via the existing heuristic. |

Folder name classification rules (priority order):
1. Matches a named person in `persons` table (case-insensitive, fuzzy) → **person anchor**
2. Matches a place name pattern (city, country) via reverse-geocode of contained files' GPS → **place anchor**
3. Matches a year/month pattern (`2019`, `2019-05`, `May 2019`) → **time anchor**
4. Matches a generic-junk denylist (`untitled`, `new folder`, `dcim`, `imports`, `temp`, `(1)`, `(copy)`, etc.) → **junk**
5. Otherwise → looked up against folder content homogeneity:
   - ≥ 80 % files share dominant person → suggest rename to that person
   - ≥ 80 % files share GPS bucket → suggest rename to that place
   - ≥ 80 % files share year → time anchor
   - else → **junk**

Diff-preview UX:
- Group changes by impact: **Stays Put** count, **Reorganized within current folder** count, **Moved to new location** count.
- Default-collapse the "Stays Put" group so the user only sees what's actually changing.
- Per-source-folder accordion: "Hawaii 2019 (847 stays, 5 outliers move)".
- Color-code: green = stays, gold = reorganized internally, blue = moves.
- Apply via symlinks first (default, reversible). "Convert to real moves" once user trusts.

Implementation files:
- `engine/Sources/FileIDEngine/Restructure.swift` (or app-side equivalent) — folder classifier
- New `RestructurePlan` DTO with per-folder tier + strategy
- `app/Sources/FileID/Views/RestructureView.swift` — diff-preview redesign
- New unit tests on the classifier (corpus of folder names → expected tier)

**Definition of done.**
- On a corpus where 30 % of folders are well-organized: those folders are flagged Anchor, content stays inside.
- 70 % junk-named folders dissolve correctly.
- Hand-judged on the user's real library: "yes, this is what an assistant would do, not a robot."
- Symlink mode + real-move mode both work end-to-end with DB path updates.
- Undo: a "revert this restructure" button that walks backwards. (Symlink mode is trivially reversible. Real-move mode needs a journal.)

---

### T4 — Apply Tags (macOS Finder tags) **[depends on T1]**

**Approach.**
1. Add `app/Sources/FileID/Services/TagWriter.swift` — given `(URL, [String])`, set Finder tags via `URLResourceKey.tagNamesKey` + reverse on undo.
2. Bulk-apply UX:
   - From Library: select N tiles → "Apply tags" → preview which tags would land → confirm.
   - From People: per-person "tag every photo of this person with their name" toggle.
   - From Cleanup: tag duplicates with a `duplicate` color tag for follow-up.
3. Show Finder tags as colored dots on tile UX.
4. Undo: track applied tags in a `tag_writes` table so we can revert.
5. Document: README section explaining "FileID writes Finder tags. They're reversible. They show up everywhere macOS shows tags (Spotlight, Smart Folders, sidebar)."

**Definition of done.**
- Apply tags to 100 files in one operation in < 5 s.
- Tags appear in Finder's sidebar.
- Spotlight: `tag:Mom` finds the right files.
- Undo restores prior state.

---

### T5 — Apply Names (per-photo VLM rename) **[depends on T1 + T4]**

**Approach.**
1. Verify the existing per-photo rename flow (`ReadStore.applyProposedName`) works end-to-end. If broken, fix.
2. Bulk-rename UX:
   - From Library after Deep Analyze: "Rename N photos to their suggested names" → preview list with old → new → confirm.
   - Conflict handling: if `mom_playing_piano.heic` exists, append `_2`, `_3`, etc.
   - Hard rename only when user confirms; preview is non-destructive.
3. Undo: a `renames` table mapping old → new path so we can revert.
4. Restructure integration: rename happens BEFORE move, so the new folder gets the new filename.

**Definition of done.**
- 100-photo bulk rename in < 10 s.
- Original paths can be restored.
- Filename collisions handled gracefully.

---

### T6 — Polish pass (the unsexy track that actually makes it 5-star)

**Approach.**
A pre-defined audit, top-to-bottom:

1. **Empty states**: every tab has a designed empty state with helpful guidance.
2. **Loading states**: every async operation has a state (skeleton, progress, or "working on X" text).
3. **Error states**: every error is surfaced via `engine.lastError` and dismissible. No `try?` swallows on user-facing paths.
4. **Accessibility**:
   - VoiceOver labels on every interactive element.
   - Keyboard navigation: Tab order, Cmd+number for tab switching, Esc dismisses sheets.
   - Color contrast: ≥ 4.5:1 for body text. Run `scripts/a11y_check.sh` (we'll write this).
5. **Animation**: tab transitions, sheet presentations, list inserts/deletes — all smooth.
6. **Copy audit**: no jargon, no internal IDs in user-facing copy, no debug paths.
7. **Onboarding**: splash already exists; verify it covers the user-relevant steps.
8. **Settings**: organize into clear sections; remove anything dev-only.
9. **Preferences**: remember window size, sidebar visibility, last folder, last sort.
10. **Sleep / wake**: the engine cleanly suspends + resumes when the lid closes / opens off AC.

**Definition of done.**
- A friend can use the app for 30 minutes without a single confused moment.
- Every screen looks intentional, not unfinished.

---

### T7 — Sign, notarize, package

**Approach.**
1. Apple Developer account (user has? if not, $99/year).
2. Code signing in `run.sh` and a separate `scripts/release.sh`.
3. Notarization via `notarytool`.
4. Stapling.
5. DMG packaging via `create-dmg` or hdiutil.
6. Hardened runtime entitlements file with only what we actually need (file access, network for HF).
7. Sparkle-style auto-update: defer to v1.1; for 1.0, manual download from GitHub Releases is fine.

**Definition of done.**
- `bash scripts/release.sh v1.0.0` produces a signed, notarized, stapled DMG.
- DMG installs cleanly on a fresh Mac with no Gatekeeper warnings.

---

### T8 — Open-source release artifacts

**Approach.**
1. **LICENSE**: MIT (broad permissive — most welcoming to contribution + redistribution). Add the standard MIT header to every Swift file.
2. **README.md**: already strong; add screenshots of every tab + a 60-second demo video link.
3. **CONTRIBUTING.md**: how to build, test, submit a PR.
4. **CODE_OF_CONDUCT.md**: standard Contributor Covenant.
5. **PRIVACY.md**: explicit statement of what stays local + what touches the network (HF model downloads).
6. **`.github/`**:
   - Issue templates (bug, feature)
   - PR template
   - GitHub Actions: build + test on every PR
   - Release workflow: triggered on tag, builds + signs + notarizes + uploads DMG
7. **Repo metadata**: description, topics, social-card image.
8. Publish.

**Definition of done.**
- Public GitHub repo with all of the above.
- A first GitHub Release (`v1.0.0`) with a downloadable DMG.
- README opens with a hero screenshot + 60-second demo gif/video.

---

## Out of scope (v1.1+)

Explicitly cut to keep us shipping:

- App Store distribution (direct DMG only)
- Multiple-user / shared library
- Cloud backup or sync
- Plugin system
- Cross-platform (Windows, Linux)
- Custom face embedder training
- More VLM choices beyond what's there
- Restructure undo journal for real-moves (v1 has symlink-mode reversible; real-move undo is v1.1)
- Sparkle auto-update (manual download from Releases for v1.0)
- Localization beyond English

---

## Testing & iteration framework

We earn the right to call something "done" by passing automated tests, not by clicking around.

### Scripts (all in `scripts/`)
- `iterate.sh` — fast end-to-end: wipe DB, scan corpus, run all clustering / restructure / cleanup paths, assert outputs match expected ranges. < 5 min.
- `soak.sh` — runs `iterate.sh` 10× back-to-back, asserts memory stable, no crashes. < 1 hour.
- `diagnose_arcface.py` — checks ArcFace embeddings are distinct + the model isn't degenerate.
- `convert_arcface.py` — already exists; add a multi-input diversity check after conversion.
- `a11y_check.sh` — uses macOS `accessibilityInspector` CLI to audit VoiceOver coverage.
- `release.sh` — sign + notarize + DMG.

### Cadence
- Before any track is marked done in this doc, `iterate.sh` must pass.
- Before any release tag, `soak.sh` must pass.
- Both wired into a pre-push git hook (optional but encouraged).

---

## Operating principles for the rest of this work

1. **No new tracks.** If we discover a problem, we fix it inside the existing track or push to v1.1. We do not open a 9th track.
2. **No silent reverts.** When something breaks, we add a test that catches it next time.
3. **The honest answer beats the fast answer.** If a fix is "really we should redesign X" — say so, and then we choose: scope into v1 vs. punt to v1.1.
4. **Ship-ready means ship-ready.** No "we'll polish later." If a feature isn't to bar, it gets cut, not landed half-done.
5. **The user is the final QA.** Every track ends with a hands-on session — does this *feel* like a team built it?

---

## Cap

8 tracks. Each has a definition-of-done. We do them in order. When the last one ships, we cut v1.0.0.

If something genuinely critical comes up that doesn't fit, it goes into a "v1.0.1 hotfix" doc — not into this plan.

---

## Appendix W — Windows v1.0 per-vendor verification matrix

The Windows port (`platforms/windows/`) is its own ship lane, parallel to the macOS v1.0 plan. The engine's ORT execution-provider picker auto-detects the best accelerator on every supported vendor's silicon. **Performance Packs were removed in V14.8.2** (see `PACKS.md`) — DirectML is now the universal GPU path for every D3D12-capable vendor, with CPU as the floor. Throughput targets below reflect that reality.

For each row below, run a 1,000-file scan on a representative library and confirm the engine log + throughput target.

| Vendor | Reference hardware       | Expected EP | Throughput target | Memory ceiling | Status |
|--------|--------------------------|-------------|-------------------|----------------|--------|
| NVIDIA | RTX 3060 / 4060+         | DirectML    | ≥ 60 files/s      | ≤ 4 GB VRAM    | ⬜ pending |
| AMD    | RX 6600 / 7600+          | DirectML    | ≥ 40 files/s      | ≤ 4 GB VRAM    | ⬜ pending |
| Intel  | Arc A380 / Iris Xe       | DirectML    | ≥ 30 files/s      | ≤ 3 GB shared  | ⬜ pending |
| Intel  | UHD 770 iGPU             | DirectML    | ≥ 18 files/s      | ≤ 3 GB shared  | ⬜ pending |
| Qualcomm | Snapdragon X Elite     | CPU         | ≥ 25 files/s      | ≤ 2 GB         | ⬜ pending |
| CPU    | i7-12700 / Ryzen 7 7700  | CPU         | ≥ 25 files/s      | ≤ 2 GB RSS     | ⬜ pending |

NVIDIA throughput sits at ~80–90% of native CUDA via DirectML (per `DECISIONS.md` 2026-05-02). Snapdragon falls to CPU because no public QNN SDK redistributable exists — power users who install the Qualcomm SDK locally will see the engine auto-pick QNN, but the default ship target is CPU.

### Per-vendor acceptance criteria

Each row passes when **all six** hold:

1. **Engine log shows the expected EP.** Open `%LOCALAPPDATA%\FileID\logs\app.log` after a fresh scan and grep for `[EP] built session`. The `ep=` field must match the table.
2. **Throughput target met** over a representative 1,000-file image library. Wall-clock from "Scan started" to "Scan completed" / 1000 ≥ target.
3. **Memory ceiling honored.** Peak RSS (per Task Manager) ≤ table value across the scan.
4. **No crash dumps** generated in `%LOCALAPPDATA%\CrashDumps\` during the run.
5. **Deep Analyze succeeds on 10 sample images.** llama.cpp Vulkan runtime covers NVIDIA / AMD / Intel; CPU fallback on Snapdragon. Surfaced in the engine log via `[VLM]` lines.
6. **iterate.ps1 corpus regression green.** When the harness lands per `NEXT.md`, all 11 assertions pass on the host.

### What "100% certainty" means

Code-level certainty is in place: `engine/src/models/runtime.rs` unit tests cover every vendor's EP pick + fallback, and the EP picker fails safely down the chain if any EP can't build a session. **Hardware certainty** is the missing layer — only running on each vendor's silicon proves drivers, DLLs, and ORT integration all line up. The six checkboxes above ARE the proof. Per-vendor pack installs are no longer part of the loop (packs are removed; see `PACKS.md`).

### Build pre-reqs for the verification pass

- EV cert installed + `FILEID_EV_THUMBPRINT` set (so signed binaries don't get SmartScreen-blocked on first run).
- `llama_runtime_x64` (Vulkan llama.cpp) downloadable from GitHub — verified live.

### Lane gate

Windows v1.0 ships **only** when ≥ 4 of the 6 rows are green (CPU, plus at least one each from NVIDIA / AMD / Intel; Snapdragon may launch in a follow-on if hardware availability blocks). All 6 rows is the goal; 4 is the minimum.
