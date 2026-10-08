# IPC schema — canonical contract

`ipc.schema.json` is the single source of truth for the wire protocol between the FileID app and the FileIDEngine. Every platform (macOS Swift, Windows Rust + C#, Linux) implements types that conform to it.

## Wire format

Newline-delimited JSON over stdin/stdout (or any byte-stream transport that preserves line boundaries). Each line is a JSON value matching either `IPCCommand` (app→engine) or `IPCEvent` (engine→app).

The discriminated union for `CommandPayload` and `EventPayload` uses **Swift Codable's externally-tagged shape**: a one-key object where the key is the variant name and the value is the payload object. Empty payloads are encoded as `{}`. Variants whose Swift case has a single unnamed associated value (e.g. `case ready(EngineInfo)`) wrap the payload in `{"_0": ...}` — this is Swift's auto-synthesis behavior, and the schema documents it explicitly so non-Swift implementations can match it byte-for-byte.

JSON object **key order is not significant and is platform-dependent** (Swift may sort; the Rust engine and C# app emit declaration order). Consumers MUST parse key-order-independently — every JSON parser does — and MUST NOT byte-compare serialized messages across platforms. (The earlier "alphabetical / byte-deterministic" wording was aspirational and not implemented by the Rust/C# emitters.) Dates are ISO8601 strings. Binary blobs are base64.

## Code generation

IPC v1.8 adds optional `CatalogRequest.timelineMode` for `enqueueTimeline`: `sampled` preserves the existing fast mode; `moments` requests overlapping frame-sequence evidence. Omission remains compatible with older clients. A platform without the worker must return an explicit unsupported error. Swift, Rust, and C# mirror the field; Linux uses the Rust DTO. A successful window records sampled coverage, not proof of complete visual observation or absence of an event.

Each platform's "generated" types currently live as hand-maintained files that a human keeps in sync with `ipc.schema.json`:

| Platform | File |
|---|---|
| Swift (macOS) | `platforms/apple/shared/Sources/FileIDShared/IPCProtocol.swift` |
| Rust (Windows engine) | `platforms/windows/src/engine/src/ipc/mod.rs` |
| C# (Windows app) | `platforms/windows/src/FileID.IpcSchema/CommandPayload.cs`, `EventPayload.cs`, and `CatalogProtocol.cs`, `ToolProtocol.cs`, and `ChatProtocol.cs` |

The `generators/` subdirectory will hold scripted codegen once the schema settles. Until then, when adding/modifying a variant:

1. Update `ipc.schema.json` first.
2. Update the per-platform DTO files to match.
3. Add a round-trip test on each platform that exercises the new variant.
4. Run all platforms' tests; all must encode the same logical message and conform to the schema.

## Versioning

The schema is versioned in its top-level `version` field. **Backward-incompatible changes** (renamed/removed variants, renamed fields, type narrowing) require a major version bump and coordinated commits across every platform. **Backward-compatible additions** (new variant, optional field) bump the minor version.

The current major version is `1.x`. Engines reject command frames with an unrecognized variant name with `IPCEvent.error(EngineError(kind: "ipc_unknown_command", ...))`.

## Privacy clause

Every payload field carrying user-content data (file paths, OCR text, EXIF) is logged through path-redaction primitives (`PathRedaction.swift` on Apple; `redact_path_for_log` in Rust; equivalent on C#). The schema's role is contract, not privacy enforcement — but the codegen targets must wire payloads through the redactor for any log output.

## Catalog v1.1

`catalogRequest`/`catalogResponse` carry typed search, chapter edit/Undo, and durable timeline job controls. Swift mirrors are in `CatalogProtocol.swift`; Rust mirrors are in `ipc/catalog.rs`, shared by Windows and Linux. Optional C# fields omit nulls to match Swift/Rust. Timeline execution is currently macOS-only; Rust rejects unavailable enqueue/resume actions explicitly. Tool capabilities and export operations are available through the v1.2 contract; chat and general analysis controls remain pending.

## Tools v1.2

`toolRequest`/`toolResponse` carry capabilities, history, immutable export preview, execution, cancellation, and Undo. Swift mirrors are in `ToolProtocol.swift`, Rust in `ipc/tools.rs`, and C# in `ToolProtocol.cs`. See `shared/docs/TOOLS.md` for exact supported pairs and adapter limits. Cancellation is available in the macOS worker; the Rust adapter reports it unavailable.

### v1.3 local chat

`chatRequest` carries typed send/history/clear/cancel actions. `chatResponse` streams retrieval/queue/progress/completion with local messages and evidence hits. Model summaries are capability-specific; schema support does not imply that a platform has a generation runtime or native panel. See shared/docs/CHAT.md.


### v1.6 native catalog retrieval

Optional `searchMode` (`keyword`, `semantic`, `hybrid`), `queryVector` (512 finite normalized values), `embeddingModel` `limit` (1–100), and `resultScope` (`all`/`files`) extend existing catalog search requests. Native CLIP semantic requests use either a compatible vector/model pair or a file-ID seed; hybrid requests also need a nonempty query. Existing keyword requests remain valid. File scope deduplicates ranked moments before applying the result limit so a single video cannot crowd out the Library grid. Responses can return `indexing` while a local cache is prepared, `ok` with timestamp/page evidence, or an explicit `error`; callers must handle unavailable modes. Rust/C# mirror the fields, but PC visual execution is deferred and returns an explicit error rather than silently treating vectors as keywords. See ARCHITECTURE.md for snapshot/change-log behavior.

IPC v1.9 adds optional `ToolRecipe.allowUpscale`. Omitted or false keeps photo downsizing behavior; true allows conventional enlargement to `maxDimension`. It is accepted only for photo recipes and does not request AI super-resolution. Swift, Rust, and C# mirror the optional field; Linux uses the Rust DTO.
