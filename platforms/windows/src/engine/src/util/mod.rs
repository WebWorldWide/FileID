//! Small cross-cutting helpers used by multiple command handlers.
//!
//! Each submodule is leaf-level (no engine state, no cross-deps) so handlers
//! can pull them in without dragging in the wider command graph.

pub(crate) mod hmac;
pub(crate) mod path_safety;
pub(crate) mod zip;
