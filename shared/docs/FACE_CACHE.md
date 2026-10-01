# Face cache provenance and refresh

The v22 migration preserves existing People, face rows, corrections and legacy embeddings. It adds nullable embedding_model, processing_version and source_revision columns; legacy vectors remain unverified rather than acquiring a guessed current-model label.

Native SFace and portable SFace fingerprint the selected ONNX weight file at load. Native processing uses macos-landmark-overlap-v2 or the explicit bbox fallback version; portable scan results use rust-align112-v1. Native refresh selects missing, malformed or outdated caches in batches of at most 5,000 faces, retaining the existing skip/attempt/cancellation rules and four-file extraction limit. It checks source size, modification time and file identity before/after decoding/inference and verifies the database snapshot and bbox before saving. It preserves person assignments and ignored faces. JPEG encoding occurs off the database writer; only committed results publish atomic internal cache replacements.

Canonical SQL produces catalog observations and model-separated 128-d vectors, with confidence zero because face quality is not calibrated identity confidence. Person changes synchronize observation membership. Ignoring a face marks generated evidence stale; unignoring cannot revive evidence whose source revision has changed. Changing a bbox invalidates its old vector. Removing a face deletes generated observations/vectors but retains stale manually edited observations. Processing refresh does not overwrite manual markers.

New portable bboxes carry explicit pixel coordinate space and decoded source dimensions, so native thumbnail decoding cannot accidentally normalize coordinates against a smaller image. Legacy dimensionless pixel boxes retain the historical fallback; repair requires source-aware reanalysis rather than guessed scaling.

Tests cover source identity changes, malformed/nonfinite/zero vectors, database/source/bbox/exclusion races, confirmed-name preservation, model-separated catalog records, assignment updates, ignored/stale evidence and manual-marker deletion survival. The actual Swift → Rust → Swift catalog round trip also retains face vectors/provenance/identities/manual markers. These are synthetic correctness tests, not recognition accuracy measurements.

## Remaining acceptance work

Legacy clustering still reads arcface_embedding without complete model/processing partitioning. Native refresh batches are bounded but not yet a durable complete-library rebuild; failed/offline/unsupported inputs need explicit coverage and resumable backlog reporting. Existing video face analysis is not tracking. Trusted exemplars, incremental assignment, held-out threshold calibration, accuracy/coverage measurements, correction UI parity and general worker inference isolation remain required. Same-size/same-mtime content replacement detection is a separate catalog revision gate. CLIP's legacy mobileclip_s2 label is ambiguous and must not be relabeled as verified ViT-B/32 evidence.

Do not erase identities or silently full-recluster to upgrade the cache. Implement complete space isolation and durable refresh with correction preservation before claiming existing-library accuracy improvement.
