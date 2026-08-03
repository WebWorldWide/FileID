# FileID TUI (`fileid-tui`)

FileID's cross-platform terminal interface: browse a library, search extracted
content, inspect tags, review people and Deep Analyze results, verify duplicate
sets, and preview a restructure plan. Everything stays on-device. The only
network action is a model download the user explicitly starts.

The TUI links the shared Rust engine and uses its schema, migrations, file-kind
classification, restructure rules, and IPC types. It runs natively on macOS,
Linux, and Windows with ratatui and crossterm—no web view or telemetry.

## Install and run

From the repository root, the tool installer builds the engine, CLI, and TUI in
release mode and puts all three executables in `~/.cargo/bin`:

```bash
bash scripts/build-tools.sh
fileid-tui
```

On Windows:

```powershell
.\scripts\build-tools.ps1
fileid-tui.exe
```

To build only the TUI:

```bash
cd platforms/tui
cargo build --release
./target/release/fileid-tui
./target/release/fileid-tui --db /path/to/fileid.sqlite
```

Rust 1.90 is pinned in `rust-toolchain.toml`.

## Choose a library

A bare `fileid-tui` opens a persistent, isolated scratch library at the
platform data directory's `FileID-TUI-Scratch/FileID/fileid.sqlite`. Scanning
inside the TUI writes to that same scratch DB; it never silently changes a
desktop-app library.

Resolution precedence is:

1. `--db <PATH>`
2. `$FILEID_DB`
3. `$CFFIXED_USER_HOME/fileid.sqlite`
4. The isolated TUI scratch library

Open an existing desktop or CLI library explicitly:

```bash
fileid-tui --db "$HOME/Library/Application Support/FileID/fileid.sqlite"
FILEID_DB=/srv/fileid/fileid.sqlite fileid-tui
```

Use `fileid-tui --help` to see the resolved behavior without entering the
alternate screen.

## Keys

| Key | Action |
| --- | --- |
| `Tab` / `Shift-Tab` | next / previous tab |
| `1`–`6` | jump directly to a tab |
| `↑`/`↓` or `k`/`j` | move one row |
| `PgUp` / `PgDn` | move ten rows |
| `Home`/`End` or `g`/`G` | first / last row |
| `/` | search Library names and extracted content; `Enter` commits, `Esc` clears |
| `s` | open the folder browser |
| `D` | download the curated scan/search model set from Hugging Face |
| `r` | reload the current DB |
| `?` | open or close the complete key guide |
| `q` / `Esc` | quit; inside an overlay, close the overlay first |
| `Ctrl-C` | quit from anywhere |

The folder browser uses arrows or `j`/`k`, `Enter` to descend, `Backspace` or
`h` to go up, `d` for drive roots, `.` to show hidden entries, `t` to type a
path, and `s` to scan the folder currently shown.

The layout adapts from full tab names to compact names and then numbered tabs,
so all six destinations remain reachable at standard and narrow terminal
widths. Color is never the only status signal; labels and symbols accompany the
FileID gold/lavender/cyan/pink palette.

## What each tab shows

| Tab | Behavior |
| --- | --- |
| **Library** | bounded, searchable file list; metadata, tags, and extracted-text detail |
| **People** | current face clusters with names, file counts, and face counts |
| **Cleanup** | bounded, live SHA-256 verification of same-size duplicate candidates; read-only |
| **Deep Analyze** | persisted captions, suggested filenames, model, and analysis time; read-only |
| **Restructure** | engine-classified source-to-destination plan with confidence; read-only |
| **Settings** | selected DB, counts, model state, privacy guarantees, and available actions |

Large libraries remain bounded: file and analysis lists cap their in-memory
snapshot, restructure marks partial previews, and duplicate verification runs
after the first frame with candidate and byte budgets. The UI labels every
partial result instead of presenting it as complete.

## Full-AI scans

Press `s`, choose a folder, then press `s` again. The TUI starts
`FileIDEngine`, sends canonical newline-delimited IPC, and renders discovery,
phase, rate, failure, and completion messages without blocking input. After a
scan completes it runs the engine's face-clustering command before reloading,
so People is immediately current.

The engine requires `mobileclip_s2` and `arcface`. Press `D` on any tab to
install the curated non-VLM scan/search set, or install only the required pair:

```bash
fileid models download mobileclip_s2 arcface --dry-run
fileid models download mobileclip_s2 arcface
```

On macOS these are the Rust engine's ONNX weights under its writable model
directory; the native app's Core ML weights are separate. A Rust-engine scan on
macOS also needs the one-time ONNX Runtime setup:

```bash
fileid runtime status
fileid runtime install
```

The engine executable is resolved from `$FILEID_ENGINE_BIN`, beside
`fileid-tui`, the repository's engine build directories, and then `PATH`. An
explicit `--db` is forwarded to the engine so scanning and rendering cannot
diverge.

Engine stderr is captured off-screen in a bounded buffer. If the engine exits
unexpectedly, its useful tail becomes a status error without corrupting the
terminal. Quitting closes the IPC pipe, allowing the engine's parent watchdog
to cancel an in-flight scan. The terminal guard always restores cooked mode,
the main screen, and the cursor on normal exit and panic.

## Deliberately read-only workflows

The TUI does not delete duplicates, move files, merge people, or apply a Deep
Analyze filename. Use the CLI's explicitly gated `dedupe --apply` and
`restructure --apply` commands, or the desktop review flows. Keeping mutations
out of list-navigation keys prevents an accidental terminal keystroke from
changing real data.

## Verify

```bash
cd platforms/tui
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests cover the state machine, DB loading and caps, scan/face-clustering IPC
sequence, cancellation, model progress, small-terminal safety, responsive
headers, modal behavior, and full-frame TestBackend rendering.
