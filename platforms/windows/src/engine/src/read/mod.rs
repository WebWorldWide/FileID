//! Read-only library queries shared by the engine, CLI, TUI and GTK app.
//!
//! Declared under `db` (via `#[path]`) so the engine binary compiles it too;
//! `lib.rs` re-exports it as `fileid_engine::read`.

pub mod dupes;
pub mod people;
pub mod restructure;
pub mod text;
