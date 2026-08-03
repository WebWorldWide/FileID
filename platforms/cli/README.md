# FileID CLI (`fileid`)

The cross-platform command-line interface for FileID. It indexes and searches
local files, inspects tags and people, finds duplicates, manages the curated AI
models, and previews or applies a restructure. There is no telemetry or cloud
processing. Network access occurs only when the user starts a model/runtime
download.

The CLI links the shared Rust engine for its schema, migrations, classifiers,
duplicate safety checks, restructure apply path, and IPC types. The same binary
builds on macOS, Linux, and Windows.

## Install

From the repository root:

```bash
bash scripts/build-tools.sh
fileid --help
```

The script builds `FileIDEngine`, `fileid`, and `fileid-tui` in release mode and
installs them in `~/.cargo/bin`. On Windows, the PowerShell installer also
stages ONNX Runtime and DirectML:

```powershell
.\scripts\build-tools.ps1
fileid.exe --help
```

To build only this crate:

```bash
cd platforms/cli
cargo build --release
./target/release/fileid --help
```

Rust 1.90 is pinned in `rust-toolchain.toml`. The engine is a path dependency,
so there is no separate library build step.

## Quick start

Start with a private DB while learning the commands:

```bash
fileid --db /tmp/fileid.sqlite scan ~/Documents
fileid --db /tmp/fileid.sqlite search "quarterly invoice"
fileid --db /tmp/fileid.sqlite info ~/Documents/invoice.pdf
fileid --db /tmp/fileid.sqlite dedupe
fileid-tui --db /tmp/fileid.sqlite
```

The default scan is fast and model-free: it records every supported file and
indexes plain-text formats such as TXT, Markdown, CSV, JSON, YAML, and source
code. Use the engine pipeline for image tags, visual search, faces, hashes, and
binary-document extraction:

```bash
fileid models download mobileclip_s2 arcface --dry-run
fileid models download mobileclip_s2 arcface
fileid scan ~/Pictures --models
fileid people
fileid search --similar ~/Pictures/IMG_4011.jpg
```

After the engine scan completes, the CLI runs face clustering before reporting
success, so `fileid people` is immediately current.

## Commands

| Command | Behavior |
| --- | --- |
| `fileid scan <PATH>` | incremental, model-free file and plain-text index |
| `fileid scan <PATH> --rescan` | reprocess every file in the model-free index |
| `fileid scan <PATH> --models` | full engine pipeline, followed by face clustering |
| `fileid search <QUERY…> [--limit N]` | FTS content/OCR search plus literal filename fallback |
| `fileid search --similar <PATH-OR-ID>` | cosine similarity over stored CLIP embeddings |
| `fileid info <PATH-OR-ID>` | metadata, flags, tags, people, text, caption, and suggested name |
| `fileid people` | current person clusters and face counts |
| `fileid dedupe` / `--exact` | live SHA-256 verification of byte-identical files |
| `fileid dedupe --similar [--threshold 0..16]` | perceptual-hash near-duplicate groups |
| `fileid dedupe --apply …` | keep one file per group and remove the rest behind confirmation |
| `fileid restructure [--plan] [ROOT]` | read-only plan from the engine rule cascade |
| `fileid restructure --apply [ROOT]` | collision-safe moves with stale-plan/path guards and undo journal |
| `fileid models list` | install state, size, license, and pinned repository for each model |
| `fileid models download <NAME…>` | SHA-256-verified, user-initiated model install |
| `fileid runtime status` | report ONNX Runtime availability |
| `fileid runtime install` | one-time macOS ONNX Runtime setup when needed |

A bare `fileid` prints a guided tour. Every command has focused examples under
`fileid <command> --help`.

### Global options

| Option | Meaning |
| --- | --- |
| `--db <PATH>` | select a library SQLite file |
| `--json` | emit machine-readable command output |
| `--quiet` | suppress progress and non-essential stderr output |
| `--no-color` | disable ANSI styling; `$NO_COLOR` is also honored |

Search limits are constrained to 1–10,000. Ambiguous input such as combining a
query with `--similar`, combining model names with `--all`, or using mutation
options without `--apply` is rejected as usage error instead of silently
ignoring part of the command.

## Library selection

The DB path is resolved in this order:

1. `--db <PATH>`
2. `$FILEID_DB`
3. `$CFFIXED_USER_HOME/fileid.sqlite`
4. On macOS, the existing native-app library at
   `~/Library/Application Support/FileID/fileid.sqlite`
5. The engine default: `$XDG_DATA_HOME/FileID/fileid.sqlite` on Unix-like
   systems or `%LOCALAPPDATA%\FileID\fileid.sqlite` on Windows

On macOS, the native Swift app and Rust engine use different model formats and
may use different implicit data roots. An explicit `--db` is forwarded to a
spawned engine, guaranteeing that `scan --models` and subsequent reads use the
same library. If an implicit engine scan would write somewhere other than the
DB used by read commands, the CLI prints the exact follow-up `--db` path.

## Full-AI scan requirements

`scan --models` starts `FileIDEngine` and streams its canonical newline-JSON
events. It requires the engine binary plus these minimum models:

- `mobileclip_s2` for image embeddings
- `arcface` for face detection and recognition

The engine is resolved from `$FILEID_ENGINE_BIN`, beside `fileid`, repository
engine build directories, and then `PATH`. Models install under the engine's
writable model directory and are separate from the macOS app's Core ML bundle.

On macOS, check the dynamically loaded inference runtime once:

```bash
fileid runtime status
fileid runtime install
```

Use `fileid models list` for optional tagging, text-search, speech, and Deep
Analyze models. `--all --dry-run` reports the full size and license set without
downloading. Restricted models require their upstream terms to be accepted;
the CLI never treats acceptance as implicit. The canonical license record is
`shared/docs/MODELS.md`.

## Safe cleanup and restructure

Listing and planning commands are read-only. Mutations require `--apply` and a
TTY confirmation; non-interactive use must add `--yes` explicitly.

```bash
# Preview exactly what an apply would do
fileid dedupe --exact --apply --dry-run
fileid dedupe --similar --apply --dry-run --yes
fileid restructure --apply --dry-run ~/Pictures

# Recoverable removal where platform trash support is available
fileid dedupe --exact --apply

# Irreversible; always explicit
fileid dedupe --exact --apply --delete --yes

# Create the proposed layout as symlinks instead of moving originals
fileid restructure --apply --symlinks ~/Pictures
```

Exact apply revalidates both keeper and victim against the live file before
removal. Similar apply warns about transitive perceptual groups and requires an
explicit `--yes`. Restructure apply rechecks the plan, contains every
destination under the selected root, uniquifies collisions, and journals
successful operations.

## Automation

Use `--json --quiet --no-color` for scripts. Structured results stay on stdout;
diagnostics stay on stderr. Important exit codes:

| Code | Meaning |
| --- | --- |
| `0` | command completed successfully |
| `1` | runtime, data, model, or safety failure |
| `2` | invalid command-line usage (clap) |
| `3` | scan committed usable results but one or more files failed |

Examples:

```bash
fileid --json --quiet search "tax return" --limit 100
fileid --json models download ram_plus --dry-run
fileid --json dedupe --exact --apply --dry-run
```

## Verify

```bash
cd platforms/cli
cargo fmt --check
cargo check
cargo clippy --all-targets -- -D warnings
cargo test
```

The test suite uses isolated temporary libraries and covers scan/search/info,
JSON contracts, mutation gates, live duplicate revalidation, bounded similarity
search, restructure apply, model/runtime safety, partial-scan exit status, and
scan-to-face-clustering IPC sequencing.
