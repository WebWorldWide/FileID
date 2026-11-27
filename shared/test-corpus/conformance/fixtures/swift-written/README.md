# Swift-written fixtures

State the retired Swift engine (`platforms/apple/engine`, deleted with the engine) wrote, kept so the shared Rust engine stays able to read libraries and journals an older Mac app left behind. Captured with the Swift engine as of `081b554a` (a debug build with `-DFILEID_TEST_TRASH_ROOT`), by running the first half of the cross-engine conformance case of the same name, then stopping the engine. The Swift engine cannot be rebuilt from `main`; check that commit out to regenerate one.

## Conformance fixtures (`<case name>/`)

Used by the cases in `../../*.json` that name `"fixture": "<case name>"` (see `shared/scripts/conformance.py`):

- `state/FileID/…` the engine's state folder as the Swift engine left it: `fileid.sqlite` (GRDB, schema v36, WAL checkpointed), and for the scan cases `capture-dates-v1/`.
- `tree/…` files the Swift run added or changed in the library tree; the rest of the tree is generated from the case's own `tree`.
- `fixture.json`: `mtimes` (every tree file's modification time as of the capture), `removed` (tree files the run deleted), `vars` (the step values the first half saved, such as an operation id) and, for chat, the `conversation` id.

The paths inside the catalog are the ones the capture ran under, `/private/tmp/fileid-conformance-fixture/<case name>/{tree,state}`, so the harness replays each case in that folder. Only the file identities (`files.file_ref`, an inode) are refreshed to the replayed files.

## Journals (`journals/`)

Read by Rust unit tests (`journal_import.rs`, `path_moves.rs`, `restructure_apply.rs`):

- `trash-journal.json`, `trash-journal-faces.json`: `trash-journal.json` after the Swift engine trashed one file (with its tag), and one file with two faces. Paths are under `/private/tmp/fileid-conformance-fixture/journal-trash{,-faces}`.
- `path-moves.ndjson`: the intent and done lines the Swift engine wrote while applying a restructure (eight moves). Paths are under `…/swift-written-restructure-apply/tree`.
- `restructure_undo.ndjson`: that restructure's undo journal, as the current Swift engine writes it (library-root header, `version` 3, `source_identity` per entry).
- `restructure_undo-headerless.ndjson`: one entry in the format older Swift engines wrote before the header. The current Swift engine can no longer produce it, so this line is written by hand in that format.

A person merge the Swift engine journaled is covered by the `swift-written-people-merge` catalog (`catalog_operations`, `catalog_corrections`), which `person_merge_tests.rs` also opens.

The catalogs also pin the schema: `db/migrations.rs` checks that a catalog the Swift engine wrote lists exactly the migrations registered there.
