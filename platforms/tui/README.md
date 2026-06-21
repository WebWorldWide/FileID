# FileID TUI (`fileid-tui`)

A cross-platform **terminal UI** for FileID — browse your indexed library,
inspect file detail, view person clusters, duplicate groups, and a read-only
restructure plan, all from the terminal. No cloud, no telemetry. Built with
[ratatui](https://ratatui.rs) + [crossterm](https://crates.io/crates/crossterm)
(pure-Rust, no system libraries).

It is a sibling of [`platforms/cli`](../cli): its own standalone Cargo
workspace that links the shared Rust engine (`fileid-engine`) as a library and
**reuses the exact same read surface** — `fileid_engine::db::open_read`,
`paths`, `pipeline::restructure::classify`, `pipeline::discovery::FileKind` —
so the DB/IPC contract can never drift across the CLI, the TUI, and the desktop
apps.

## Build & run

```sh
cd platforms/tui
cargo build --release
./target/release/fileid-tui                 # default library (engine canonical path)
./target/release/fileid-tui --db /path/to/fileid.sqlite
```

### Library location

Resolved with the **same precedence as the CLI**:

1. `--db <PATH>`
2. `$FILEID_DB`
3. `$CFFIXED_USER_HOME/fileid.sqlite` (parity with the macOS app sandbox root)
4. `fileid_engine::paths::db_path()` — the engine's canonical location
   (`$XDG_DATA_HOME` / `%LOCALAPPDATA%`), i.e. the same file the desktop apps
   read/write.

Populate a library first with the CLI: `fileid --db <db> scan <folder>`, then
point the TUI at the same `--db`.

## Keys

| Key | Action |
| --- | --- |
| `Tab` / `Shift-Tab` | next / previous tab |
| `1`–`5` | jump to a tab |
| `↑`/`↓` or `k`/`j` | move selection |
| `g` / `G` | first / last row |
| `/` | search (Library tab) — type to filter, `Enter` keeps, `Esc` clears |
| `r` | reload from the DB |
| `?` | toggle the keys overlay |
| `q` / `Esc` / `Ctrl-C` | quit |

## Screens

Mirrors the desktop app's six-tab layout at terminal fidelity, with the
signature gold/lavender/cyan/pink accent palette.

| Tab | Status | Source |
| --- | --- | --- |
| **Library** | ✅ working | live `files` rows, master/detail split, searchable; detail shows kind/size/date/flags + tags + a text snippet |
| **People** | ✅ working | live `persons` clusters (id, display name, face count, file count) |
| **Cleanup** | ✅ working | exact-duplicate groups by BLAKE3 `content_hash`, master/detail (group → member paths) |
| **Restructure** | ✅ working (read-only) | in-process `restructure::classify` preview — proposed source → destination, category, confidence tier |
| **Settings** | ✅ working | resolved DB path, row/tag/people/dup counts, engine wiring, stubbed-feature notes |
| _(People/Cleanup/Restructure are also a single tab strip — no 6th screen; "Deep Analyze" is folded into Settings notes for the MVP)_ | | |

The status line at the bottom is driven by a **live event stream**: a
background loader thread streams progress messages (`Opening…`, `Reading
files…`, `Computing restructure plan…`, `Loaded N files · …`) over an `mpsc`
channel into the render loop — the same architecture an engine-spawn-IPC event
feed slots into.

## What is stubbed (follow-on)

This is a **compiling MVP**: the read/query surface is fully live; mutating and
model-dependent operations are intentionally deferred and clearly labelled in
the Settings tab.

- **Engine-spawn command IPC.** The CLI and TUI currently drive the engine
  **in-process** (read surface + the pure `restructure::classify`). Spawning the
  `FileIDEngine` binary and streaming live `scan` / `cluster` progress events
  over newline-delimited JSON stdio is the next step. The status-line event
  stream is already wired to consume such a feed; today it carries the DB-load
  progress instead.
- **`scan` / `cluster` actions.** No in-app trigger yet — index with the CLI
  (`fileid scan`) and reload (`r`). People/duplicate signals (faces, perceptual
  + content hashes) require a full engine scan with ML models, exactly as for
  the CLI.
- **Restructure apply.** The plan is a **read-only preview**; nothing is moved.
- **Semantic search, people merge/rename, Deep Analyze.** App-side / model
  features not in the terminal MVP.

## Verify (same gate as CI)

```sh
cd platforms/tui
cargo clippy --all-targets -- -D warnings
cargo build
cargo test          # headless: state machine, layout split, search filter,
                    # date fmt, + a TestBackend full-frame render assertion
```

CI builds + tests this crate on `ubuntu-latest` via the `tui` job in
[`.github/workflows/linux.yml`](../../.github/workflows/linux.yml).
