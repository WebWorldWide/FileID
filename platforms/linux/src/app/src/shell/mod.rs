// Prototype v3 shell (shared/docs/DESIGN_V3.md): title bar, area sidebar with
// the scan card, and page headers. Mirrors macOS `Views/Shell/*`,
// `Views/Sidebar.swift` and the header in `Views/Detail.swift`.

pub mod page_header;
pub mod sidebar;
pub mod status;
pub mod title_bar;

use gtk::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Navigate(&'static str),
    SelectArea(Area),
    ToggleSidebar,
    PickFolder,
    StartScan,
    PauseScan,
    ResumeScan,
    CancelScan,
    CancelDeep,
    ClearFolder,
    WipeAndRescan,
    AskChat(String),
    LibraryQuery(String),
    FocusSearch,
    DismissNote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Area {
    Browse,
    Analyze,
    Act,
}

impl Area {
    pub const ALL: [Area; 3] = [Area::Browse, Area::Analyze, Area::Act];

    pub fn name(self) -> &'static str {
        match self {
            Area::Browse => "Browse",
            Area::Analyze => "Analyze",
            Area::Act => "Act",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Area::Browse => 0,
            Area::Analyze => 1,
            Area::Act => 2,
        }
    }

    pub fn pages(self) -> impl Iterator<Item = &'static Page> {
        PAGES.iter().filter(move |page| page.area == Some(self))
    }

    pub fn shortcut_hint(self) -> &'static str {
        match self {
            Area::Browse => "Ctrl+1–3",
            Area::Analyze => "Ctrl+4–5",
            Area::Act => "Ctrl+7–9",
        }
    }

    /// Switching areas returns to the page last used in that area.
    pub fn remembered(self, current: &str, stored: Option<&str>) -> &'static Page {
        self.pages()
            .find(|page| page.id == current)
            .or_else(|| stored.and_then(|id| self.pages().find(|page| page.id == id)))
            .or_else(|| self.pages().next())
            .expect("every area has pages")
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Page {
    pub id: &'static str,
    pub title: &'static str,
    pub icon: &'static str,
    pub area: Option<Area>,
    /// Ctrl+N, numbered like macOS ⌘1–9 (6 is Best Takes, not on Linux yet).
    pub shortcut: Option<u32>,
}

pub const PAGES: [Page; 9] = [
    Page {
        id: "library",
        title: "Library",
        icon: "view-grid-symbolic",
        area: Some(Area::Browse),
        shortcut: Some(1),
    },
    Page {
        id: "people",
        title: "People",
        icon: "system-users-symbolic",
        area: Some(Area::Browse),
        shortcut: Some(2),
    },
    Page {
        id: "moments",
        title: "Search & Moments",
        icon: "system-search-symbolic",
        area: Some(Area::Browse),
        shortcut: Some(3),
    },
    Page {
        id: "deep-analyze",
        title: "Deep Analyze",
        icon: "starred-symbolic",
        area: Some(Area::Analyze),
        shortcut: Some(4),
    },
    Page {
        id: "chat",
        title: "Chat",
        icon: "chat-symbolic",
        area: Some(Area::Analyze),
        shortcut: Some(5),
    },
    Page {
        id: "cleanup",
        title: "Cleanup",
        icon: "user-trash-symbolic",
        area: Some(Area::Act),
        shortcut: Some(7),
    },
    Page {
        id: "restructure",
        title: "Restructure",
        icon: "view-list-symbolic",
        area: Some(Area::Act),
        shortcut: Some(8),
    },
    Page {
        id: "tools",
        title: "File Tools",
        icon: "applications-utilities-symbolic",
        area: Some(Area::Act),
        shortcut: Some(9),
    },
    Page {
        id: "settings",
        title: "Settings",
        icon: "emblem-system-symbolic",
        area: None,
        shortcut: None,
    },
];

pub fn page(id: &str) -> Option<&'static Page> {
    PAGES.iter().find(|page| page.id == id)
}

pub fn page_for_shortcut(n: u32) -> Option<&'static Page> {
    PAGES.iter().find(|page| page.shortcut == Some(n))
}

impl Page {
    pub fn crumb(&self) -> String {
        format!(
            "{} / {}",
            self.area.map_or("FileID", Area::name),
            self.title
        )
    }

    pub fn tooltip(&self) -> String {
        match self.shortcut {
            Some(n) => format!("{} (Ctrl+{n})", self.title),
            None if self.id == "settings" => format!("{} (Ctrl+,)", self.title),
            None => self.title.to_owned(),
        }
    }

    pub fn search_hint(&self) -> String {
        let area = self.area.map_or("FileID", Area::name);
        match self.shortcut {
            Some(n) => format!("{area} · Ctrl+{n}"),
            None => area.to_owned(),
        }
    }
}

/// Pages whose name matches `query`, in sidebar order (all ten when empty).
pub fn matching_pages(query: &str) -> Vec<&'static Page> {
    let q = query.trim().to_lowercase();
    PAGES
        .iter()
        .filter(|page| q.is_empty() || page.title.to_lowercase().contains(&q))
        .take(if q.is_empty() { PAGES.len() } else { 4 })
        .collect()
}

pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

pub fn compact(n: u64) -> String {
    if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1000.0)
    } else {
        grouped(n)
    }
}

pub fn section_label(text: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text.to_uppercase())
        .xalign(0.0)
        .css_classes(["section-label"])
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build()
}

pub fn label(text: &str, classes: &[&str]) -> gtk::Label {
    let label = gtk::Label::builder().label(text).xalign(0.0).build();
    for class in classes {
        label.add_css_class(class);
    }
    label
}

pub fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

pub fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

pub fn spacer() -> gtk::Box {
    let spacer = hbox(0);
    spacer.set_hexpand(true);
    spacer
}

pub fn set_tone(widget: &impl IsA<gtk::Widget>, tone: status::Tone) {
    for other in status::Tone::ALL {
        widget.remove_css_class(other.css());
    }
    widget.add_css_class(tone.css());
}

/// A capsule progress bar whose fraction moves on the brand spring.
#[derive(Clone)]
pub struct GrainBar {
    pub bar: gtk::ProgressBar,
    target: std::rc::Rc<std::cell::Cell<f64>>,
}

impl GrainBar {
    pub fn new(mini: bool) -> Self {
        let bar = gtk::ProgressBar::builder()
            .css_classes(["grain-bar"])
            .valign(gtk::Align::Center)
            .build();
        if mini {
            bar.add_css_class("mini");
        }
        Self {
            bar,
            target: std::rc::Rc::new(std::cell::Cell::new(0.0)),
        }
    }

    pub fn set(&self, fraction: f64, tone: status::Tone) {
        set_tone(&self.bar, tone);
        let to = if fraction.is_finite() {
            fraction.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if (to - self.target.get()).abs() < 0.001 {
            return;
        }
        self.target.set(to);
        let from = self.bar.fraction();
        let bar = self.bar.clone();
        let _ = crate::spring::animate(&self.bar, from, to, move |value| {
            bar.set_fraction(value.clamp(0.0, 1.0))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn areas_hold_the_v3_pages_in_order() {
        let ids = |area: Area| area.pages().map(|page| page.id).collect::<Vec<_>>();
        assert_eq!(ids(Area::Browse), ["library", "people", "moments"]);
        assert_eq!(ids(Area::Analyze), ["deep-analyze", "chat"]);
        assert_eq!(ids(Area::Act), ["cleanup", "restructure", "tools"]);
        assert!(page("settings").unwrap().area.is_none());
    }

    #[test]
    fn crumbs_and_shortcuts_follow_the_macos_numbering() {
        assert_eq!(page("people").unwrap().crumb(), "Browse / People");
        assert_eq!(page("settings").unwrap().crumb(), "FileID / Settings");
        assert_eq!(page_for_shortcut(4).unwrap().id, "deep-analyze");
        assert_eq!(page_for_shortcut(9).unwrap().id, "tools");
        assert!(page_for_shortcut(6).is_none());
        assert_eq!(page("cleanup").unwrap().search_hint(), "Act · Ctrl+7");
        assert_eq!(page("settings").unwrap().tooltip(), "Settings (Ctrl+,)");
    }

    #[test]
    fn switching_area_returns_to_its_last_page() {
        assert_eq!(
            Area::Act.remembered("library", Some("restructure")).id,
            "restructure"
        );
        assert_eq!(
            Area::Act.remembered("tools", Some("restructure")).id,
            "tools"
        );
        assert_eq!(
            Area::Analyze.remembered("library", Some("library")).id,
            "deep-analyze"
        );
        assert_eq!(Area::Browse.remembered("cleanup", None).id, "library");
    }

    #[test]
    fn page_search_matches_names_case_insensitively() {
        assert_eq!(matching_pages("").len(), PAGES.len());
        let ids: Vec<_> = matching_pages("re").iter().map(|page| page.id).collect();
        assert_eq!(ids, ["restructure"]);
        assert_eq!(matching_pages("CHAT")[0].id, "chat");
    }

    #[test]
    fn counts_group_and_compact_like_macos() {
        assert_eq!(grouped(0), "0");
        assert_eq!(grouped(999), "999");
        assert_eq!(grouped(1234567), "1,234,567");
        assert_eq!(compact(9_999), "9,999");
        assert_eq!(compact(48_200), "48k");
    }
}
