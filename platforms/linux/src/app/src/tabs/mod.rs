// Native GTK tabs share the typed engine connection.

pub mod chat;
mod chat_actions;
mod chat_cards;
mod chat_handoff;
mod chat_selection;
pub mod cleanup;
pub mod deep_analyze;
mod face_queues;
mod face_review;
pub mod file_tools;
pub mod library;
mod library_bulk;
mod library_bulk_model;
mod library_pane;
pub mod library_scope;
mod library_selection;
mod locations;
mod merge_ledger;
mod model_catalog;
pub mod moments;
mod moments_dates;
mod moments_timeline;
mod naming;
mod offline_roots;
pub mod people;
mod people_actions;
mod reload_gate;
pub mod restructure;
mod restructure_map;
mod restructure_map_layout;
mod restructure_model;
pub mod settings;
mod settings_panels;
pub mod takes;
mod takes_groups;
mod takes_groups_model;
mod takes_model;
mod takes_proposals_model;
mod takes_widgets;
pub(crate) mod util;
mod vlm_picker;

mod workbench_state;
pub mod workbenches;

#[cfg(test)]
mod native_workbench_tests;
