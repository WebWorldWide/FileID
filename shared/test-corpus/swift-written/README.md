# State the Swift engine wrote

Each folder is what the Swift engine left behind after the first half of a cross-engine
conformance case: its catalog, the journals and records beside it, and the library files
its rows describe. A case in `shared/test-corpus/conformance/swift-written.json` seeds one of
these folders and starts the Rust engine on it, so the Rust engine reading Swift-written
state stays tested after the Swift engine is deleted. The original cross-engine cases
(`requires: ["swift", "rust"]`) stay until then.

## Folder layout

| File | What it holds |
|---|---|
| `catalog.sql` | The catalog as SQL: every schema object exactly as stored, one `INSERT` per row, triggers and indexes created after the data. Full-text indexes are rebuilt from their text on load (their segment blobs depend on insertion history and held the capture folder's path tokens). |
| `fixture.json` | Format version, the case it came from, the catalog pragmas (`journal_mode`, `page_size`, ...), the values later steps need (`vars`: operation ids and the like), the chat conversation id, the folders, and every file with its modification time in nanoseconds. |
| `tree/` | The library files, byte for byte. A file that is a copy of a repo media file is only named (`copy`) in `fixture.json`. |
| `state/FileID/` | Files the engine kept beside the catalog (`trash-journal.json`, `path-moves.ndjson`, `restructure_undo.ndjson`, ...). `state/Trash/` is the test Trash root. Logs, the catalog files and empty scratch folders are left out. |

Nothing machine-specific is stored. The harness (`shared/scripts/conformance.py`) expands
these placeholders when it seeds a folder: `{root}`, `{state}`, `{std_root}`, `{std_state}`
(the folders as Foundation spells them, without the `/private` of a macOS temp folder),
the same with `:json` where the Swift encoder escaped `/` as `\/`, `{inode:rel}`,
`{state_inode:rel}`, `{path_hash:rel}`, `{std_path_hash:rel}` and `{device}`. Files are
recreated with their recorded modification times, because the engines compare size and
modification time with what the catalog holds. Timestamps inside the catalog are those of the
capture run, so a fixture does not age; nothing compares them with the clock.

The catalog is SQL, not a `.sqlite` file, because it is readable in review and its paths
can be templated. A fresh catalog with a few rows is about 40 KB as SQL. The dump rebuilds the
catalog exactly: `--capture-fixture` loads the SQL it just wrote and requires the same
`sqlite_master` (type, name, table, SQL), the same rows in every table (the shadow tables of
the full-text indexes are compared by their text and terms), and the same `user_version`,
`application_id`, `page_size` and `auto_vacuum` as the live catalog.

## Capture and check

Capture needs a Swift engine built as `.github/workflows/macos.yml` builds the test one
(`swift build --product FileIDEngine -Xswiftc -DFILEID_TEST_TRASH_ROOT`) and runs it isolated
(state, HOME and CFFIXED_USER_HOME in a scratch folder; no models). The harness runs the case up to its
first `restart` to the other engine, stops the Swift engine, and writes the folder:

    python3 shared/scripts/conformance.py --engine <swift engine> --test-trash-root \
        --case <case> --capture-fixture shared/test-corpus/swift-written/<folder>

Check any folder without an engine (it seeds the folder into a scratch library and captures it
again; the result must be the same folder):

    python3 shared/scripts/conformance.py --verify-fixture <folder> [--verify-fixture ...]

| Folder | Captured from the Swift half of | Used by case (`swift-written-...`) |
|---|---|---|
| `tools-chapters-vtt` | `cross-tools-swift-previews-chapters-rust-executes` | `tools-chapters-vtt` |
| `tools-photo-jpeg` | `cross-tools-swift-preview-rust-execute-jpeg` | `tools-photo-jpeg` |
| `tools-photo-png-enlarge` | `cross-tools-swift-preview-rust-execute-png-enlarge` | `tools-photo-png-enlarge` |
| `video-fit` | `cross-video-swift-preview-rust-execute-fit` | `video-fit` |
| `media-rotate` | `cross-media-swift-preview-rust-execute-rotate` | `media-rotate` |
| `media-trim` | `cross-media-swift-preview-rust-execute-trim` | `media-trim` |
| `catalog-chapter-edit` | `cross-catalog-chapter-edit-and-undo` | `catalog-chapter-edit` |
| `chat-history` | `cross-chat-history-and-filters` | `chat-history` |
| `takes-grouping` | `cross-takes-grouping-swift-then-rust` | `takes-grouping` |
| `names-learned-style` | `cross-names-learned-style` | `names-learned-style` |
| `trash-journal` | `cross-trash-swift-trashes-rust-puts-back` | `trash-journal-imported`, `trash-log-dual-written` |
| `scan-mtimes-chapter` | `cross-scan-swift-then-rust-keeps-modified-times-and-reviewed-rows` | `scan-mtimes-chapter` |
| `scan-image-hashes` | `cross-scan-swift-then-rust-same-image-hashes` | `scan-image-hashes` |
| `face-queues` | `cross-face-queues-merge-split-and-undo` | `face-queues` |
| `face-review` | `cross-face-review-resolve-and-undo` | `face-review` |
| `people-merge` | `cross-people-merge-undo-swift-merge-rust-undo` | `people-merge` |
| `restructure-apply` | `cross-restructure-swift-apply-rust-undo` | `restructure-apply`, `path-moves-repaired` |
| `mlx-install-record` | `cross-engine-mlx-install-record-swift-first` | `mlx-install-record` |

Notes on single folders:

- `trash-journal` holds both what the Swift engine dual-writes beside `trash-journal.json`
  (`trash_log.json`, signed, and its `log-hmac.key`) and the journal itself. The case
  `trash-journal-imported` leaves the signed log and key out (`fixtureOmit`), so the Rust
  engine must import the journal, as it must for a library whose Swift build predates the
  signed log; `trash-log-dual-written` seeds all of it.
- `restructure-apply` holds the headerless `restructure_undo.ndjson` and the `path-moves.ndjson`
  of a finished apply. `path-moves-repaired` derives the state of a kill between a rename and
  its done line from it: the case drops the `done` lines (`fixtureDropLines`) and puts two
  catalog rows back to their old paths (`sql`). That state is derived, not captured: no way
  to stop the Swift engine at exactly that point exists.
- `timeline-cross-swift-then-rust` is not captured: it needs an installed MLX model, none is
  available to the harness here. Re-capture it where one is installed.

## Older libraries

`older-catalog-vNN` are catalogs of earlier schema versions, made by
`shared/scripts/make_older_catalogs.py`: it copies the Swift engine's migrator verbatim out of
`Database.swift`, compiles it against the GRDB the Swift package built, runs GRDB's own
`migrate(upTo:)` to the version, inserts a few rows by SQL (files, tags, text, a person and
face, and the tables that version has) and captures the folder. The cases
`swift-written-older-catalog-v20|v21|v28|v35` start the Rust engine on each and require
v36 with every row intact and the schema of a freshly written v36 catalog (`trash-journal`).
They were made as:

    python3 shared/scripts/make_older_catalogs.py --swift-build <SwiftPM scratch path of a built platforms/apple> \
        --out shared/test-corpus/swift-written --version 20 --version 21 --version 28 --version 35

`v20` is the schema of the only tag, `v0.1.0` (the last migration it registers is
`v20_vlm_full_model`); `v21` is the first with the catalog tables; `v28` is mid-chain; `v35`
is the version before the current one. The rows are inserted by SQL, not written by a scan,
because no Swift engine of those versions exists to run.
