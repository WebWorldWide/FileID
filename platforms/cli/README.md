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

1. The MVP is **read/query + plan**. Most of these operations (search, info,
   people, dedupe) have **no IPC command** — the desktop apps run them as
   direct read-only SQL against the engine's DB. The CLI does the same.
2. The engine's `startScan` IPC **hard-requires ML models**, which is
   incompatible with a model-free CLI. The CLI's `scan` is a model-free FTS
   indexer that writes through the engine's own schema/migrations.

Reusing the engine crate (path dependency) means the CLI **can't drift** from
the engine: same tables, same migrations, same `FileKind`, same restructure
logic.

## Build

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
| `fileid search <query…> [--limit N] [--similar]` | FTS5 keyword search over document text + image OCR text, plus a filename fallback. `--similar` (semantic/CLIP) prints a "needs models" notice. | No (FTS) |
| `fileid info <path-or-id>` | A file's metadata, flags, tags, people, and a text snippet. | No |
| `fileid people` | Person clusters (id, name, face count). Empty until a full engine scan with face models has run. | reads only |
| `fileid dedupe [--exact\|--similar] [--threshold N]` | `--exact`: byte-identical groups by BLAKE3 `content_hash`. `--similar`: near-dups by perceptual-hash Hamming distance (default ≤ 8, the engine's threshold). | reads only |
| `fileid restructure --plan [root]` | Compute + print the proposed reorg using the engine's exact rule cascade. Read-only — never moves files. | No |

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

## What the CLI does *not* do (documented follow-ons)

The CLI's `scan` is **model-free**: it indexes filenames + plain-text/markdown/
source content for FTS. It does **not** run the ML pipeline (RAM++ image tags,
CLIP embeddings, face detection/clustering, perceptual/content hashes) or
extract text from binary documents (`.docx`/`.pdf`). Those are produced by a
full engine scan and light up `people`, `dedupe`, and semantic `search` when
present. Planned follow-ons:

- **`apply`** for restructure / rename / trash (maps to `applyRestructure`,
  `bulkAction`, `trashFiles` — destructive, deliberately out of the MVP).
- **Semantic search** wiring (`embedTextQuery` → CLIP) once models are wired in.
- A **full-pipeline `scan --models`** path that spawns/drives the engine's
  `startScan` for ML enrichment.
- An optional **TUI** (ratatui) on top of these same in-process calls.

## Examples

```bash
# Index a folder into an isolated library, then search it (no models needed)
fileid --db /tmp/lib.sqlite scan ~/Documents
fileid --db /tmp/lib.sqlite search invoice 2024
fileid --db /tmp/lib.sqlite --json info ~/Documents/invoice.pdf

# Query the library your desktop app already built
fileid people
fileid dedupe --similar --threshold 8
fileid restructure --plan ~/Pictures
```
