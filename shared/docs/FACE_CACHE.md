# Face cache provenance and refresh

The v22 migration preserves existing People, face rows, corrections and legacy embeddings. It adds nullable embedding_model, processing_version and source_revision columns; legacy vectors remain unverified rather than acquiring a guessed current-model label.

Native SFace and portable SFace fingerprint the selected ONNX weight file at load. Native processing uses macos-landmark-overlap-v3-orientation or the explicit bbox fallback version; portable scan results use rust-align112-v1. Native refresh selects missing, malformed or outdated caches in successive batches of at most 5,000 faces, retaining the existing skip/cancellation guards and four-file extraction limit. It checks source size, modification time and file identity before/after decoding/inference and verifies the database snapshot and bbox before saving. It preserves person assignments and ignored faces. JPEG encoding occurs off the database writer; only committed results publish atomic internal cache replacements.

Canonical SQL produces catalog observations and model-separated 128-d vectors, with confidence zero because face quality is not calibrated identity confidence. Person changes synchronize observation membership. Ignoring a face marks generated evidence stale; unignoring cannot revive evidence whose source revision has changed. Changing a bbox invalidates its old vector. Removing a face deletes generated observations/vectors but retains stale manually edited observations. Processing refresh does not overwrite manual markers.

New portable bboxes carry explicit pixel coordinate space and decoded source dimensions, so native thumbnail decoding cannot accidentally normalize coordinates against a smaller image. Legacy dimensionless pixel boxes retain the historical fallback; repair requires source-aware reanalysis rather than guessed scaling.

Tests cover source identity changes, malformed/nonfinite/zero vectors, database/source/bbox/exclusion races, confirmed-name preservation, model-separated catalog records, assignment updates, ignored/stale evidence and manual-marker deletion survival. The actual Swift → Rust → Swift catalog round trip also retains face vectors/provenance/identities/manual markers. These are synthetic correctness tests, not recognition accuracy measurements.

Clustering now rejects unknown/mixed model and processing spaces, stale catalog revisions, invalid non-unit 128-d vectors and oversized batches before persistence. Native uses the selected runtime space and rereads the input under the persist lock; Rust rereads its vector snapshot and retains People on empty input. Auto-merge excludes ignored faces and rechecks its space. Unversioned stored person centroids cannot participate in inheritance; face-ID matching remains. This conservative whole-pass guard can defer clustering until bounded cache refresh finishes; it does not replace durable incremental assignment, stable identity IDs, offline/failure coverage or held-out calibration.

## Remaining acceptance work

Namespace-aware incremental assignment remains unimplemented; whole-pass compatibility is guarded. Native refresh now advances through successive bounded batches and records failures with durable source/model/bbox-scoped retry eligibility. Portable refresh execution, user-facing coverage, explicit retry controls, video face tracking, trusted exemplars, held-out threshold calibration, correction UI parity, and general worker inference isolation remain required. Same-size/same-mtime content replacement detection is a separate gate. The legacy mobileclip_s2 label must not be relabeled as verified ViT-B/32 evidence.

Do not erase identities or silently full-recluster to upgrade the cache. Implement incremental namespace isolation and durable refresh with correction preservation before claiming existing-library accuracy improvement.

## Native refresh retry state (v24)

The additive `face_refresh_failures` table stores the latest failed input snapshot, attempt count, coarse reason, and next eligible time. It does not exclude a face, change People, or claim a recognition confidence. Batches advance by face ID during one refresh; a later invocation resumes from stale caches and persisted retry eligibility. Cancellation preserves committed work and leaves unfinished inputs eligible. Changing a source's path/size/mtime, bounding box, or model/processing namespace invalidates the old delay. Successful refresh clears its record. Same-size/same-mtime replacement detection remains a separate limitation.

Existing engines that refuse unknown migration IDs need their matching v24 build. Rollback should retain the database and restore compatible code; no downgrade, graph reset, or destructive contraction is performed. The Rust migration preserves this state for database interchange, but the portable refresh worker and progress/coverage interface remain pending.

## Imported box orientation

Native refresh carries the source EXIF orientation alongside ImageIO's transformed pixels. Explicit raw-pixel boxes are transformed into the same upright coordinate system before landmark matching or fallback cropping; native normalized boxes already describe upright pixels and pass through unchanged. Processing version v3 makes older native caches eligible for safe refresh. Rotated legacy pixel boxes without explicit source dimensions are deferred instead of guessing their scale. Tests encode and decode actual JPEGs under all eight EXIF orientations and verify the selected crop region. Portable upright decoding/rebuild and held-out face accuracy remain pending.
