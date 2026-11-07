// "All locations" or one library location (macOS `LibraryScope`). Library,
// Search & Moments and Chat keep only the chosen location's files, by the
// engine's own `rootID` rule: the location's path and below, never a sibling
// sharing its prefix, never a deeper location (the most specific owns a file).

use std::cell::RefCell;
use std::rc::Rc;

use fileid_engine::commands::roots::RootScope;
use fileid_engine::ipc::LibraryRoot;

#[derive(Clone, Debug, PartialEq)]
pub struct LibraryScope {
    pub root: LibraryRoot,
    rule: RootScope,
}

impl LibraryScope {
    pub fn new(root_id: Option<i64>, roots: &[LibraryRoot]) -> Option<Self> {
        let id = root_id?;
        let root = roots.iter().find(|root| root.id == id)?.clone();
        let others: Vec<String> = roots.iter().filter(|other| other.id != id).map(|other| other.path.clone()).collect();
        let rule = RootScope::new(&root.path, &others);
        Some(Self { root, rule })
    }

    #[cfg(test)]
    pub fn contains(&self, path: &str) -> bool {
        self.rule.contains(path)
    }

    pub fn param(&self) -> &str {
        self.rule.param()
    }
}

/// The stored choice once the location list is known: kept while the list is
/// still empty (not read yet), dropped when its location is gone.
pub fn reconciled(stored: Option<i64>, roots: &[LibraryRoot]) -> Option<i64> {
    let id = stored?;
    if roots.is_empty() || roots.iter().any(|root| root.id == id) {
        Some(id)
    } else {
        None
    }
}

/// Title bar label: the location's name, "All locations", or the folder being
/// worked on before anything is cataloged.
pub fn title(scope: Option<&LibraryScope>, roots: &[LibraryRoot], folder: Option<&str>) -> String {
    match scope {
        Some(scope) => scope.root.display.clone(),
        None if roots.is_empty() => folder.unwrap_or("Choose a folder").to_owned(),
        None => "All locations".to_owned(),
    }
}

pub fn file_count_text(count: i64) -> String {
    if count == 1 {
        "1 file".to_owned()
    } else {
        format!("{} files", crate::shell::grouped(count.max(0) as u64))
    }
}

pub fn location_count_text(count: usize) -> String {
    if count == 1 {
        "1 location".to_owned()
    } else {
        format!("{count} locations")
    }
}

/// Spoken name of one location row in the switcher.
pub fn spoken(root: &LibraryRoot) -> String {
    let offline = if root.online { "" } else { ", offline" };
    format!("{}, {}{offline}", root.display, file_count_text(root.file_count))
}

#[derive(Default)]
struct State {
    roots: Vec<LibraryRoot>,
    current: Option<Rc<LibraryScope>>,
    listeners: Vec<Rc<dyn Fn()>>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State::default());
}

pub fn current() -> Option<Rc<LibraryScope>> {
    STATE.with(|state| state.borrow().current.clone())
}

pub fn current_id() -> Option<i64> {
    current().map(|scope| scope.root.id)
}

pub fn roots() -> Vec<LibraryRoot> {
    STATE.with(|state| state.borrow().roots.clone())
}

/// Runs `listener` on the main loop whenever the location list or the choice changes.
pub fn subscribe(listener: Rc<dyn Fn()>) {
    STATE.with(|state| state.borrow_mut().listeners.push(listener));
}

fn notify() {
    let listeners = STATE.with(|state| state.borrow().listeners.clone());
    for listener in listeners {
        listener();
    }
}

fn apply() {
    let changed = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let next = LibraryScope::new(crate::app_settings::library_scope_root_id(), &state.roots).map(Rc::new);
        let changed = next.as_deref() != state.current.as_deref();
        state.current = next;
        changed
    });
    if changed {
        notify();
    }
}

/// Picks a location, or "All locations" for `None`, and remembers it.
pub fn choose(root_id: Option<i64>) {
    crate::app_settings::remember_library_scope_root_id(root_id);
    apply();
}

/// Takes a fresh location list.
pub fn apply_roots(roots: Vec<LibraryRoot>) {
    let stored = crate::app_settings::library_scope_root_id();
    let kept = reconciled(stored, &roots);
    if kept != stored {
        crate::app_settings::remember_library_scope_root_id(kept);
    }
    let listed = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let same = state.roots == roots;
        state.roots = roots;
        !same
    });
    apply();
    if listed {
        notify();
    }
}

/// Re-reads the locations (the same read-only listing Settings ▸ Locations uses).
pub fn refresh() {
    let rx = crate::engine_client::read_db(|conn| fileid_engine::commands::roots::list(conn).unwrap_or_default());
    gtk::glib::MainContext::default().spawn_local(async move {
        if let Ok(roots) = rx.recv().await {
            apply_roots(roots);
        }
    });
}

/// The chosen location's read filter for `QuerySpec::scope`; `None` under "All locations".
pub fn current_param() -> Option<String> {
    current().map(|scope| scope.param().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(id: i64, path: &str, count: i64, online: bool) -> LibraryRoot {
        LibraryRoot {
            id,
            path: path.into(),
            display: path.trim_end_matches('/').rsplit('/').next().unwrap_or(path).into(),
            kind: "external".into(),
            enabled: true,
            online,
            file_count: count,
            total_bytes: 1,
            last_scan_at: None,
            last_status: None,
        }
    }

    fn roots() -> Vec<LibraryRoot> {
        vec![
            root(1, "/m/Camera/", 2, true),
            root(2, "/m/Camera2", 1, true),
            root(3, "/m/Camera/Synced", 1, true),
            root(4, "/elsewhere", 1, true),
        ]
    }

    #[test]
    fn a_scope_keeps_its_own_files_not_lookalikes_or_deeper_locations() {
        let scope = LibraryScope::new(Some(1), &roots()).unwrap();
        for inside in ["/m/Camera", "/m/Camera/a.jpg", "/m/Camera/2020/b.jpg", "/m/Camera/Synced2/c.jpg"] {
            assert!(scope.contains(inside), "{inside}");
        }
        for outside in ["/m/Camera2/a.jpg", "/m/Cameras/a.jpg", "/m/Camera/Synced/d.jpg", "/elsewhere/e.jpg"] {
            assert!(!scope.contains(outside), "{outside}");
        }
        assert!(LibraryScope::new(Some(3), &roots()).unwrap().contains("/m/Camera/Synced/d.jpg"));
        assert!(LibraryScope::new(Some(9), &roots()).is_none());
        assert!(LibraryScope::new(None, &roots()).is_none());
    }

    #[test]
    fn the_choice_survives_loading_and_falls_back_when_its_location_is_gone() {
        assert_eq!(reconciled(Some(2), &[]), Some(2));
        assert_eq!(reconciled(Some(2), &roots()), Some(2));
        assert_eq!(reconciled(Some(9), &roots()), None);
        assert_eq!(reconciled(None, &roots()), None);
    }

    #[test]
    fn the_title_bar_names_the_location() {
        let all = roots();
        assert_eq!(title(LibraryScope::new(Some(2), &all).as_ref(), &all, Some("Pictures")), "Camera2");
        assert_eq!(title(None, &all, Some("Pictures")), "All locations");
        assert_eq!(title(None, &[], Some("Pictures")), "Pictures");
        assert_eq!(title(None, &[], None), "Choose a folder");
        assert_eq!(spoken(&root(5, "/media/Card", 1204, false)), "Card, 1,204 files, offline");
        assert_eq!(spoken(&root(6, "/home/a/One", 1, true)), "One, 1 file");
        assert_eq!(location_count_text(1), "1 location");
    }
}
