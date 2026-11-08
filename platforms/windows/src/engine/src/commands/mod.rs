//! IPC command handlers, split by domain. Each submodule owns a coherent
//! group of `handle_*` functions; the dispatcher in `server::handle_line`
//! pattern-matches the inbound command and calls the relevant handler here.

pub(crate) mod bulk;
pub(crate) mod deep_analyze;
pub(crate) mod deep_scope;
pub(crate) mod embed;
pub(crate) mod face_clustering;
pub(crate) mod face_review;
pub(crate) mod hardware;
pub(crate) mod prewarm;
pub(crate) mod restructure;
pub(crate) mod scan;
pub(crate) mod thumbnail;
pub(crate) mod trash;
pub(crate) mod trash_log;
pub(crate) mod wipe;

pub mod catalog;
pub(crate) mod catalog_search;
pub(crate) mod catalog_retrieval;
pub(crate) mod catalog_names;

pub mod catalog_takes;
pub mod catalog_take_auto;
pub(crate) mod take_goal;
mod catalog_take_suggestions;
pub(crate) mod take_grouping;

pub mod chat;
mod chat_search;
pub(crate) mod chat_intent;
pub(crate) mod chat_facts;
mod chat_actions;
mod chat_selection;
pub mod tools;
pub mod models;
pub mod roots;
pub mod benchmark;
pub mod timeline;
pub(crate) mod video_export;
mod media_recipes;

/// Whether FileID's vetted FFmpeg runtime is installed (macOS video frames need it).
#[cfg(all(not(windows), not(target_os = "linux")))]
pub(crate) fn ffmpeg_runtime_available() -> bool {
    video_export::available()
}
