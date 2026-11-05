IPC v1.17 adds optional `ChatRequest.context` (`ChatContext`: page, selected file IDs, open person IDs, folder, library root), optional `ChatRequest.queryVector`/`embeddingModel` (a client CLIP text embedding for hybrid retrieval), and optional `ChatResponse.actions` (`ChatAction`). An action names one of 17 kinds mapped to existing commands or pages. `requiresConfirmation` is fixed per kind and true exactly for moveDuplicatesToTrash, applyFolderLayout, undoLastMove, renameToSuggested, namePerson and mergePeople; clients must show a confirmation card before sending anything for those, and drop actions whose flag or kind disagrees. Swift, Rust and C# mirror the types; omission keeps older callers compatible.

IPC v1.17 adds zero-setup Best Takes to the catalog envelope: `autoTakeGroups` returns `takeGroups` (suggested groups of tries from capture time, CLIP and pHash; a RAW and its JPEG are one `CatalogTakeShot`), `scoreTakes` returns model-free `takeQualities` (relative score, rank, recommended flag, at most three plain reasons, face and subject boxes), `decideTakeGroup` stores Keep all / Skip / reopen through the optional `takeDecision`, and `rankTakesWithGoal` returns `takeGoalRanking` from one local-model pass over a contact sheet (status `unavailable` when no description model is loaded, always on the shared engine for now). The macOS engine's `trashFiles` now returns a `trashFiles:<batch>` action and implements `restoreFromTrash`. Swift, Rust and C# mirror every field.

IPC v1.15 adds optional `ChatRequest.modelKind`. A Mac chat send only summarizes with that already-loaded registered model; selecting/sending never downloads. Omission retains compatibility with older callers. `ToolRecipe.allowUpscale` now permits conventional photo or video enlargement on macOS, including a 3840-pixel video target. Portable adapters still reject unavailable enlargement/video/stabilization recipes through capabilities. Mac catalog `detail` also returns current cast appearances in existing `hits` (`kind=person`, `evidenceID=person:…`); absent timestamps remain explicitly untimed.

IPC v1.14 adds optional `ToolRecipe.videoStabilization`. Omitted or false retains motion. macOS enabled mode estimates frame-to-frame translation/rotation, smooths camera motion, and reports uncorrected unsafe or unavailable registrations. It supports SDR clips up to ten minutes and 60 fps, with at most 12 percent border-covering zoom; social framing can be combined. Other platforms reject enabled stabilization until their adapter exists. It does not synthesize missing borders or perform AI enhancement.

IPC v1.13 adds optional `ToolRecipe.videoFraming`: `fit` (the default) or `auto`. macOS auto mode follows detected people in a selected social frame, smooths crop paths, and fits/pads when detections are missing or a group cannot safely fit. It samples twice per second and accepts clips up to ten minutes. Auto requires a non-source aspect ratio. Other platforms reject auto recipes until their adapter is implemented. This does not provide identity selection, editable keyframes, object tracking, stabilization, or AI enhancement.

IPC v1.13 also adds optional `FaceClusteringResult.deferredFaces`. Portable grouping processes one compatible selected-model/source partition while preserving deferred identities and corrections, and reports the count of active faces awaiting usable evidence. Omission means coverage is unavailable; it must not be displayed as a verified zero. Swift/Rust/C# mirrors retain legacy omission and current counts. The result field does not establish calibrated identity accuracy or complete native backlog/retry controls.

# IPC schema — canonical contract

IPC v1.12 adds optional `ToolRecipe.videoAspectRatio`: `source`, `9:16`, `16:9`, `1:1`, or `4:5`. Omitted or `source` retains legacy video conversion. With framing omitted or `fit`, the macOS exporter fits the full picture in the requested frame and pads unused space; it does not track a subject, crop footage, or recover scenery outside the source. Swift, Rust, and C# DTOs mirror the field. The portable Rust tool worker rejects framed recipes until a video adapter is implemented.

IPC v1.11 adds `suggestTakeGroups` to the catalog envelope and the optional `suggestedTakeGroups` response. Each proposal contains local file paths, an embedding cosine similarity, and a reason; it is read-only and never asserts that a requested event happened. The request requires a bounded `fileIDs` scope. Mac, Rust, and C# DTOs mirror the canonical schema.

IPC v1.10 adds typed `CatalogEvent`, `CatalogTakeFeedback`, `CatalogTake`, and `CatalogTakeRecommendation` payloads and optional `events`, `takes`, and `recommendation` response fields. `listEvents`, `saveEvent`, `deleteEvent`, `undoEventEdit`, `takeGroup`, `setTakeFeedback`, and `undoTakeFeedback` use the existing catalog request/response envelope. Swift, Rust (Windows/Linux), and C# DTOs mirror the fields. Empty outcome evidence returns `insufficient`; it must not be presented as an automatically detected missed event.

`ipc.schema.json` is the single source of truth for the wire protocol between the FileID app and the FileIDEngine. Every platform (macOS Swift, Windows Rust + C#, Linux) implements types that conform to it.

## Wire format

Newline-delimited JSON over stdin/stdout (or any byte-stream transport that preserves line boundaries). Each line is a JSON value matching either `IPCCommand` (app→engine) or `IPCEvent` (engine→app).

The discriminated union for `CommandPayload` and `EventPayload` uses **Swift Codable's externally-tagged shape**: a one-key object where the key is the variant name and the value is the payload object. Empty payloads are encoded as `{}`. Variants whose Swift case has a single unnamed associated value (e.g. `case ready(EngineInfo)`) wrap the payload in `{"_0": ...}` — this is Swift's auto-synthesis behavior, and the schema documents it explicitly so non-Swift implementations can match it byte-for-byte.

JSON object **key order is not significant and is platform-dependent** (Swift may sort; the Rust engine and C# app emit declaration order). Consumers MUST parse key-order-independently — every JSON parser does — and MUST NOT byte-compare serialized messages across platforms. (The earlier "alphabetical / byte-deterministic" wording was aspirational and not implemented by the Rust/C# emitters.) Dates are ISO8601 strings. Binary blobs are base64.

## Code generation

IPC v1.8 adds optional `CatalogRequest.timelineMode` for `enqueueTimeline`: `sampled` preserves the existing fast mode; `moments` requests overlapping frame-sequence evidence. Omission remains compatible with older clients. A platform without the worker must return an explicit unsupported error. Swift, Rust, and C# mirror the field; Linux uses the Rust DTO. A successful window records sampled coverage, not proof of complete visual observation or absence of an event.

`shared/scripts/gen_ipc_dtos.py` turns this schema into DTO source. Per-language details that JSON Schema cannot express (type names for inline objects, integer widths, typed enums, omission rules) live in `codegen.json`.

| Platform | File | How it stays in sync |
|---|---|---|
| Rust (engine; Windows, Linux, CLI, TUI) | `platforms/windows/src/engine/src/ipc/generated.rs` | Generated. Do not edit. |
| C# (Windows app) | `platforms/windows/src/FileID.IpcSchema/*.cs` | Hand-written; `--check` compares every type's wire keys with the schema |
| Swift (macOS) | `platforms/apple/shared/Sources/FileIDShared/IPCProtocol.swift`, `CatalogProtocol.swift`, `ChatProtocol.swift`, `ToolProtocol.swift` | Hand-written; `--check` compares every type's wire keys with the schema |

When adding or changing a variant or field:

1. Update `ipc.schema.json` first (and `codegen.json` if Rust needs a non-default type or name).
2. Run `python3 shared/scripts/gen_ipc_dtos.py` to regenerate the Rust DTOs.
3. Update the C# and Swift mirrors until `python3 shared/scripts/gen_ipc_dtos.py --check` passes. The Repository policy workflow runs the same check.
4. Add a round-trip test on each platform that exercises the change, and run all platforms' tests.

## Versioning

The schema is versioned in its top-level `version` field. **Backward-incompatible changes** (renamed/removed variants, renamed fields, type narrowing) require a major version bump and coordinated commits across every platform. **Backward-compatible additions** (new variant, optional field) bump the minor version.

The current major version is `1.x`. Engines reject command frames with an unrecognized variant name with `IPCEvent.error(EngineError(kind: "ipc_unknown_command", ...))`.

## Privacy clause

Every payload field carrying user-content data (file paths, OCR text, EXIF) is logged through path-redaction primitives (`PathRedaction.swift` on Apple; `redact_path_for_log` in Rust; equivalent on C#). The schema's role is contract, not privacy enforcement — but the codegen targets must wire payloads through the redactor for any log output.

## Catalog v1.1

`catalogRequest`/`catalogResponse` carry typed search, chapter edit/Undo, and durable timeline job controls. Swift mirrors are in `CatalogProtocol.swift`; Rust types are generated into `ipc/generated.rs`, shared by Windows and Linux. Optional C# fields omit nulls to match Swift/Rust. Timeline execution is currently macOS-only; Rust rejects unavailable enqueue/resume actions explicitly. Tool capabilities and export operations are available through the v1.2 contract; chat and general analysis controls remain pending.

## Tools v1.2

`toolRequest`/`toolResponse` carry capabilities, history, immutable export preview, execution, cancellation, and Undo. Swift mirrors are in `ToolProtocol.swift`, Rust in `ipc/generated.rs`, and C# in `ToolProtocol.cs`. See `shared/docs/TOOLS.md` for exact supported pairs and adapter limits. Cancellation is available in the macOS worker; the Rust adapter reports it unavailable.

### v1.3 local chat

`chatRequest` carries typed send/history/clear/cancel actions. `chatResponse` streams retrieval/queue/progress/completion with local messages and evidence hits. Model summaries are capability-specific; schema support does not imply that a platform has a generation runtime or native panel. See shared/docs/CHAT.md.


### v1.6 native catalog retrieval

Optional `searchMode` (`keyword`, `semantic`, `hybrid`), `queryVector` (512 finite normalized values), `embeddingModel` `limit` (1–100), and `resultScope` (`all`/`files`) extend existing catalog search requests. Native CLIP semantic requests use either a compatible vector/model pair or a file-ID seed; hybrid requests also need a nonempty query. Existing keyword requests remain valid. File scope deduplicates ranked moments before applying the result limit so a single video cannot crowd out the Library grid. Responses can return `indexing` while a local cache is prepared, `ok` with timestamp/page evidence, or an explicit `error`; callers must handle unavailable modes. Rust/C# mirror the fields, but PC visual execution is deferred and returns an explicit error rather than silently treating vectors as keywords. See ARCHITECTURE.md for snapshot/change-log behavior.

IPC v1.9 adds optional `ToolRecipe.allowUpscale`. Omitted or false keeps photo downsizing behavior; true allows conventional enlargement to `maxDimension`. It is accepted only for photo recipes and does not request AI super-resolution. Swift, Rust, and C# mirror the optional field; Linux uses the Rust DTO.

Catalog `search` accepts optional `capturedFrom` / `capturedTo` (Unix seconds, `capturedTo >= capturedFrom`): results are limited to files whose `COALESCE(created_at, modified_at)` falls inside the window. The engines filter the top 100 candidates, then apply `limit`.
