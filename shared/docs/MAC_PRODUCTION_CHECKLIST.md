# Mac production checklist — Rust-only app, Adlon read-only

Every tick names its evidence. "Not exercised" is never a tick. Owner-only items are marked **[owner]**.
Evidence lived on the dev Mac, outside the repo: wave and round reports in `~/FileIDDev/briefs/rustfinal/` (wave2 to wave8, xplat, xplat2, cleanup, round5, round6) and acceptance captures, logs and catalogs in `~/FileIDDev/scratch/rf-accept4` to `rf-accept7`. The session summary is in `STATE.md` (2026-11-24 to 2026-11-26); open work is in `NEXT.md`.

Legend: [x] done with evidence · [ ] open · [~] partly (see note)

## 1. One engine
- [x] Swift engine sources, tests and the ONNX Runtime Swift package deleted (`fa8460e6`); tract replaces ONNX Runtime everywhere.
- [x] No Swift engine binary in any bundle; only `fileidd` opens model files and runs inference (round 4 and round 5 lane F: lsof/vmmap during scan, search, faces, chat, describe; `nm` of the FileID binary shows no inference symbols).
- [x] CI builds and tests no Swift engine; conformance runs plain Rust (109) and app-hosted Rust (137) only.
- [x] Docs (CLAUDE.md files, CONVERGENCE, SWIFT_ENGINE_RETIREMENT, STATE, NEXT, SHIP) describe one engine (final docs pass, 2026-11-26).

## 2. Adlon read-only safety
- [x] Adlon mounted read-only at the OS level (2026-11-24; `touch` fails with EROFS).
- [x] Metadata manifest of every entry, diffed after every wave and round: 0 added, 0 removed, 0 changed (`~/FileIDDev/scratch/rf-adlon/manifest-0` to `manifest-6`, latest after round 5).
- [x] Live catalog `~/Library/Application Support/FileID` untouched (unchanged since 2026-11-19; checked after round 5).
- [x] Engine detects any read-only volume by its flag (statfs `MNT_RDONLY`, statvfs `ST_RDONLY`, `GetVolumeInformationW` `FILE_READ_ONLY_VOLUME`) as well as the Adlon name, and refuses every mutating command up front with `location_read_only` and a plain sentence; a mixed batch is refused whole with its count; an Organize plan touching a read-only drive is refused whole (wave 5 readonly-proof on a real read-only exFAT image: every mutating command, 0-change manifest).
- [x] Mac app: READ-ONLY badge on the location, mutating controls disabled with the reason, Chat cards refuse in plain words (rounds 4–5 captures).
- [~] WinUI shows the READ-ONLY badge and plain refusals; GTK's badge fix is in wave 7 (R5-03). Neither pre-disables write buttons like the Mac (the engine refuses instead) — follow-up.
- [x] Mutating features work end to end on writable copies: Trash/put back (39/39 byte-identical), rename/Undo, Organize Move/Undo, tags, exports (round 4 lane D, round 5 lane D).

## 3. Whole-drive scan (Adlon, release bundle)
- [x] First scan completes: 130,108 catalogued; the rest are documented skips (`._*`, dotfiles, recycle bins, volume metadata) — round 4 A, round 5 A, wave 5 perf.
- [x] Peak RSS: fileidd 4.5–5.4 GB, FileID 1.05 GB; catalog 851 MB.
- [~] Speed: main pass ~59–91 files/s (Vision bound ~150 files/s alone on this M1 Pro); post-scan RAM++ ~7.4–8 files/s GPU-bound, ~2.7 h on the whole drive. Below the 140 files/s target; wave 7 measures quality-safe speedups and fixes the jumping total and the rescan redo (R5-01, R5-17).
- [~] Unchanged rescan: 3.6–6.5 s, but the first rescan after a full scan redid post-scan work (195 s) — wave 7 (R5-01).
- [x] Relaunch: 0.67–1.0 s to the file counter, rows 0.3 s later; first launch of a new bundle 4 s.
- [x] Offline drive: library stays, location shows offline, nothing rebinds (round 4).
- [x] Face grouping resumes after a quit (wave 6b); grouping 78k faces takes ~62 s, peak 2.6 GB.

## 4. Accuracy
- [x] Chat: 138 questions answered with counts matching SQL (round 5 C); folder-scope and follow-up defects in wave 7 (R5-08…R5-13).
- [~] Capture dates: photos correct on the round-5 sample; videos read the QuickTime creation date (wave 5); epoch mvhd, wmv/avi/mkv dates, FAT 1980 dates in wave 7 (R5-18, R5-42, R5-27).
- [x] Duplicates: 1,608 exact groups / 45.1 GB on the whole drive, totals match SQL, every group reachable, background byte check, RAW+JPEG and edited copies never grouped (waves 5–6, round 5 D).
- [~] Faces: 6 pure + 7 mostly pure of 20 random people; 4 mega-persons of look-alike family members — wave 7 k-way split; a stronger embedder is an owner decision (licence vetting).
- [x] Search: keyword p50 50–56 ms, p95 137–354 ms at 130k rows; visual search p95 ~430–550 ms.

## 5. Every page (1000×700 and default size, Larger text; dark-only by design)
- [x] Library at 130k rows: End/⌘↓ reach the last row on every sort, grid and list (wave 6, round 5 A).
- [x] People, Cleanup, Organize, Search & Moments, Chat, File Tools, Best Takes, Settings, Welcome captured at 1000×700 and Larger text (waves 5–6, round 5 E); remaining overflow items R5-19, R5-38, R5-39 in wave 7.
- [ ] **[owner]** Click-through of sheets, hover, drag and drop, menus and shortcuts: agents cannot click without Accessibility and Screen Recording permission for the terminal.

## 6. Gates
- [x] Engine fmt, clippy (plain, test-trash-root, apple-adapters), cargo test (1,921 plain, 1,952 apple-adapters).
- [x] Conformance: plain 109/0, app-hosted 137/0.
- [x] Swift: FileID + FileIDAppleKit debug and release builds; 690 tests.
- [x] GTK 342, CLI 63, TUI 100, WinUI contract 218, C# IpcSchema 71.
- [x] Every policy script, including runtime egress with audited digest refreshes.
- [x] Hosted CI green on all 8 workflows (`e19a3b92`, 2026-11-26) — re-confirm on the final head.

## 7. Windows and Linux
- [x] Line-by-line drill of GTK, WinUI, CLI, TUI, the engine's Windows/Linux code and packaging: 80 + 5 confirmed defects fixed, each verified by a skeptic and reviewed (`~/FileIDDev/briefs/rustfinal/xplat` and `xplat2`).
- [x] Hosted CI builds and tests them on Windows x64/arm64 and Linux x64/arm64.
- [ ] **[owner]** Click-through on a real Windows PC and a Linux desktop.

## 8. Code quality
- [x] Two cleanup passes: ~3,350 lines of narrating/history comments, stale Swift-engine references and dead code removed across ~280 files, behaviour unchanged (every gate; comment-only digest audit).

## 9. Release **[owner]**
- [ ] `xcrun notarytool store-credentials fileid-notary` (missing)
- [ ] `bash scripts/release.sh v<next>-rc1`, stapler validate, spctl
- [ ] App Store Connect API key + `FILEID_MAC_APP_STORE_PROFILE`, then `scripts/testflight.sh`
- [ ] Merge PR #345 to `main` and confirm main CI green
