// Scan pipeline modules. Mirror of macOS engine/Sources/FileIDEngine/Pipeline/.
//
//   discovery → bounded mpsc → tagging workers → bounded mpsc → dbwriter
//
// All channels backpressured; workers paced by the slowest stage downstream.
// ScanCoordinator's AtomicBool sync mirrors checked between batches for
// cancellation that lands within milliseconds without an actor hop per file.

pub mod discovery;
pub mod tagging;
pub mod dbwriter;
pub mod face_clustering;
pub mod deep_analyze;
pub mod restructure;
pub mod restructure_apply;

pub use discovery::{DiscoveredFile, Discovery, FileKind};
pub use tagging::{TaggedFile, Tagger};
pub use dbwriter::DbWriter;
