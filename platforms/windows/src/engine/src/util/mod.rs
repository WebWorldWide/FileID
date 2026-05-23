//! Small cross-cutting helpers used by multiple command handlers.
//!
//! Each submodule is leaf-level (no engine state, no cross-deps) so handlers
//! can pull them in without dragging in the wider command graph.

pub(crate) mod content_hash;
pub(crate) mod elevation;
pub(crate) mod hmac;
pub(crate) mod hnsw_index;
pub(crate) mod keywords;
pub(crate) mod path_safety;
pub(crate) mod zip;
