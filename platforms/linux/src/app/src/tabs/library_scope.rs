// "All locations" or one library location (macOS `LibraryScope`). Library,
// Search & Moments and Chat keep only the chosen location's files, by the
// engine's own `rootID` rule: the location's path and below, never a sibling
// sharing its prefix, never a deeper location (the most specific owns a file).

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::thread;

use async_channel::Receiver;
use fileid_engine::commands::roots::{clause, RootScope};
use fileid_engine::ipc::LibraryRoot;
use fileid_engine::read::text::{escape_like, fts_phrase_query};

use crate::engine_client::{EngineClient, FileRow, QuerySpec, QUERY_MAX_ROWS};

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

/// The Library's page read: the engine client's query under "All locations",
/// the same query kept to the chosen location otherwise, so counts and paging
/// see only its files.
pub fn query_files(engine: &RefCell<EngineClient>, spec: QuerySpec) -> Receiver<anyhow::Result<(Vec<FileRow>, i64)>> {
    let Some(scope) = current() else { return engine.borrow().query_files(spec) };
    let param = scope.param().to_owned();
    let (tx, rx) = async_channel::bounded(1);
    thread::spawn(move || {
        let result = fileid_engine::paths::db_path().and_then(|path| {
            if path.exists() {
                query_scoped(&path, &spec, &param)
            } else {
                Ok((Vec::new(), 0))
            }
        });
        let _ = tx.send_blocking(result);
    });
    rx
}

const QUERY_FILTER: &str = "f.failed = 0 \
    AND (:has_search = 0 \
         OR COALESCE(f.path_search, f.path_text) LIKE :like ESCAPE '\\' \
         OR EXISTS (SELECT 1 FROM tags t WHERE t.file_id = f.id AND t.tag LIKE :like ESCAPE '\\') \
         OR (:has_fts = 1 AND ( \
                f.id IN (SELECT rowid FROM ocr_fts WHERE ocr_fts MATCH :fts) \
                OR f.id IN (SELECT rowid FROM doc_fts WHERE doc_fts MATCH :fts)))) \
    AND (:kind IS NULL OR f.kind = :kind)";

const FILE_COLUMNS: &str = "f.id, f.path_text, f.kind, f.extension, f.size_bytes, f.created_at, \
     f.modified_at, f.has_faces, f.has_text, f.file_ref, f.content_hash, f.vlm_description, f.vlm_proposed_name, \
     f.vlm_inherited_from IS NOT NULL";

fn file_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FileRow> {
    let path: String = row.get(1)?;
    let name = Path::new(&path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    Ok(FileRow {
        id: row.get(0)?,
        path,
        name,
        kind: row.get(2)?,
        extension: row.get(3)?,
        size_bytes: row.get(4)?,
        created_at: row.get(5)?,
        modified_at: row.get(6)?,
        has_faces: row.get(7)?,
        has_text: row.get(8)?,
        file_ref: row.get(9)?,
        content_hash: row.get(10)?,
        description: row.get(11)?,
        proposed_name: row.get(12)?,
        caption_inherited: row.get(13)?,
    })
}

fn query_scoped(path: &Path, spec: &QuerySpec, scope: &str) -> anyhow::Result<(Vec<FileRow>, i64)> {
    let conn = fileid_engine::db::open_read(path)?;
    query_scoped_on(&conn, spec, scope)
}

fn query_scoped_on(conn: &rusqlite::Connection, spec: &QuerySpec, scope: &str) -> anyhow::Result<(Vec<FileRow>, i64)> {
    let search = spec.search.trim();
    let has_search = !search.is_empty();
    let like = format!("%{}%", escape_like(search));
    let fts = fts_phrase_query(search);
    let has_fts = !fts.is_empty();
    let kind = spec.kind.as_deref().filter(|kind| !kind.is_empty());
    let limit = if spec.limit > 0 { spec.limit.min(QUERY_MAX_ROWS) } else { 500 };
    let within = clause("f.path_text", ":scope");
    let mut stmt = conn.prepare(&format!(
        "SELECT {FILE_COLUMNS} FROM files f WHERE {QUERY_FILTER} AND {within} \
         ORDER BY f.scanned_at DESC, f.id DESC LIMIT :limit OFFSET :offset"
    ))?;
    let rows = stmt
        .query_map(
            rusqlite::named_params! {
                ":has_search": has_search,
                ":like": like,
                ":has_fts": has_fts,
                ":fts": fts,
                ":kind": kind,
                ":scope": scope,
                ":limit": limit,
                ":offset": spec.offset.max(0),
            },
            file_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let total = if spec.offset <= 0 && (rows.len() as i64) < limit {
        rows.len() as i64
    } else {
        conn.query_row(
            &format!("SELECT COUNT(*) FROM files f WHERE {QUERY_FILTER} AND {within}"),
            rusqlite::named_params! {
                ":has_search": has_search,
                ":like": like,
                ":has_fts": has_fts,
                ":fts": fts,
                ":kind": kind,
                ":scope": scope,
            },
            |row| row.get(0),
        )?
    };
    Ok((rows, total))
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

    #[test]
    fn the_library_page_read_keeps_to_the_scope_before_its_limit() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        fileid_engine::db::migrations::apply(&conn).unwrap();
        conn.execute_batch(
            "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES
              (1,'/m/Camera/a.jpg',1,1,1,'image','jpg'),(2,'/m/Camera/2020/b.mov',2,1,2,'video','mov'),
              (3,'/m/Camera2/c.jpg',3,1,3,'image','jpg'),(4,'/m/Camera/Synced/d.jpg',4,1,4,'image','jpg'),
              (5,'/m/Cameras/e.jpg',5,1,5,'image','jpg');",
        )
        .unwrap();
        let scope = LibraryScope::new(Some(1), &roots()).unwrap();
        let ids = |spec: QuerySpec| -> (Vec<i64>, i64) {
            let (rows, total) = query_scoped_on(&conn, &spec, scope.param()).unwrap();
            (rows.into_iter().map(|row| row.id).collect(), total)
        };
        assert_eq!(ids(QuerySpec { limit: 10, ..QuerySpec::default() }), (vec![2, 1], 2));
        assert_eq!(ids(QuerySpec { limit: 1, ..QuerySpec::default() }), (vec![2], 2));
        assert_eq!(ids(QuerySpec { limit: 1, offset: 1, ..QuerySpec::default() }), (vec![1], 2));
        assert_eq!(ids(QuerySpec { limit: 10, kind: Some("image".into()), ..QuerySpec::default() }), (vec![1], 1));
        assert_eq!(ids(QuerySpec { limit: 10, search: "b.mov".into(), ..QuerySpec::default() }), (vec![2], 1));
    }
}
