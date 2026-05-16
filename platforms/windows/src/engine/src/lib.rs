//! `fileid-engine` library surface — re-declares the same submodules as
//! `main.rs` so external test/bench harnesses can depend on the engine's
//! internals without going through stdin/stdout.
//!
//! V15.3 N3: this lives alongside `main.rs` (both targets, same crate)
//! so `benches/*.rs` and integration tests can `use fileid_engine::*`.
//! Each module is compiled twice (once for the bin, once for the lib);
//! the dev-compile-time cost is ~30% versus a single bin target, and
//! the runtime cost is zero — the bin still links its own copy with
//! release LTO. This is the standard Cargo workaround for bin-only
//! crates wanting bench/test scaffolding without a wholesale refactor
//! of `main.rs`'s 600+ LOC of setup code.

#![allow(clippy::needless_return)]
#![allow(dead_code)]

pub mod commands;
pub mod coordinator;
pub mod db;
pub mod downloader;
pub mod ipc;
pub mod job_queue;
pub mod logging;
pub mod models;
pub mod paths;
pub mod pipeline;
pub mod platform;
pub mod scan_session;
pub mod shell;
pub mod util;
