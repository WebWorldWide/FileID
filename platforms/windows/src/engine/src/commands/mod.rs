//! IPC command handlers, split by domain. Each submodule owns a coherent
//! group of `handle_*` functions; the dispatcher in `main::handle_line`
//! pattern-matches the inbound command and calls the relevant handler here.

pub(crate) mod bulk;
pub(crate) mod deep_analyze;
pub(crate) mod embed;
pub(crate) mod face_clustering;
pub(crate) mod hardware;
pub(crate) mod prewarm;
pub(crate) mod restructure;
pub(crate) mod scan;
pub(crate) mod thumbnail;
pub(crate) mod trash;
pub(crate) mod trash_log;
pub(crate) mod wipe;
