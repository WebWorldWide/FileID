// Tab modules. Library is implemented (mirror of macOS `LibraryView`); the
// other five tabs are placeholder `adw::StatusPage`s pending implementation —
// People, Cleanup, Deep Analyze, Restructure, Settings.

pub mod library;

use adw::prelude::*;

/// A "Coming soon" placeholder page for a not-yet-ported tab. Wrapped in the
/// transparent `.fileid-tab` host so the LavaLamp + scrim read through, keeping
/// the placeholders visually consistent with the live Library tab.
pub fn placeholder(icon_name: &str, title: &str, description: &str) -> gtk::Widget {
    let host = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["fileid-tab"])
        .hexpand(true)
        .vexpand(true)
        .build();

    let status = adw::StatusPage::builder()
        .icon_name(icon_name)
        .title(title)
        .description(description)
        .vexpand(true)
        .build();

    host.append(&status);
    host.upcast()
}
