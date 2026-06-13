# Audit 2026-06-10 — campaign working notes

## Baselines (branch cut from main @ 83ab77f, all 3 CI workflows green)
- Rust: clippy `-D warnings` clean; `cargo test` = 292 lib + 2 integration passed, 2 ignored (windows-only).
- Swift: `swift build` debug + release clean (no Xcode locally — swift test is CI-only).

## User rulings
1. D-7 collision policy: auto-rename `name (2).ext` on BOTH platforms (Windows behavior is spec).
2. Full pipeline: batch pushes → draft PR → fix-forward → merge to main on green.
3. Scope: fix everything, all severities, including roadmap items F-1..F-4.

## Fix-design prep (verified against code, pre-findings)

### F-1 — macOS restructure-apply collision parity (Restructure.swift ~241-325)
Port from restructure_apply.rs:
- `unique_destination` semantics: occupied = in-batch claimed set ∪ on-disk lstat
  (use `FileManager.attributesOfItem` — non-traversing, dangling symlink counts
  occupied, matching Windows `symlink_metadata`); suffix `stem (n).ext`, n=2...9999,
  fallback original (moveItem then fails safely — never overwrites).
- Add B4 stale-plan guard: re-read live `files.path_text` for fileID, require == oldPath, else failed++.
- Add in-batch `claimed` set (B3 in-batch half — currently absent on macOS).
- DB update must also refresh `path_hash` (ENG-91 parity — macOS currently updates only
  path_text + path_search; path_hash column exists, notNull+indexed → stale after apply. REAL BUG.)
- Add ENG-42 no-op skip-before-uniquify parity (source already at planned dest → applied, no rename churn).
- Wire result: schema RestructureApplyResult = {applied, failed, privilegeError?}. Map moved→applied;
  conflicts list disappears (auto-renamed now).

### F-2 — rename-heal exact-duplicate: WINDOWS ALREADY FIXED, BUGS.md STALE
- `heal_candidate_moved` (dbwriter.rs:724) gates every heal (file_ref AND content_hash)
  on old-path-gone via symlink_metadata; landed in a05a59a with B1 regression tests (line 1024+).
- Remaining scope: macOS has NO heal at all — content_hash/file_ref not computed by the macOS
  scan path (DBWriter.swift:398 comment). Renamed files lose tags/faces on rescan (new row).
  Plan: implement macOS rename-heal with file_ref = APFS inode (st_ino, no new dependency),
  same old-path-gone gate (lstat). content_hash (BLAKE3) deferred — Swift BLAKE3 would be a
  new package → DECISIONS.md question; file_ref alone covers the rename/move-on-same-volume case.
- Update BUGS.md: mark Windows half fixed; rescope to macOS mirror.

### F-3 — macOS memory adaptation: design arriving from WF-3 `mac-memory-adaptation` check.
### F-4 — face clustering mutual-kNN: pass-1 single-linkage lives in identity_clustering.rs (Windows source of truth) + FaceClustering.swift mirror; thresholds stay provisional, recipe → NEXT.md.

## Workflows
- WF-1 unit-audit: run wf_0df32b02-327 (70 unit assignments)
- WF-2 parity-audit: run wf_1a3aeb73-157 (9 pairs)
- WF-3 perf-adaptive: run wf_ad95f9d1-e42 (8 checks)
