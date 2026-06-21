// Tab modules — each a 1:1 port of the corresponding macOS reference view. All
// six tabs are now implemented and share the single `Rc<RefCell<EngineClient>>`
// built in `window.rs`: Library · People · Cleanup · Deep Analyze · Restructure
// · Settings.

pub mod cleanup;
pub mod deep_analyze;
pub mod library;
pub mod people;
pub mod restructure;
pub mod settings;
