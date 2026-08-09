# IPC schema — canonical contract

`ipc.schema.json` is the single source of truth for the wire protocol between the FileID app and the FileIDEngine. Every platform (macOS Swift, Windows Rust + C#, future Linux) implements types that conform to it.

## Wire format

Newline-delimited JSON over stdin/stdout (or any byte-stream transport that preserves line boundaries). Each line is a JSON value matching either `IPCCommand` (app→engine) or `IPCEvent` (engine→app).

The discriminated union for `CommandPayload` and `EventPayload` uses **Swift Codable's externally-tagged shape**: a one-key object where the key is the variant name and the value is the payload object. Empty payloads are encoded as `{}`. Variants whose Swift case has a single unnamed associated value (e.g. `case ready(EngineInfo)`) wrap the payload in `{"_0": ...}` — this is Swift's auto-synthesis behavior, and the schema documents it explicitly so non-Swift implementations can match it byte-for-byte.

JSON object **key order is not significant and is platform-dependent** (Swift may sort; the Rust engine and C# app emit declaration order). Consumers MUST parse key-order-independently — every JSON parser does — and MUST NOT byte-compare serialized messages across platforms. (The earlier "alphabetical / byte-deterministic" wording was aspirational and not implemented by the Rust/C# emitters.) Dates are ISO8601 strings. Binary blobs are base64.

## Code generation

Each platform's "generated" types currently live as hand-maintained files that a human keeps in sync with `ipc.schema.json`:

| Platform | File |
|---|---|
| Swift (macOS) | `platforms/apple/shared/Sources/FileIDShared/IPCProtocol.swift` |
| Rust (Windows engine) | `platforms/windows/src/engine/src/ipc/mod.rs` |
| C# (Windows app) | `platforms/windows/src/FileID.IpcSchema/Generated.cs` |

The `generators/` subdirectory will hold scripted codegen once the schema settles. Until then, when adding/modifying a variant:

1. Update `ipc.schema.json` first.
2. Update the per-platform DTO files to match.
3. Add a round-trip test on each platform that exercises the new variant.
4. Run all platforms' tests; all must encode the same byte string for the same logical message.

## Versioning

The schema is versioned in its top-level `version` field. **Backward-incompatible changes** (renamed/removed variants, renamed fields, type narrowing) require a major version bump and coordinated commits across every platform. **Backward-compatible additions** (new variant, optional field) bump the minor version.

The current major version is `1.x`. Engines reject command frames with an unrecognized variant name with `IPCEvent.error(EngineError(kind: "ipc_unknown_command", ...))`.

## Privacy clause

Every payload field carrying user-content data (file paths, OCR text, EXIF) is logged through path-redaction primitives (`PathRedaction.swift` on Apple; `redact_path_for_log` in Rust; equivalent on C#). The schema's role is contract, not privacy enforcement — but the codegen targets must wire payloads through the redactor for any log output.
