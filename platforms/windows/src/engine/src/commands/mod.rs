pub(crate) mod bulk;
pub(crate) mod deep_analyze;
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
pub(crate) mod deep_analyze_mlx;
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
pub(crate) mod mlx_models;
pub(crate) mod deep_scope;
pub(crate) mod embed;
pub(crate) mod face_clustering;
pub(crate) mod face_clustering_mac;
pub mod face_extract_mac;
pub(crate) mod face_queues;
pub(crate) mod face_refresh;
pub(crate) mod face_review;
pub(crate) mod folder_access;
pub(crate) mod person_merge;
pub(crate) mod hardware;
pub(crate) mod prewarm;
pub(crate) mod restructure;
pub(crate) mod scan;
pub(crate) mod thumbnail;
pub(crate) mod trash;
pub(crate) mod trash_log;
pub(crate) mod journal_import;
pub(crate) mod path_moves;
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
pub(crate) mod timeout_retry;

pub mod chat;
mod chat_search;
pub(crate) mod chat_commands;
pub(crate) mod chat_intent;
pub(crate) mod chat_facts;
mod chat_actions;
mod chat_selection;
pub mod tools;
pub mod models;
pub mod roots;
pub mod benchmark;
pub mod timeline;
mod timeline_media;
#[cfg_attr(all(target_os = "macos", feature = "apple-adapters"), allow(dead_code))]
pub(crate) mod video_export;
mod media_recipes;

#[cfg(all(not(windows), not(target_os = "linux")))]
pub(crate) fn ffmpeg_runtime_available() -> bool {
    video_export::available()
}
