# FileID TUI (`fileid-tui`)

A cross-platform **terminal UI** for FileID — browse your indexed library,
inspect file detail, view person clusters, duplicate groups, Deep Analyze status,
and a read-only restructure plan, all from the terminal. No cloud, no telemetry. Built with
[ratatui](https://ratatui.rs) + [crossterm](https://crates.io/crates/crossterm)
(pure-Rust, no system libraries).

It is a sibling of [`platforms/cli`](../cli): its own standalone Cargo
workspace that links the shared Rust engine (`fileid-engine`) as a library and
**reuses the exact same read surface** — `fileid_engine::db::open_read`,
`paths`, `pipeline::restructure::classify`, `pipeline::discovery::FileKind` —
so the DB/IPC contract can never drift across the CLI, the TUI, and the desktop
apps.

## Build & run

Quickest path — from the repo root, `bash scripts/build-tools.sh` builds the
engine, CLI, and TUI in release and installs `fileid`, `fileid-tui`, and the
engine binary to `~/.cargo/bin` (make sure that's on your `PATH`). To build
just this crate:

On Windows, use `.\scripts\build-tools.ps1`; it installs the three executables
plus the ONNX Runtime/DirectML DLLs required by engine-driven scans.

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

Populate a library out-of-band with the CLI's model-free scan — `fileid --db
<db> scan <folder>` (filenames + OCR/doc text → FTS; works on every platform) —
then point the TUI at the same `--db`. You can also press `s` to scan from
inside the TUI (see Keys); that drives the engine's **full ML pipeline** on all
three platforms once the models are installed — press `D` on the Settings tab
(or run `fileid models download --all`) to fetch them, macOS included.

## Keys

| Key | Action |
| --- | --- |
| `Tab` / `Shift-Tab` | next / previous tab |
| `1`–`5` | jump to a tab |
| `↑`/`↓` or `k`/`j` | move selection |
| `g` / `G` | first / last row |
| `/` | search (Library tab) — type to filter, `Enter` keeps, `Esc` clears |
| `s` | **scan a folder** — opens a path prompt; `~` expands; `Enter`/`Tab` confirm, `Esc` cancel. Drives the engine's full-ML scan (Linux/Windows; on **macOS** use the desktop app for full ML) and live-streams progress to the status line, then auto-reloads. |
| `r` | reload from the DB (re-reads every view) |
| `?` | toggle the keys overlay |
| `q` / `Esc` / `Ctrl-C` | quit (works mid-scan; the terminal is always restored) |

The whole UI paints its own brand-dark background, so it stays legible on
light-background terminals too — it never relies on the terminal's default
colors.

## Screens

Mirrors the desktop app's six-tab layout at terminal fidelity, with the
signature gold/lavender/cyan/pink accent palette.

| Tab | Status | Source |
| --- | --- | --- |
| **Library** | ✅ working | live `files` rows, master/detail split, searchable; detail shows kind/size/date/flags + tags + a text snippet |
| **People** | ✅ working | live `persons` clusters (id, display name, face count, file count) |
| **Cleanup** | ✅ working | 5,000-candidate/64-GiB read-only full-file SHA-256 preview across legacy/current stored hash recipes, master/detail (group → member paths) |
| **Restructure** | ✅ working (read-only) | in-process `restructure::classify` preview — proposed source → destination, category, confidence tier |
| **Settings** | ✅ working | resolved DB path, row/tag/people/dup counts, engine wiring, stubbed-feature notes |
| **Deep Analyze** | ⚠️ companion-only | explains the desktop VLM review workflow; model install remains available from the TUI |

The status line at the bottom is driven by a **live event stream**: a
background thread streams progress messages over an `mpsc` channel into the
render loop. The DB loader feeds it (`Opening…`, `Reading files…`, `Loaded N
files · …`), and during a scan the **engine's own IPC events** feed the same
channel (`Scanning [Tagging] 1234/5678 (142 files/s)`, `Scan complete…`).

## Scanning (live, in-TUI)

Press `s`, type a folder, confirm — the TUI spawns the `FileIDEngine` binary and
speaks newline-delimited JSON over stdio, reusing the engine's own
`ipc::IpcCommand` / `IpcEvent` types (no contract drift), exactly as the CLI's
`scan --models` and the desktop apps do. It sends `startScan` and live-streams
`progress` / `phaseChanged` / `scanComplete` events to the status line, then
auto-reloads every view. See [`src/scan.rs`](src/scan.rs).

`startScan` runs the **full ML pipeline**, so it requires the AI models
(`mobileclip_s2` + `arcface`) and the engine binary:

> **macOS:** this in-TUI `s` scan runs full ML once you install the engine's
> **own** models — press **`D`** on the Settings tab (or run `fileid models
> download --all`). The Rust engine needs its own model layout, which the macOS
> app's Swift CoreML models don't satisfy, so the TUI installs + reads its
> weights under `~/.local/share/FileID/Models` (separate from the app's read-only
> CoreML dir). Until then it reports "models not installed" — never a crash. You
> can also scan with full ML in the **FileID desktop app** and reload here with
> `r`. Full-ML `s` scanning works the same on **Linux/Windows**.

- **Models** — installed once with `D` on the Settings tab (or `fileid models
  download --all`), on every platform incl. macOS; the desktop app's
  Settings → Local AI also installs them. If they're missing, the status line
  says exactly which, and how.
- **Engine binary** — located via `FILEID_ENGINE_BIN`, then beside `fileid-tui`,
  then the dev-layout `platforms/windows/src/engine/target/{release,debug}/`,
  then `PATH`. If absent, the status line says how to build/point at it.

`startScan` drives the engine's own library location (`$XDG_DATA_HOME` /
`%LOCALAPPDATA%`); when you pinned a different `--db`, the TUI notes the mismatch
(the reload reads your `--db`). The engine's stderr is discarded so its logs
can't scribble over the terminal UI; `q` quits cleanly even mid-scan.

## What is stubbed (follow-on)

The read/query surface and folder scanning are live; the remaining mutating /
model-dependent operations are intentionally deferred and labelled in Settings.

- **Face clustering.** A scan detects + embeds faces, but grouping them into
  people is a separate `runFaceClustering` engine command — not yet triggerable
  in-TUI.
- **Restructure apply.** The plan is a **read-only preview**; nothing is moved.
- **Semantic search, people merge/rename, Deep Analyze apply/review.** App-side / model
  features not in the terminal MVP; the Deep Analyze tab is currently a companion handoff.

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
