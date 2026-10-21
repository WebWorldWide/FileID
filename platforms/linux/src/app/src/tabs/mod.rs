// Native GTK tabs share the typed engine connection.

pub mod chat;
pub mod cleanup;
pub mod deep_analyze;
pub mod file_tools;
pub mod library;
mod library_pane;
mod model_catalog;
pub mod moments;
mod moments_timeline;
pub mod people;
pub mod restructure;
mod restructure_map;
mod restructure_map_layout;
mod restructure_model;
pub mod settings;
pub(crate) mod util;

mod workbench_state;
pub mod workbenches;

#[cfg(test)]
mod native_workbench_tests;
