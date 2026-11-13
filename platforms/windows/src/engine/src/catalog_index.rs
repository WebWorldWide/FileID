use crate::{
    db,
    ipc::{CatalogHit, CatalogRequest, CatalogResponse},
    models::{clip_space, search_strength},
    resource_scheduler::{Demand, Priority, ResourceScheduler},
    util::hnsw_index::{self, Embedding, Searcher},
};
use anyhow::{bail, ensure, Result};
use instant_distance::HnswMap;
use parking_lot::{Condvar, Mutex};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

#[path = "catalog_index_snapshot.rs"]
mod snapshot;

const MAX_FILES: usize = 200_000;
static CURRENT: Mutex<std::sync::Weak<CatalogIndex>> = Mutex::new(std::sync::Weak::new());
const MAX_GRAPH_BYTES: u64 = snapshot::max_graph_bytes(clip_space::DIMENSION, MAX_FILES);
/// JSON snapshots written before the binary format; read once, then rewritten.
const LEGACY_MAX_GRAPH_BYTES: u64 = 512 * 1024 * 1024;
const EF_SEARCH: usize = 1000;
/// Changes kept beside the graph and searched directly. Past this many (or a
/// tenth of the graph) the graph is rebuilt instead.
const MIN_PENDING_BEFORE_REBUILD: usize = 256;
const MAX_PENDING: usize = MAX_FILES / 10 + MIN_PENDING_BEFORE_REBUILD;
const MAX_DELTA_BYTES: u64 = snapshot::max_delta_bytes(clip_space::DIMENSION, MAX_PENDING, MAX_PENDING);
const SAVE_RETRY: std::time::Duration = std::time::Duration::from_secs(60);
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Revision {
    instance: String,
    pub(crate) generation: i64,
    nonce: String,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Entry {
    file_id: i64,
    fingerprint: String,
}
#[derive(Deserialize, Serialize)]
struct Manifest {
    model: String,
    dimension: usize,
    revision: Revision,
    graph_hash: String,
    /// Absent for the JSON snapshots older engines wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    format: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delta_hash: Option<String>,
}
struct Cache {
    graph: Arc<HnswMap<Embedding, Entry>>,
    /// Vectors added or changed since `graph` was built, searched directly.
    added: Vec<(Vec<f32>, Entry)>,
    /// Files in `graph` that are gone or superseded by `added`.
    removed: HashSet<i64>,
    revision: Revision,
    lease: Option<crate::resource_scheduler::Lease>,
    saved: Mutex<Saved>,
}
#[derive(Default)]
struct Saved {
    /// The binary graph file on disk that holds `graph`, when there is one.
    graph_hash: Option<String>,
    /// Set after a failed save; the next search past it tries again.
    retry_at: Option<std::time::Instant>,
}
impl Cache {
    fn built(graph: HnswMap<Embedding, Entry>, revision: Revision) -> Self {
        Self { graph: Arc::new(graph), added: Vec::new(), removed: HashSet::new(), revision, lease: None, saved: Mutex::default() }
    }
    fn pending(&self) -> usize {
        self.added.len() + self.removed.len()
    }
    /// Nearest entries across the graph and the changes beside it, closest first.
    fn nearest(&self, query: &[f32], wanted: usize) -> Vec<(Entry, f32)> {
        let mut candidates = Searcher::default().top_k(&self.graph, query, wanted);
        candidates.retain(|(entry, _)| !self.removed.contains(&entry.file_id));
        candidates.extend(
            self.added
                .iter()
                .map(|(vector, entry)| (entry.clone(), crate::util::vector_math::squared_l2(vector, query))),
        );
        candidates.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.file_id.cmp(&b.0.file_id)));
        candidates.truncate(wanted);
        candidates
    }
}
fn needs_rebuild(graph: usize, pending: usize) -> bool {
    pending > MIN_PENDING_BEFORE_REBUILD.max(graph / 10)
}
#[derive(Default)]
struct State {
    cache: Option<Arc<Cache>>,
    worker: Option<Arc<AtomicBool>>,
    closed: bool,
}
/// A search reply plus the files judged weaker look-alikes; `weak` is None
/// when the search was not judged (keyword-only, similar-to-a-file, indexing).
pub(crate) struct Judged {
    pub(crate) response: CatalogResponse,
    pub(crate) weak: Option<std::collections::HashSet<i64>>,
}
impl Judged {
    /// Good matches first, then weaker look-alikes, and a count that says so.
    pub(crate) fn labelled(mut self) -> Self {
        if let Some(weak) = &self.weak {
            let (good, weaker): (Vec<_>, Vec<_>) = std::mem::take(&mut self.response.hits).into_iter().partition(|hit| !weak.contains(&hit.file_id));
            self.response.message = Some(search_strength::summary(good.len(), weaker.len()));
            self.response.hits = good.into_iter().chain(weaker).collect();
        }
        self
    }
}
/// A read-only connection lent out for one search and returned afterwards, so
/// searches skip opening the catalog and parsing its schema each time.
pub(crate) struct Reader<'a> {
    slot: &'a Mutex<Option<Connection>>,
    conn: Option<Connection>,
}
impl std::ops::Deref for Reader<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.conn.as_ref().expect("held until drop")
    }
}
impl Drop for Reader<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take()
            && conn.is_autocommit()
        {
            let mut slot = self.slot.lock();
            if slot.is_none() {
                *slot = Some(conn);
            }
        }
    }
}
pub struct CatalogIndex {
    writer: Arc<Mutex<Connection>>,
    database: PathBuf,
    directory: PathBuf,
    reader: Mutex<Option<Connection>>,
    state: Mutex<State>,
    changed: Condvar,
    scheduler: Arc<ResourceScheduler>,
}
impl CatalogIndex {
    pub fn new(writer: Arc<Mutex<Connection>>, database: PathBuf, directory: PathBuf) -> Arc<Self> {
        let index = Self::with_scheduler(writer, database, directory, ResourceScheduler::shared());
        *CURRENT.lock() = Arc::downgrade(&index);
        index
    }
    /// The engine's live index, for Chat's visual retrieval.
    pub fn current() -> Option<Arc<Self>> {
        CURRENT.lock().upgrade()
    }
    fn with_scheduler(
        writer: Arc<Mutex<Connection>>,
        database: PathBuf,
        directory: PathBuf,
        scheduler: Arc<ResourceScheduler>,
    ) -> Arc<Self> {
        Arc::new(Self {
            writer,
            database,
            directory,
            reader: Mutex::new(None),
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            scheduler,
        })
    }
    pub fn job_id() -> String {
        format!(
            "catalog-index-{}",
            clip_space::digest(clip_space::model_id().as_bytes())
        )
    }
    pub fn recover(conn: &Connection) -> Result<()> {
        conn.execute("UPDATE catalog_jobs SET state='paused',error='Interrupted. Resume visual index preparation from the last verified snapshot.',updated_at=?1 WHERE kind='catalogIndex' AND state IN ('queued','running')", [now()])?;
        Ok(())
    }
    /// The cached read connection, or a new one while another search holds it.
    pub(crate) fn reader(&self) -> Result<Reader<'_>> {
        let cached = self.reader.lock().take();
        let conn = match cached {
            Some(conn) => conn,
            None => {
                let conn = db::open_read(&self.database)?;
                conn.set_prepared_statement_cache_capacity(32);
                conn
            }
        };
        Ok(Reader { slot: &self.reader, conn: Some(conn) })
    }
    pub fn close(&self) {
        self.reader.lock().take();
        self.state.lock().closed = true;
        self.shutdown();
        self.state.lock().cache = None;
    }
    pub fn shutdown(&self) {
        let mut state = self.state.lock();
        if let Some(cancel) = &state.worker {
            cancel.store(true, Ordering::Relaxed);
        }
        while state.worker.is_some() {
            self.changed.wait(&mut state);
        }
    }
    pub fn control(self: &Arc<Self>, request: &CatalogRequest) -> Result<()> {
        ensure!(
            request.job_id.as_deref() == Some(Self::job_id().as_str()),
            "Unknown visual index job"
        );
        let target = match request.action.as_str() {
            "pauseJob" => "paused",
            "cancelJob" => "cancelled",
            "resumeJob" => "queued",
            _ => bail!("Invalid index control"),
        };
        // Wait for the cancelled worker before a resume can acquire its job identity.
        if target == "queued" {
            let current: String = self.writer.lock().query_row(
                "SELECT state FROM catalog_jobs WHERE id=?1",
                [Self::job_id()],
                |row| row.get(0),
            )?;
            ensure!(
                ["paused", "failed", "cancelled"].contains(&current.as_str()),
                "This index job cannot resume"
            );
            self.shutdown();
        }
        {
            let conn = self.writer.lock();
            let current: String = conn.query_row(
                "SELECT state FROM catalog_jobs WHERE id=?1 AND kind='catalogIndex'",
                [Self::job_id()],
                |row| row.get(0),
            )?;
            let allowed = match target {
                "queued" => ["paused", "failed", "cancelled"].contains(&current.as_str()),
                "paused" => ["queued", "running"].contains(&current.as_str()),
                _ => ["queued", "running", "paused"].contains(&current.as_str()),
            };
            ensure!(allowed, "This index job cannot make that transition");
            conn.execute(
                "UPDATE catalog_jobs SET state=?1,error=NULL,updated_at=?2 WHERE id=?3",
                params![target, now(), Self::job_id()],
            )?;
        }
        if target == "queued" {
            self.prepare()?;
        } else {
            self.shutdown();
            self.state.lock().cache = None;
        }
        Ok(())
    }
    pub fn search(self: &Arc<Self>, request: &CatalogRequest) -> Result<CatalogResponse> {
        Ok(self.judged(request, &search_strength::live_encoder)?.response)
    }
    /// The reply plus the files judged weaker look-alikes (Swift `CatalogSearch.judged`).
    pub(crate) fn judged(self: &Arc<Self>, request: &CatalogRequest, encode: &dyn Fn(&str) -> Option<Vec<f32>>) -> Result<Judged> {
        let root = match request.root_id {
            Some(id) => {
                let conn = self.reader()?;
                crate::commands::roots::RootScope::load(&conn, Some(id))?
            }
            None => None,
        };
        if request.captured_from.is_none() && request.captured_to.is_none() {
            return Ok(self.search_unfiltered(request, root.as_ref(), encode)?.labelled());
        }
        let from = request.captured_from.unwrap_or(0.0);
        let to = request.captured_to.unwrap_or(f64::MAX);
        if !from.is_finite() || from < 0.0 || to < from || to.is_nan() {
            bail!("Invalid catalog search")
        }
        let limit = request.limit.unwrap_or(100);
        if !(1..=100).contains(&limit) {
            bail!("Invalid catalog search")
        }
        let mut wide = request.clone();
        wide.limit = Some(100);
        let mut judged = self.search_unfiltered(&wide, root.as_ref(), encode)?;
        let response = &mut judged.response;
        let conn = self.reader()?;
        let mut statement = conn.prepare("SELECT 1 FROM files WHERE id=?1 AND COALESCE(created_at,modified_at) BETWEEN ?2 AND ?3")?;
        let mut inside = std::collections::HashSet::new();
        for hit in &response.hits {
            if inside.contains(&hit.file_id) {
                continue;
            }
            if statement.exists(rusqlite::params![hit.file_id, from, to])? {
                inside.insert(hit.file_id);
            }
        }
        response.hits.retain(|hit| inside.contains(&hit.file_id));
        response.hits.truncate(limit);
        // Ranking keeps the best hundred overall; files in the window beyond them
        // come from every file the words describe, newest first (Swift `CatalogSearch.handle`).
        let query = request.query.as_deref().map(str::trim).unwrap_or_default();
        if response.hits.len() < limit
            && !query.is_empty()
            && request.search_mode.as_deref().unwrap_or("keyword") != "semantic"
            && let Some(subject) = crate::commands::chat_facts::subject_ids(&conn, query)?
        {
            {
                let seen: std::collections::HashSet<i64> = response.hits.iter().map(|hit| hit.file_id).collect();
                let wanted = serde_json::to_string(&subject)?;
                let mut more = conn.prepare(
                    "SELECT id,path_text,kind,COALESCE(vlm_description,'') FROM files
                     WHERE failed=0 AND id IN (SELECT value FROM json_each(?1)) AND COALESCE(created_at,modified_at) BETWEEN ?2 AND ?3 AND {}
                     ORDER BY COALESCE(created_at,modified_at) DESC,id DESC LIMIT ?4"
                        .replace("{}", &crate::commands::roots::clause("path_text", "?5"))
                        .as_str(),
                )?;
                let rows = more.query_map(rusqlite::params![wanted, from, to, (limit + seen.len()) as i64, root.as_ref().map(|root| root.param())], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?))
                })?;
                for row in rows {
                    let (file_id, path, kind, text) = row?;
                    if response.hits.len() >= limit {
                        break;
                    }
                    if seen.contains(&file_id) {
                        continue;
                    }
                    response.hits.push(CatalogHit { file_id, path, kind, text, evidence_id: None, start_seconds: None, page: None, selection_match: None });
                }
            }
        }
        Ok(judged.labelled())
    }

    fn search_unfiltered(
        self: &Arc<Self>,
        request: &CatalogRequest,
        root: Option<&crate::commands::roots::RootScope>,
        encode: &dyn Fn(&str) -> Option<Vec<f32>>,
    ) -> Result<Judged> {
        let (mode, query, limit, files_only) = crate::commands::catalog_search::validate(request)?;
        let retrieval_lease = if mode == "semantic" {
            None
        } else {
            Some(self.scheduler.reserve(
                Demand {
                    memory_mb: if mode == "keyword" { 0 } else { 32 },
                    cpu: 1,
                    io: 1,
                },
                Priority::Interactive,
                || false,
            )?)
        };
        let conn = self.reader()?;
        let tx = conn.unchecked_transaction()?;
        let keyword = if mode == "semantic" {
            vec![]
        } else {
            crate::commands::catalog::search(&tx, query, &[], root)?
        };
        let mut response = CatalogResponse {
            request_id: request.request_id.clone(),
            status: "ok".into(),
            message: None,
            hits: vec![],
            chapters: vec![],
            jobs: vec![],
            events: None,
            takes: None,
            recommendation: None,
            suggested_take_groups: None,
            take_groups: None,
            take_qualities: None,
            take_goal_ranking: None,
        };
        if mode == "keyword" {
            response.hits = crate::commands::catalog_search::select(keyword, limit, files_only);
            return Ok(Judged { response, weak: None });
        }
        let mut query_vector = crate::commands::catalog_search::vector(&tx, request)?;
        normalize(&mut query_vector);
        let judging = request.query_vector.is_some() && search_strength::applies();
        if judging {
            query_vector = search_strength::scoring_vector(query, &query_vector, encode);
        }
        let latest = revision(&tx)?;
        let cached = self.state.lock().cache.clone().filter(|cache| cache.revision == latest);
        let Some(cache) = cached else {
            drop(tx);
            drop(retrieval_lease);
            self.prepare()?;
            response.status = if mode == "semantic" { "error" } else { "ok" }.into();
            response.message = Some(
                if mode == "semantic" {
                    "Preparing the local visual search index. Similarity results are not available yet."
                } else {
                    "Keyword results only. Preparing the local visual search index."
                }
                .into(),
            );
            response.jobs = crate::commands::catalog::jobs(&conn)?;
            response.hits = crate::commands::catalog_search::select(keyword, limit, files_only);
            return Ok(Judged { response, weak: None });
        };
        let _visual_lease = if mode == "semantic" {
            Some(self.scheduler.reserve(
                Demand {
                    memory_mb: 32,
                    cpu: 1,
                    io: 1,
                },
                Priority::Interactive,
                || false,
            )?)
        } else {
            None
        };
        self.retry_save(&cache);
        let wanted = if root.is_some() { 1000 } else { (limit * 4 + 1).min(1000) };
        let candidates = cache.nearest(&query_vector, wanted);
        let mut visual: Vec<(CatalogHit, f32)> = Vec::new();
        let mut stmt = tx.prepare("SELECT f.id,f.path_text,f.kind,COALESCE(f.vlm_description,''),e.embedding FROM files f JOIN clip_embeddings e ON e.file_id=f.id WHERE f.id=?1 AND f.failed=0 AND e.model=?2")?;
        for (entry, _) in candidates {
            if Some(entry.file_id) == request.file_id {
                continue;
            }
            let found = stmt
                .query_row(params![entry.file_id, clip_space::model_id()], |row| {
                    Ok((
                        CatalogHit {
                            file_id: row.get(0)?,
                            path: row.get(1)?,
                            kind: row.get(2)?,
                            text: row.get(3)?,
                            evidence_id: None,
                            start_seconds: None,
                            page: None,
                            selection_match: None,
                        },
                        row.get::<_, Vec<u8>>(4)?,
                    ))
                })
                .optional()?;
            if let Some((hit, blob)) = found
                && root.is_none_or(|root| root.contains(&hit.path))
                && clip_space::digest(&blob) == entry.fingerprint
                && let Some(embedding) = clip_space::decode(&blob) {
                    let margin = if judging { search_strength::score(&query_vector, &embedding).1 } else { 0.0 };
                    visual.push((hit, margin));
                }
        }
        if judging {
            visual.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.file_id.cmp(&b.0.file_id)));
        }
        let weak_visual: Option<std::collections::HashSet<i64>> =
            judging.then(|| visual.iter().filter(|(_, margin)| !search_strength::is_strong(*margin)).map(|(hit, _)| hit.file_id).collect());
        let visual: Vec<CatalogHit> = visual.into_iter().map(|(hit, _)| hit).collect();
        response.message = Some(
            if mode == "semantic" {
                "Visual similarity results."
            } else {
                "Matches by name, text and what's in the picture."
            }
            .into(),
        );
        if mode == "semantic" {
            response.hits = crate::commands::catalog_search::select(visual, limit, files_only);
            return Ok(Judged { response, weak: weak_visual });
        }
        let Some(mut weak) = weak_visual else {
            response.hits = crate::commands::catalog_search::merge(keyword, visual, limit, files_only);
            return Ok(Judged { response, weak: None });
        };
        for hit in &keyword {
            weak.remove(&hit.file_id);
        }
        let everything = keyword.len() + visual.len();
        let merged = crate::commands::catalog_search::merge(keyword, visual, everything, files_only);
        let (good, weaker): (Vec<_>, Vec<_>) = merged.into_iter().partition(|hit| !weak.contains(&hit.file_id));
        response.hits = crate::commands::catalog_search::select(good.into_iter().chain(weaker).collect(), limit, files_only);
        Ok(Judged { response, weak: Some(weak) })
    }
    fn prepare(self: &Arc<Self>) -> Result<()> {
        let mut state = self.state.lock();
        ensure!(!state.closed, "The visual index runtime is shutting down");
        if state.worker.is_some() {
            return Ok(());
        }
        {
            let conn = self.writer.lock();
            let id = Self::job_id();
            let recipe = serde_json::json!({"model":clip_space::model_id()}).to_string();
            conn.execute("INSERT OR IGNORE INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,priority,created_at,updated_at) VALUES(?1,'catalogIndex','[]',?2,'queued',1,?3,?3)",params![id,recipe,now()])?;
            let current: String =
                conn.query_row("SELECT state FROM catalog_jobs WHERE id=?1", [&id], |row| row.get(0))?;
            ensure!(
                ["queued", "completed"].contains(&current.as_str()),
                "Visual index preparation is stopped or failed. Resume its job; keyword search remains available."
            );
            conn.execute(
                "UPDATE catalog_jobs SET state='running',progress=0,error=NULL,updated_at=?1 WHERE id=?2",
                params![now(), id],
            )?;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        state.worker = Some(cancel.clone());
        let previous = state.cache.clone();
        let owner = self.clone();
        let started = std::thread::Builder::new().name("catalog-index".into()).spawn(move || {
            let result =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.synchronize(previous, &cancel)))
                    .unwrap_or_else(|_| Err(anyhow::anyhow!("Visual index worker interrupted")));
            owner.finish(result, &cancel);
        });
        if let Err(error) = started {
            state.worker = None;
            self.changed.notify_all();
            self.writer.lock().execute(
                "UPDATE catalog_jobs SET state='failed',error=?1 WHERE id=?2 AND state='running'",
                params![error.to_string(), Self::job_id()],
            )?;
            return Err(error.into());
        }
        Ok(())
    }
    fn finish(&self, result: Result<Cache>, cancel: &AtomicBool) {
        let mut state = self.state.lock();
        let (target, message) = match result {
            Ok(cache) if !cancel.load(Ordering::Relaxed) => {
                state.cache = Some(Arc::new(cache));
                ("completed", None)
            }
            Ok(_) => ("paused", Some("Visual index preparation interrupted".to_owned())),
            Err(error) => {
                let message = error.to_string();
                (
                    if cancel.load(Ordering::Relaxed) || message.starts_with("Resource admission") {
                        "paused"
                    } else {
                        "failed"
                    },
                    Some(message),
                )
            }
        };
        let result = self.writer.lock().execute("UPDATE catalog_jobs SET state=?1,error=?2,progress=CASE WHEN ?1='completed' THEN 1 ELSE progress END,updated_at=?3 WHERE id=?4 AND state='running'",params![target,message,now(),Self::job_id()]);
        if result.is_err() {
            state.cache = None;
        }
        state.worker = None;
        self.changed.notify_all();
    }
    fn checkpoint(&self, revision: &Revision, processed: usize, total: usize, cancel: &AtomicBool) -> Result<()> {
        check(cancel)?;
        let checkpoint =
            serde_json::json!({"model":clip_space::model_id(),"revision":revision,"processed":processed,"total":total})
                .to_string();
        let changed = self.writer.lock().execute(
            "UPDATE catalog_jobs SET checkpoint_json=?1,progress=?2,updated_at=?3 WHERE id=?4 AND state='running'",
            params![
                checkpoint,
                (processed as f64 / total.max(1) as f64).min(0.99),
                now(),
                Self::job_id()
            ],
        )?;
        ensure!(changed == 1, "Visual index job stopped");
        Ok(())
    }
    fn synchronize(&self, previous: Option<Arc<Cache>>, cancel: &AtomicBool) -> Result<Cache> {
        check(cancel)?;
        let conn = db::open_read(&self.database)?;
        let tx = conn.unchecked_transaction()?;
        let latest = revision(&tx)?;
        let count: usize = tx.query_row(
            "SELECT COUNT(*) FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE e.model=?1 AND f.failed=0",
            [clip_space::model_id()],
            |row| row.get(0),
        )?;
        ensure!(count <= MAX_FILES, "Visual index capacity exceeded");
        let mut candidate = if previous.is_none() {
            self.cache_candidate(&tx, &latest).ok()
        } else {
            None
        };
        // Loading holds the validated arrays and the graph at once; a JSON
        // snapshot is parsed into a value tree many times its size.
        let disk_mb = candidate
            .as_ref()
            .map(|(manifest, bytes)| bytes.div_ceil(1_048_576) * if manifest.format.is_some() { 3 } else { 20 })
            .unwrap_or(0);
        let rebuild_memory_mb = 64 + (count as u64).saturating_mul(BUILD_BYTES_PER_POINT).div_ceil(1_048_576);
        let reserve = |memory_mb| {
            self.scheduler.reserve(
                Demand {
                    memory_mb,
                    cpu: 1,
                    io: 1,
                },
                Priority::Background,
                || cancel.load(Ordering::Relaxed),
            )
        };
        let lease = match reserve(rebuild_memory_mb + disk_mb) {
            Ok(lease) => Ok(lease),
            Err(_) if candidate.is_some() => {
                candidate = None;
                reserve(rebuild_memory_mb)
            }
            Err(error) => Err(error),
        }
        .map_err(|error| anyhow::anyhow!("Resource admission deferred: {error}"))?;
        let disk = candidate.and_then(|(manifest, _)| self.load_graph(manifest).ok());
        if previous.is_none() && disk.as_ref().is_some_and(|cache| cache.revision == latest) {
            let mut cache = disk.expect("verified disk cache");
            if cache.saved.lock().graph_hash.is_none() {
                self.save_or_defer(&cache, cancel)?;
            }
            lease.retain_memory();
            cache.lease = Some(lease);
            return Ok(cache);
        }
        let base = previous.as_deref().or(disk.as_ref());
        let mut result = match base.filter(|cache| can_advance(&tx, &cache.revision, &latest).unwrap_or(false)) {
            Some(cache) => self.advance(&tx, cache, latest, cancel)?,
            None => self.rebuild(&tx, latest, cancel)?,
        };
        check(cancel)?;
        self.save_or_defer(&result, cancel)?;
        lease.retain_memory();
        result.lease = Some(lease);
        Ok(result)
    }
    /// Applies the catalog's vector changes since `cache` beside its graph,
    /// or rebuilds the graph from memory once too many have piled up.
    fn advance(&self, tx: &Connection, cache: &Cache, latest: Revision, cancel: &AtomicBool) -> Result<Cache> {
        let in_graph: HashMap<i64, &str> =
            cache.graph.values.iter().map(|entry| (entry.file_id, entry.fingerprint.as_str())).collect();
        let mut added: BTreeMap<i64, (Vec<f32>, Entry)> =
            cache.added.iter().map(|(vector, entry)| (entry.file_id, (vector.clone(), entry.clone()))).collect();
        let mut removed = cache.removed.clone();
        let ids = tx.prepare("SELECT DISTINCT file_id FROM catalog_vector_changes WHERE namespace='clip' AND generation>?1 AND generation<=?2 ORDER BY file_id")?.query_map(params![cache.revision.generation,latest.generation], |row| row.get::<_,i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut current = tx.prepare("SELECT e.embedding FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE e.file_id=?1 AND e.model=?2 AND f.failed=0")?;
        for (processed, id) in ids.iter().enumerate() {
            check(cancel)?;
            if processed % 128 == 0 {
                self.checkpoint(&latest, processed, ids.len(), cancel)?;
            }
            added.remove(id);
            let blob: Option<Vec<u8>> = current.query_row(params![id, clip_space::model_id()], |row| row.get(0)).optional()?;
            let entry = blob.and_then(|blob| {
                let mut vector = clip_space::decode(&blob)?;
                normalize(&mut vector);
                Some((vector, Entry { file_id: *id, fingerprint: clip_space::digest(&blob) }))
            });
            match (in_graph.get(id), entry) {
                (Some(fingerprint), Some((_, entry))) if *fingerprint == entry.fingerprint => {
                    removed.remove(id);
                }
                (known, entry) => {
                    if known.is_some() {
                        removed.insert(*id);
                    }
                    if let Some(entry) = entry {
                        added.insert(*id, entry);
                    }
                }
            }
        }
        drop(current);
        ensure!(in_graph.len() - removed.len() + added.len() <= MAX_FILES, "Visual index capacity exceeded");
        if !needs_rebuild(in_graph.len(), added.len() + removed.len()) {
            return Ok(Cache {
                graph: cache.graph.clone(),
                added: added.into_values().collect(),
                removed,
                revision: latest,
                lease: None,
                saved: Mutex::new(Saved { graph_hash: cache.saved.lock().graph_hash.clone(), retry_at: None }),
            });
        }
        let mut points: BTreeMap<i64, (Vec<f32>, Entry)> = cache
            .graph
            .iter()
            .zip(&cache.graph.values)
            .filter(|(_, entry)| !removed.contains(&entry.file_id))
            .map(|((_, point), entry)| (entry.file_id, (point.0.clone(), entry.clone())))
            .collect();
        points.extend(added);
        check(cancel)?;
        Ok(Cache::built(hnsw_index::build_search_index(points.into_values().collect(), EF_SEARCH), latest))
    }
    fn rebuild(&self, conn: &Connection, latest: Revision, cancel: &AtomicBool) -> Result<Cache> {
        let mut stmt = conn.prepare("SELECT e.file_id,e.embedding FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE e.model=?1 AND f.failed=0 ORDER BY e.file_id")?;
        let total: usize = conn.query_row(
            "SELECT COUNT(*) FROM clip_embeddings e JOIN files f ON f.id=e.file_id WHERE e.model=?1 AND f.failed=0",
            [clip_space::model_id()],
            |row| row.get(0),
        )?;
        let mut processed = 0;
        let mut rows = stmt.query([clip_space::model_id()])?;
        let mut points = Vec::new();
        while let Some(row) = rows.next()? {
            check(cancel)?;
            if processed % 128 == 0 {
                self.checkpoint(&latest, processed, total, cancel)?;
            }
            processed += 1;
            let blob: Vec<u8> = row.get(1)?;
            if let Some(mut vector) = clip_space::decode(&blob) {
                normalize(&mut vector);
                points.push((
                    vector,
                    Entry {
                        file_id: row.get(0)?,
                        fingerprint: clip_space::digest(&blob),
                    },
                ));
            }
            ensure!(points.len() <= MAX_FILES, "Visual index capacity exceeded");
        }
        let graph = hnsw_index::build_search_index(points, EF_SEARCH);
        check(cancel)?;
        Ok(Cache::built(graph, latest))
    }
    fn manifest_path(&self) -> PathBuf {
        self.directory.join(format!(
            "{}.json",
            clip_space::digest(clip_space::model_id().as_bytes())
        ))
    }
    fn shape() -> snapshot::Shape<'static> {
        snapshot::Shape {
            model: clip_space::model_id(),
            dimension: clip_space::DIMENSION,
            max_points: MAX_FILES,
            ef_search: EF_SEARCH as u64,
            valid: &clip_space::valid,
        }
    }
    /// A save that fails for any reason but cancellation leaves the index
    /// usable from memory; a later search tries again.
    fn save_or_defer(&self, cache: &Cache, cancel: &AtomicBool) -> Result<()> {
        match self.save(cache, cancel) {
            Ok(()) => Ok(()),
            Err(error) if cancel.load(Ordering::Relaxed) => Err(error),
            Err(error) => {
                tracing::warn!(%error, "visual index snapshot was not saved; searching from memory");
                cache.saved.lock().retry_at = Some(std::time::Instant::now() + SAVE_RETRY);
                Ok(())
            }
        }
    }
    fn retry_save(self: &Arc<Self>, cache: &Arc<Cache>) {
        if cache.saved.lock().retry_at.is_none_or(|at| std::time::Instant::now() < at) {
            return;
        }
        let mut state = self.state.lock();
        if state.closed || state.worker.is_some() {
            return;
        }
        cache.saved.lock().retry_at = Some(std::time::Instant::now() + SAVE_RETRY);
        let cancel = Arc::new(AtomicBool::new(false));
        state.worker = Some(cancel.clone());
        let (owner, cache) = (self.clone(), cache.clone());
        let started = std::thread::Builder::new().name("catalog-index-save".into()).spawn(move || {
            let saved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| owner.save(&cache, &cancel)));
            if !matches!(saved, Ok(Ok(()))) {
                tracing::warn!("visual index snapshot retry did not finish; it will be tried again");
            }
            owner.state.lock().worker = None;
            owner.changed.notify_all();
        });
        if started.is_err() {
            state.worker = None;
            self.changed.notify_all();
        }
    }
    fn save(&self, cache: &Cache, cancel: &AtomicBool) -> Result<()> {
        check(cancel)?;
        crate::util::read_only::require_writable(&self.directory)?;
        std::fs::create_dir_all(&self.directory)?;
        let old_manifest = bounded_read(&self.manifest_path(), 65536)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok());
        self.sweep_staging();
        let stop = || cancel.load(Ordering::Relaxed);
        let on_disk = cache.saved.lock().graph_hash.clone().filter(|hash| self.directory.join(format!("{hash}.graph")).is_file());
        let graph_hash = match on_disk {
            Some(hash) => hash,
            None => atomic_stream(&self.directory, "graph", MAX_GRAPH_BYTES, |file| {
                snapshot::write_graph(file, clip_space::model_id(), clip_space::DIMENSION, &cache.graph, &stop)
            })?,
        };
        check(cancel)?;
        let delta_hash = if cache.pending() == 0 {
            None
        } else {
            ensure!(cache.added.len() <= MAX_PENDING && cache.removed.len() <= MAX_PENDING, "Visual index change capacity exceeded");
            let mut removed: Vec<i64> = cache.removed.iter().copied().collect();
            removed.sort_unstable();
            let delta = snapshot::Delta { added: cache.added.clone(), removed };
            Some(atomic_stream(&self.directory, "delta", MAX_DELTA_BYTES, |file| {
                snapshot::write_delta(file, clip_space::DIMENSION, &delta, &stop)
            })?)
        };
        check(cancel)?;
        let manifest = Manifest {
            model: clip_space::model_id().into(),
            dimension: clip_space::DIMENSION,
            revision: cache.revision.clone(),
            graph_hash,
            format: Some(snapshot::VERSION),
            delta_hash,
        };
        atomic_write(&self.manifest_path(), &serde_json::to_vec(&manifest)?)?;
        {
            let mut saved = cache.saved.lock();
            saved.graph_hash = Some(manifest.graph_hash.clone());
            saved.retry_at = None;
        }
        if let Some(old) = old_manifest {
            let stale = [
                Some((old.graph_hash, "graph")).filter(|(hash, _)| *hash != manifest.graph_hash),
                old.delta_hash.map(|hash| (hash, "delta")).filter(|(hash, _)| Some(hash) != manifest.delta_hash.as_ref()),
            ];
            for (hash, extension) in stale.into_iter().flatten().filter(|(hash, _)| valid_digest(hash)) {
                let path = self.directory.join(format!("{hash}.{extension}"));
                if crate::util::read_only::require_writable(&path).is_ok()
                    && let Err(error) = std::fs::remove_file(&path)
                    && error.kind() != std::io::ErrorKind::NotFound
                {
                    tracing::warn!(%error, "an old visual index snapshot could not be removed");
                }
            }
        }
        Ok(())
    }
    /// Removes staging files an interrupted save left behind.
    fn sweep_staging(&self) {
        let Ok(entries) = std::fs::read_dir(&self.directory) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.contains(".tmp-") && entry.file_type().is_ok_and(|kind| kind.is_file()) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    fn cache_candidate(&self, conn: &Connection, latest: &Revision) -> Result<(Manifest, u64)> {
        let manifest: Manifest = serde_json::from_slice(&bounded_read(&self.manifest_path(), 65536)?)?;
        ensure!(
            manifest.model == clip_space::model_id()
                && manifest.dimension == clip_space::DIMENSION
                && can_advance(conn, &manifest.revision, latest)?
                && valid_digest(&manifest.graph_hash)
                && manifest.format.is_none_or(|format| format == snapshot::VERSION)
                && manifest.delta_hash.as_deref().is_none_or(valid_digest),
            "Incompatible visual index snapshot"
        );
        let limit = if manifest.format.is_some() { MAX_GRAPH_BYTES } else { LEGACY_MAX_GRAPH_BYTES };
        let metadata = std::fs::metadata(self.directory.join(format!("{}.graph", manifest.graph_hash)))?;
        ensure!(
            metadata.is_file() && metadata.len() > 0 && metadata.len() <= limit,
            "Invalid or oversized visual index cache"
        );
        let mut bytes = metadata.len();
        if let Some(hash) = &manifest.delta_hash {
            let metadata = std::fs::metadata(self.directory.join(format!("{hash}.delta")))?;
            ensure!(
                metadata.is_file() && metadata.len() > 0 && metadata.len() <= MAX_DELTA_BYTES,
                "Invalid or oversized visual index cache"
            );
            bytes += metadata.len();
        }
        Ok((manifest, bytes))
    }
    #[cfg(test)]
    fn load(&self, conn: &Connection, latest: &Revision) -> Result<Cache> {
        self.load_graph(self.cache_candidate(conn, latest)?.0)
    }
    fn load_graph(&self, manifest: Manifest) -> Result<Cache> {
        let path = self.directory.join(format!("{}.graph", manifest.graph_hash));
        if manifest.format.is_none() {
            let graph = bounded_read(&path, LEGACY_MAX_GRAPH_BYTES)?;
            ensure!(
                clip_space::digest(&graph) == manifest.graph_hash,
                "Visual graph checksum mismatch"
            );
            validate_graph(&graph)?;
            return Ok(Cache::built(serde_json::from_slice(&graph)?, manifest.revision));
        }
        let shape = Self::shape();
        let (file, length) = open_bounded(&path, MAX_GRAPH_BYTES)?;
        let graph = snapshot::read_graph(file, length, &manifest.graph_hash, &shape)?;
        let mut cache = Cache::built(graph, manifest.revision);
        if let Some(hash) = &manifest.delta_hash {
            let (file, length) = open_bounded(&self.directory.join(format!("{hash}.delta")), MAX_DELTA_BYTES)?;
            let delta = snapshot::read_delta(file, length, hash, &shape)?;
            ensure!(delta.added.len() <= MAX_PENDING && delta.removed.len() <= MAX_PENDING, "Invalid visual index change count");
            let in_graph: HashSet<i64> = cache.graph.values.iter().map(|entry| entry.file_id).collect();
            ensure!(
                delta.removed.iter().all(|id| in_graph.contains(id))
                    && delta.added.iter().all(|(_, entry)| !in_graph.contains(&entry.file_id) || delta.removed.contains(&entry.file_id)),
                "Visual index change does not match its graph"
            );
            cache.added = delta.added;
            cache.removed = delta.removed.into_iter().collect();
        }
        cache.saved.lock().graph_hash = Some(manifest.graph_hash);
        Ok(cache)
    }
}
/// Peak memory while building: the rows read, the graph's own copy of them and
/// its neighbour lists.
const BUILD_BYTES_PER_POINT: u64 = 8192;
fn now() -> f64 {
    chrono::Utc::now().timestamp_millis() as f64 / 1000.0
}
fn check(cancel: &AtomicBool) -> Result<()> {
    ensure!(!cancel.load(Ordering::Relaxed), "Visual index preparation cancelled");
    Ok(())
}
fn revision(conn: &Connection) -> Result<Revision> {
    vector_revision(conn, "clip")
}
fn can_advance(conn: &Connection, old: &Revision, latest: &Revision) -> Result<bool> {
    vector_can_advance(conn, "clip", old, latest)
}
pub(crate) fn vector_revision(conn: &Connection, namespace: &str) -> Result<Revision> {
    Ok(conn.query_row(
        "SELECT instance_id,generation,nonce FROM catalog_vector_state WHERE namespace=?1",
        [namespace],
        |row| {
            Ok(Revision {
                instance: row.get(0)?,
                generation: row.get(1)?,
                nonce: row.get(2)?,
            })
        },
    )?)
}
/// Whether the change log still holds every step from `old` to `latest`.
pub(crate) fn vector_can_advance(conn: &Connection, namespace: &str, old: &Revision, latest: &Revision) -> Result<bool> {
    if old.instance != latest.instance || old.generation > latest.generation || old.generation < 0 {
        return Ok(false);
    }
    if old.generation == latest.generation {
        return Ok(old.nonce == latest.nonce);
    }
    let checkpoint = if old.generation == 0 {
        Some(old.instance.clone())
    } else {
        conn.query_row(
            "SELECT nonce FROM catalog_vector_changes WHERE namespace=?1 AND generation=?2",
            params![namespace, old.generation],
            |row| row.get::<_, String>(0),
        )
        .optional()?
    };
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM catalog_vector_changes WHERE namespace=?1 AND generation>?2 AND generation<=?3",
        params![namespace, old.generation, latest.generation],
        |row| row.get(0),
    )?;
    Ok(checkpoint.as_deref() == Some(old.nonce.as_str()) && count == latest.generation - old.generation)
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn bounded_read(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() > 0 && metadata.len() <= maximum,
        "Invalid or oversized visual index cache"
    );
    let mut data = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut data)?;
    ensure!(data.len() as u64 <= maximum, "Visual index cache exceeds safe size");
    Ok(data)
}
fn open_bounded(path: &Path, maximum: u64) -> Result<(std::fs::File, u64)> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() > 0 && metadata.len() <= maximum,
        "Invalid or oversized visual index cache"
    );
    Ok((file, metadata.len()))
}
/// Streams a snapshot into a staging file and renames it to `<hash>.<extension>`.
fn atomic_stream(
    directory: &Path,
    extension: &str,
    maximum: u64,
    write: impl FnOnce(&mut std::fs::File) -> Result<(String, u64)>,
) -> Result<String> {
    let staging = directory.join(format!("{extension}.tmp-{}", uuid::Uuid::new_v4()));
    crate::util::read_only::require_writable(&staging)?;
    let result = (|| -> Result<String> {
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&staging)?;
        let (hash, bytes) = write(&mut file)?;
        ensure!(bytes <= maximum, "Visual graph snapshot capacity exceeded");
        file.sync_all()?;
        drop(file);
        std::fs::rename(&staging, directory.join(format!("{hash}.{extension}")))?;
        Ok(hash)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staging);
    }
    result
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::util::read_only::require_writable(path)?;
    let staging = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&staging, path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staging);
    }
    result
}
// Validate private HNSW topology before the crate's unchecked indexing can observe disk data.
// JSON snapshots only; binary ones are validated as they are read.
fn validate_graph(bytes: &[u8]) -> Result<()> {
    let value: serde_json::Value = serde_json::from_slice(bytes)?;

    let points = array(&value["hnsw"]["points"])?;
    let entries = array(&value["values"])?;
    let count = points.len();
    ensure!(
        count <= MAX_FILES && entries.len() == count && value["hnsw"]["ef_search"].as_u64() == Some(1000),
        "Invalid visual graph capacity"
    );
    let mut ids = std::collections::HashSet::new();
    for (point, entry) in points.iter().zip(entries) {
        let vector: Vec<f32> = serde_json::from_value(point.clone())?;
        ensure!(clip_space::valid(&vector), "Invalid visual graph vector");
        let id = entry["file_id"].as_i64().unwrap_or(0);
        ensure!(
            id > 0 && ids.insert(id) && entry["fingerprint"].as_str().is_some_and(valid_digest),
            "Invalid visual graph entry"
        );
    }
    let zero = array(&value["hnsw"]["zero"])?;
    ensure!(zero.len() == count, "Invalid graph node count");
    let layers = array(&value["hnsw"]["layers"])?;
    ensure!(layers.len() <= 32, "Invalid graph layer count");
    ensure!(layers.iter().all(serde_json::Value::is_array), "Malformed graph layer");
    let mut capacity = count;
    for (nodes, width) in std::iter::once((zero, 64)).chain(
        layers
            .iter()
            .map(|layer| (layer.as_array().expect("validated graph layer"), 32)),
    ) {
        ensure!(
            nodes.len() <= capacity && (count == 0 || !nodes.is_empty()),
            "Invalid graph layer capacity"
        );
        capacity = nodes.len();
        for node in nodes {
            let edges = array(node)?;
            ensure!(edges.len() == width, "Invalid graph edge count");
            for edge in edges {
                let id = edge.as_u64().ok_or_else(|| anyhow::anyhow!("Invalid graph neighbor"))?;
                ensure!(
                    id == u64::from(u32::MAX) || id < nodes.len() as u64,
                    "Graph neighbor out of bounds"
                );
            }
        }
    }
    Ok(())
}

fn array(value: &serde_json::Value) -> Result<&Vec<serde_json::Value>> {
    value.as_array().ok_or_else(|| anyhow::anyhow!("Malformed graph array"))
}
fn normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|v| f64::from(*v).powi(2)).sum::<f64>().sqrt() as f32;
    for value in vector {
        *value /= norm;
    }
}

#[cfg(test)]
#[path = "catalog_index_tests.rs"]
mod tests;
