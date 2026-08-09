//! `fileid-engine` library surface — re-declares the same submodules as
//! `main.rs` so external test/bench harnesses can depend on the engine's
//! internals without going through stdin/stdout.
//!
//! Each module is compiled twice (once for the bin, once for the lib);
//! ~30 % dev-compile-time cost, zero runtime cost (the bin still links
//! its own LTO copy). Standard Cargo workaround for bin-only crates
//! wanting bench/test scaffolding.

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
