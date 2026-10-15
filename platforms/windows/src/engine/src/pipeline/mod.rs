// Scan pipeline modules.
//
//   discovery → bounded mpsc → tagging workers → bounded mpsc → dbwriter
//
// All channels backpressured; workers paced by the slowest stage downstream.
// ScanCoordinator's AtomicBool sync mirrors checked between batches for
// cancellation that lands within milliseconds without an actor hop per file.

pub mod discovery;
pub mod tagging;
pub mod batch_clip;
pub mod dbwriter;
pub mod face_clustering;
pub mod identity_clustering;
pub mod deep_analyze;
pub(crate) mod deep_analyze_naming;
pub mod restructure;
pub mod restructure_apply;
pub mod restructure_feedback;
pub mod restructure_respect;
pub mod restructure_semantic;
pub mod audio_meta;
pub mod obj_render;
pub mod doc_extract;
pub(crate) mod timeline_analysis;
