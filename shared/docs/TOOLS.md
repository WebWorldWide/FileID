# File tools: supported exports and recovery

Tools use typed IPC v1.2 requests. Preview resolves an exact catalog selection, fingerprints source bytes with SHA-256, reserves collision-safe output names, and stores an immutable operation plan. Execute uses that saved plan, checks sources and chapter snapshots again, stages each output, validates it, and publishes with a non-overwriting hard link. The original is never mutated. This requires an output filesystem that supports hard links; unsupported publication fails explicitly.

| Adapter | Inputs | Outputs | Limits and behavior |
|---|---|---|---|
| macOS ImageIO worker | Single-image PNG, JPEG, TIFF, HEIC | PNG, JPEG, TIFF | Orientation applied; maximum dimension 1–8192; downsize only; 8-bit SDR. Color space carried through when supported; camera/location metadata stripped. JPEG alpha flattened onto white. Native worker has a 30-second limit and supports cancellation/parent-death termination. |
| Rust image-rs | Single-image PNG, JPEG | PNG, JPEG, TIFF | EXIF orientation applied; APNG rejected; input at most 32 megapixels and bounded decode allocation. Embedded ICC inputs are rejected until color-managed conversion is added. Private metadata stripped; 8-bit output. JPEG alpha flattened onto white. This adapter currently runs decoders in-process and cannot cancel a running export. |
| Both chapter adapters | Current non-stale catalog markers | JSON, WebVTT | JSON retains marker provenance. WebVTT contains chapter cues, not speech transcripts. Cue text is escaped and zero-duration markers receive a 1 ms interval. |

No AI enhancement is implied by resize or conversion. RAW development, HDR-preserving conversion, HEIC output, video/audio tools, stabilization, AI upscaling, tracked reframing, subtitles from speech, FCPXML, and the broad document/archive/data/ebook/CAD toolbox remain in the implementation ledger. Capabilities describe the adapter actually running; they do not promise every format pair on every OS.

## History and Undo

Every output enters the catalog with an original/export relationship and its recipe. Completed receipts, output hashes, staging paths, and derived catalog IDs are journaled. History survives restart and engine changes. Undo checks all remaining exports before moving them into `ExportRecovery/<operation-id>` beside the internal catalog; edited exports are left untouched. Recovery paths are shown to the user, so Undo does not permanently delete generated files. Moving an export across filesystems can fail on the Rust adapter; the original remains untouched and the operation journal remains available.

Interrupted exports become failed operations instead of replaying a possibly ambiguous write. Registered staging files are cleaned after a short parent-worker grace period. Cleanup requires an owned UUID filename, the recorded output directory, a regular file, and a writable unprotected location; it preserves unrelated files and aliases. A crash between filesystem publication and receipt acknowledgement can leave a prepared output for manual review. General batch restart/resume and a fully reconciled publication journal remain release gates.

The native macOS File Tools panel is accessible from the existing toolbar. It supports catalog search, multi-selection, recipe/output-folder selection, exact preview, export, cancellation, persistent last-operation history, and Undo. Windows/Linux have the typed backend contracts; native toolbox UI parity is still pending.

Adlon and managed media-library originals are protected before any output is created, including alias and missing destination checks. Modification and interoperability tests use internal temporary directories only. No new application dependency or model weight was introduced: ImageIO/CoreGraphics are system frameworks, and Rust uses the already locked image-rs and SHA-256 crates.
