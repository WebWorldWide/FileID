# FileID CLI (`fileid`)

A cross-platform command-line front-end for FileID — index, search, inspect,
de-duplicate, and plan a restructure of a local file library from the terminal.
No cloud, no telemetry, model-free for the read/query paths.

> **Cross-OS, despite living under `platforms/`.** Unlike its siblings
> (`apple`, `windows`, `linux`), this crate is not OS-specific. It links the
> shared Rust engine as a library and uses only portable `std` + bundled
> SQLite, so it builds and runs identically on **macOS, Linux, and Windows**.
> It is its own standalone Cargo workspace (mirroring `platforms/linux`).

## What it is

The desktop apps (SwiftUI / WinUI 3 / GTK4) and this CLI are all **clients of
the same engine** (`platforms/windows/src/engine`, cross-platform) over the
same SQLite library + IPC contract (`shared/ipc-schema/ipc.schema.json`).

The desktop clients spawn the engine binary and talk newline-delimited JSON
over stdio. The CLI instead **links the engine crate directly and calls its
public library surface in-process** — `db::open_writer`/`open_read` (schema +
migrations), `pipeline::discovery::FileKind` (classification),
`pipeline::restructure::classify` (the restructure rule cascade), and `paths`
(default library location). Two reasons for the in-process choice:

1. **Reads + plan** (search, info, people, dedupe-list, restructure-plan) have
   **no IPC command** — the desktop apps run them as direct read-only SQL
   against the engine's DB. The CLI does the same.
2. **Apply** (`dedupe --apply`, `restructure --apply`) calls the engine's exact
   apply code in-process — the *same* `pipeline::restructure_apply::RestructureApply`
   and `shell::trash::trash` that the `applyRestructure` / `trashFiles` IPC
   handlers invoke — so there's no second implementation to drift.
3. **`scan --models`** is the one path that can't be a library call: the
   engine's `startScan` hard-requires ML models *and* owns its own async + ORT
   runtime. There the CLI **spawns the `FileIDEngine` binary** and speaks the
   engine's own newline-JSON IPC (`ipc::IpcCommand` / `IpcEvent`). The default
   model-free `scan` stays an in-process FTS indexer through the engine schema.

Reusing the engine crate (path dependency) means the CLI **can't drift** from
the engine: same tables, same migrations, same `FileKind`, same restructure +
apply + trash logic, same IPC types.

## Build

Quickest path — from the repo root, `bash scripts/build-tools.sh` builds the
engine, CLI, and TUI in release and installs `fileid`, `fileid-tui`, and the
engine binary to `~/.cargo/bin` (make sure that's on your `PATH`). To build
just this crate:

```bash
cd platforms/cli
cargo build --release          # compiles the shared engine too (first build is slow)
./target/release/fileid --help
```

Toolchain is pinned to Rust **1.90** (`rust-toolchain.toml`), matching the
engine and CI. Self-verify:

```bash
cargo clippy --all-targets -- -D warnings
cargo test                     # model-free, isolated scan→search→info smoke test
```

There is **no separate engine build step** — the engine is a path dependency
(`fileid-engine = { path = "../windows/src/engine", default-features = false }`)
and is compiled and linked into the single `fileid` binary. (`default-features
= false` drops the engine's `pdf-analyze`/pdfium feature; the CLI never
rasterizes PDFs.)

## Commands

| Command | What it does | Models? |
|---|---|---|
| `fileid scan <path> [--rescan]` | Index a directory: one `files` row per file + plain-text content into `doc_text` (FTS). Incremental by default; `--rescan` reprocesses everything. | No |
| `fileid scan <path> --models [--rescan]` | FULL pipeline — image tags, CLIP embeddings, faces, perceptual + content hashes — by spawning the engine binary and streaming its progress. **Linux/Windows** (native engine); on **macOS** scan with full ML in the desktop app instead. Prints an actionable message if models (or the engine binary) aren't installed. | **Yes** |
| `fileid search <query…> [--limit N]` | FTS5 keyword search over document text + image OCR text, plus a filename fallback. | No (FTS) |
| `fileid search --similar <path-or-id> [--limit N]` | Visual / semantic nearest-neighbor: ranks files by cosine similarity to the seed file's CLIP embedding. Clear message when no embeddings are present. | reads embeddings |
| `fileid info <path-or-id>` | A file's metadata, flags, tags, people, and a text snippet. | No |
| `fileid people` | Person clusters (id, name, face count). Empty until a full engine scan with face models has run. | reads only |
| `fileid dedupe [--exact\|--similar] [--threshold N]` | List duplicate groups. `--exact`: byte-identical by BLAKE3 `content_hash`. `--similar`: near-dups by perceptual-hash Hamming distance (default ≤ 8). | reads only |
| `fileid dedupe --apply [--similar] [--dry-run] [--delete] [--yes]` | Keep one file per group, remove the rest — to Trash/Recycle Bin (recoverable; Windows + Linux) or permanently with `--delete`. SAFE: nothing removed without `--apply`; prompts unless `--yes`. | reads signal |
| `fileid restructure --plan [root]` | Compute + print the proposed reorg using the engine's exact rule cascade. Read-only. | No |
| `fileid restructure --apply [--dry-run] [--symlinks] [--yes] [root]` | Execute the plan via the engine's exact `applyRestructure` code path (collision-uniquify, stale-plan + path-traversal guards, undo journal). `--symlinks` previews without moving. Prompts unless `--yes`. | No |

### Global flags

- `--json` — machine-readable output (everything human-facing has a JSON form).
- `--quiet` — suppress progress (stderr); JSON/stdout is unaffected.
- `--no-color` — disable ANSI color.
- `--db <path>` — explicit library SQLite file.

### Library location

Precedence for the library DB:

1. `--db <path>`
2. `$FILEID_DB`
3. `$CFFIXED_USER_HOME/fileid.sqlite` (parity with the macOS app's sandbox var;
   handy for isolating a test library)
4. The engine default via `fileid_engine::paths::db_path()` —
   `$XDG_DATA_HOME/FileID/fileid.sqlite` (`~/.local/share/FileID/…`) on
   macOS/Linux, `%LOCALAPPDATA%\FileID\fileid.sqlite` on Windows. **This is the
   same file the desktop apps read/write**, so `fileid search`/`info` work
   against a library a desktop app already scanned.

To isolate a library (e.g. for scripting or tests) without touching the real
one, pass `--db /tmp/lib.sqlite` or set `XDG_DATA_HOME`.

## The full pipeline (`scan --models`)

The default `scan` is **model-free**: filenames + plain-text content for FTS.
`scan --models` runs the **full ML pipeline** — RAM++ image tags, CLIP
embeddings, face detect/embed/cluster, perceptual + content hashes, binary-
document text. The engine's `startScan` hard-requires the AI models and owns its
own async + ORT runtime, so this path **spawns the `FileIDEngine` binary** and
speaks newline-delimited JSON over stdio (reusing the engine's own
`ipc::IpcCommand` / `IpcEvent` types — no schema drift), exactly as the desktop
apps do. It writes the engine's own library (`$XDG_DATA_HOME` / `%LOCALAPPDATA%`
location), so a pinned `--db` is reported as not-applicable here.

> **macOS:** `--models` works here once you install the engine's **own** models
> with **`fileid models download --all`**. The Rust engine needs its own model
> layout (`mobileclip_s2` / `arcface`), which the macOS desktop app's Swift
> CoreML models don't satisfy — so the CLI/TUI install + read the engine's
> weights under `~/.local/share/FileID/Models` (kept separate from the app's
> read-only CoreML dir). Alternatively, **scan with full ML in the FileID desktop
> app** and query that library with the read commands above. `--models` works the
> same way on **Linux and Windows**, where the Rust engine is native.

Two pre-flights before it spawns anything:

1. **Models installed?** Mirrors the engine's `startScan` gate (`mobileclip_s2`
   + `arcface` sentinels). If missing, it prints which models are missing, the
   models directory, and how to install them (**`fileid models download --all`**,
   or the desktop app's Welcome screen / Settings → Local AI; see
   `shared/docs/MODELS.md`).
2. **Engine binary located?** Looks at `$FILEID_ENGINE_BIN`, next to the
   `fileid` executable, the dev-layout engine `target/` dir, then `PATH`. If
   absent, it says how to provide it.

Install the models with **`fileid models download --all`** (the CLI's own
downloader — user-initiated HF egress + SHA-256 pinning; the desktop app's
installer also works). Once installed, `fileid scan --models <path>` lights up
`people`, `dedupe --exact/--similar`, and `search --similar` on **all three
platforms** (macOS included; see the note above). A desktop-app scan remains an
alternative way to populate those columns.

A terminal UI (ratatui) over the same in-process read surface now ships as a
sibling crate — see [`platforms/tui`](../tui).

## Examples

```bash
# Index a folder into an isolated library, then search it (no models needed)
fileid --db /tmp/lib.sqlite scan ~/Documents
fileid --db /tmp/lib.sqlite search invoice 2024
fileid --db /tmp/lib.sqlite --json info ~/Documents/invoice.pdf

# Full ML pipeline on the engine's library — Linux/Windows; needs installed
# models (on macOS, scan with full ML in the desktop app instead)
fileid scan --models ~/Pictures

# Query the library a desktop app (or `scan --models`) already built
fileid people
fileid search --similar ~/Pictures/IMG_4011.jpg --limit 20
fileid dedupe --similar --threshold 8

# Destructive actions are opt-in, prompt unless --yes, and have --dry-run
fileid dedupe --apply --dry-run                 # preview: keep one per group
fileid dedupe --apply --yes                     # → Trash/Recycle Bin (recoverable)
fileid restructure --apply --dry-run ~/Pictures # preview the reorg
fileid restructure --apply --symlinks ~/Pictures # preview as symlinks, no move
```
