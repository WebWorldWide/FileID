// Native GTK tabs share the typed engine connection.

pub mod cleanup;
pub mod deep_analyze;
pub mod library;
mod library_pane;
mod model_catalog;
pub mod people;
pub mod restructure;
pub mod settings;
pub(crate) mod util;

mod workbench_state;
pub mod workbenches;

#[cfg(test)]
mod native_workbench_tests;
