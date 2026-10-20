//! `fileid-engine` library: the engine internals shared by the `FileIDEngine`
//! binary, the GTK app, the CLI, the TUI, tests and benches.

#![allow(clippy::needless_return)]

pub mod client;
pub mod commands;
pub mod catalog_index;
pub mod coordinator;
pub mod db;
pub mod face_bbox;
pub mod downloader;
pub mod ipc;
pub mod job_queue;
pub mod logging;
pub mod models;
pub mod ort_runtime;
pub mod paths;
pub mod pipeline;
pub mod platform;
pub mod scan_session;
pub mod server;
pub mod resource_scheduler;
pub mod shell;
pub mod util;

pub use db::read;
