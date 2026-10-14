// Native GTK tabs share the typed engine connection.

pub mod cleanup;
pub mod deep_analyze;
pub mod library;
pub mod people;
pub mod restructure;
pub mod settings;
mod model_catalog;
mod util;

mod tools;
mod catalog;
mod workbench_state;
pub mod workbenches;

#[cfg(test)]
mod native_workbench_tests;
