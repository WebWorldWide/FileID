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
    ToggleChat,
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

    pub fn tooltip(self) -> String {
        let titles = self.pages().map(|page| page.title).collect::<Vec<_>>();
        format!(
            "{}: {} ({})",
            self.name(),
            titles.join(", "),
            self.shortcut_hint()
        )
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
    pub summary: &'static str,
    pub icon: &'static str,
    pub area: Option<Area>,
    /// Ctrl+N, numbered like macOS ⌘1–9 (6 is Best Takes, not on Linux yet).
    pub shortcut: Option<u32>,
}

pub const PAGES: [Page; 9] = [
    Page {
        id: "library",
        title: "Library",
        summary: "Every scanned file, with search and preview",
        icon: "view-grid-symbolic",
        area: Some(Area::Browse),
        shortcut: Some(1),
    },
    Page {
        id: "people",
        title: "People",
        summary: "Faces grouped by person; name them here",
        icon: "system-users-symbolic",
        area: Some(Area::Browse),
        shortcut: Some(2),
    },
    Page {
        id: "moments",
        title: "Search & Moments",
        summary: "Search inside videos and documents",
        icon: "system-search-symbolic",
        area: Some(Area::Browse),
        shortcut: Some(3),
    },
    Page {
        id: "deep-analyze",
        title: "Describe Files",
        summary: "Write a description and a better file name for each file",
        icon: "starred-symbolic",
        area: Some(Area::Analyze),
        shortcut: Some(4),
    },
    Page {
        id: "chat",
        title: "Chat",
        summary: "Ask questions about your files",
        icon: "chat-symbolic",
        area: Some(Area::Analyze),
        shortcut: Some(5),
    },
    Page {
        id: "cleanup",
        title: "Duplicates",
        summary: "Find extra copies and move them to Trash",
        icon: "user-trash-symbolic",
        area: Some(Area::Act),
        shortcut: Some(7),
    },
    Page {
        id: "restructure",
        title: "Organize Folders",
        summary: "Suggest a tidier folder layout, then move files",
        icon: "view-list-symbolic",
        area: Some(Area::Act),
        shortcut: Some(8),
    },
    Page {
        id: "tools",
        title: "File Tools",
        summary: "Convert, resize or reframe photos and videos",
        icon: "applications-utilities-symbolic",
        area: Some(Area::Act),
        shortcut: Some(9),
    },
    Page {
        id: "settings",
        title: "Settings",
        summary: "Appearance, AI models and storage",
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
            Some(n) => format!("{} (Ctrl+{n})", self.summary),
            None if self.id == "settings" => format!("{} (Ctrl+,)", self.summary),
            None => self.summary.to_owned(),
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

/// Pages whose title, id or summary matches `query`, in sidebar order (all when empty).
pub fn matching_pages(query: &str) -> Vec<&'static Page> {
    let q = query.trim().to_lowercase();
    PAGES
        .iter()
        .filter(|page| {
            q.is_empty()
                || page.title.to_lowercase().contains(&q)
                || page.id.to_lowercase().contains(&q)
                || page.summary.to_lowercase().contains(&q)
        })
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
        assert_eq!(
            page("settings").unwrap().tooltip(),
            "Appearance, AI models and storage (Ctrl+,)"
        );
        assert_eq!(page("deep-analyze").unwrap().crumb(), "Analyze / Describe Files");
        assert_eq!(
            page("cleanup").unwrap().tooltip(),
            "Find extra copies and move them to Trash (Ctrl+7)"
        );
        assert_eq!(
            Area::Act.tooltip(),
            "Act: Duplicates, Organize Folders, File Tools (Ctrl+7–9)"
        );
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
    fn page_search_matches_titles_ids_and_summaries() {
        assert_eq!(matching_pages("").len(), PAGES.len());
        let ids: Vec<_> = matching_pages("organize").iter().map(|page| page.id).collect();
        assert_eq!(ids, ["restructure"]);
        assert_eq!(matching_pages("CHAT")[0].id, "chat");
        assert_eq!(matching_pages("restructure")[0].id, "restructure");
        assert_eq!(matching_pages("trash")[0].id, "cleanup");
        assert_eq!(matching_pages("duplicates")[0].id, "cleanup");
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
