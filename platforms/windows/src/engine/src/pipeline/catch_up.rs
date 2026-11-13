//! What a scan still owes a file after its main pass, and what it remembers
//! about files it could not read. Both live in `scan_catchup` (catalog v34),
//! shared with the Swift engine's `ScanCatchUp`, so either engine can finish
//! the other's work.

use parking_lot::Mutex;

/// RAM++ tags are owed (`ScanCatchUp.tagsOwed`).
pub const TAGS_OWED: i64 = 1;
/// The careful text read is owed (`ScanCatchUp.carefulTextOwed`).
pub const CAREFUL_TEXT_OWED: i64 = 2;
/// The fast text read ran during the scan (`ScanCatchUp.textReadAtScan`).
pub const TEXT_READ_AT_SCAN: i64 = 4;
pub const OWED_MASK: i64 = TAGS_OWED | CAREFUL_TEXT_OWED;

/// What the writer stores in `scan_catchup` next to a file's scan results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub stages: i64,
    /// The classifier context JSON (`ScanCatchUp.VisionContext`).
    pub vision: Option<String>,
    pub face_count: i64,
}

// MARK: - Files that keep failing

/// A file that could not be analysed is retried on this many scans, then left
/// alone until its size or date changes, the engine is updated, or the user
/// asks for a full rescan.
pub const FAILURE_RETRY_LIMIT: i64 = 3;

/// `scan_catchup.stages` of a failure-memory row. The value owes nothing
/// (no bit of `OWED_MASK`), so neither engine's catch-up pass selects it, and
/// neither engine writes it for an analysed file (a marker that owes nothing
/// is deleted). `attempts` counts the failures, `size_bytes`/`modified_at`
/// hold the file as it was, and `vision` names the engine build that failed.
pub const FAILURE_STAGE: i64 = TEXT_READ_AT_SCAN;

/// Names the engine and decoder set whose failures are remembered: a new
/// build, or a switch between the Mac adapters and the portable decoders,
/// retries every file.
pub fn failure_context() -> &'static str {
    static CONTEXT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CONTEXT.get_or_init(|| {
        let decoders = if crate::pipeline::tagging::apple_scan_enabled() { "apple" } else { "portable" };
        format!("{{\"failedBy\":\"fileid-engine {} {decoders}\"}}", env!("CARGO_PKG_VERSION"))
    })
}

/// ?1 file id, ?2 size, ?3 modified, ?4 `failure_context()`, ?5 now.
pub const RECORD_FAILURE_SQL: &str = "INSERT INTO scan_catchup \
     (file_id, stages, size_bytes, modified_at, vision, face_count, attempts, created_at) \
     VALUES (?1, 4, ?2, ?3, ?4, 0, 1, ?5) \
     ON CONFLICT(file_id) DO UPDATE SET \
       attempts = CASE WHEN scan_catchup.stages = 4 AND scan_catchup.size_bytes = excluded.size_bytes \
                        AND scan_catchup.modified_at IS excluded.modified_at \
                        AND scan_catchup.vision IS excluded.vision \
                   THEN scan_catchup.attempts + 1 ELSE 1 END, \
       stages = 4, size_bytes = excluded.size_bytes, modified_at = excluded.modified_at, \
       vision = excluded.vision, face_count = 0, created_at = excluded.created_at";

pub const CLEAR_FAILURE_SQL: &str = "DELETE FROM scan_catchup WHERE file_id = ?1 AND stages = 4";

/// Failed rows that have used up their retries and still describe the file
/// on record. ?1 `failure_context()`; callers append a path-range clause.
pub const GIVEN_UP_SQL: &str = "SELECT f.path_text, f.size_bytes, f.modified_at \
     FROM scan_catchup c JOIN files f ON f.id = c.file_id \
     WHERE c.stages = 4 AND c.attempts >= 3 AND f.failed = 1 \
       AND c.size_bytes = f.size_bytes AND c.modified_at IS f.modified_at AND c.vision = ?1";

const _: () = assert!(FAILURE_STAGE == 4 && FAILURE_RETRY_LIMIT == 3 && FAILURE_STAGE & OWED_MASK == 0);

/// Whether a failure says something about the file. A file that cannot even
/// be opened right now (drive offline, permission, deleted) is not counted:
/// nothing about its size or date changes when that is fixed.
pub fn failure_is_about_the_file(path: &std::path::Path) -> bool {
    use std::io::Read;
    let long = crate::util::path_safety::to_extended_length(path);
    std::fs::File::open(long).and_then(|mut file| file.read(&mut [0u8; 1])).is_ok()
}

// MARK: - RAM++ after the scan

/// A file's catch-up is given up after this many failed attempts
/// (`ScanCatchUp.maxAttempts`); it keeps the tags the scan stored.
pub const MAX_ATTEMPTS: i64 = 3;

/// Whether RAM++ runs after the main pass instead of inside it. On the Mac
/// (the adapters path) that is the default, as in the Swift engine, and
/// `FILEID_RAMPLUS_INLINE=1` is the control. Elsewhere RAM++ is GPU-bound and
/// deferring only changes when tags appear, so it is opt-in with
/// `FILEID_RAMPLUS_DEFERRED=1`.
pub fn ram_plus_deferred(apple_scan: bool) -> bool {
    static MODE: std::sync::OnceLock<(bool, bool)> = std::sync::OnceLock::new();
    let (apple, portable) = *MODE.get_or_init(|| {
        let inline = std::env::var("FILEID_RAMPLUS_INLINE").ok();
        let deferred = std::env::var("FILEID_RAMPLUS_DEFERRED").ok();
        (
            deferred_mode(true, inline.as_deref(), deferred.as_deref()),
            deferred_mode(false, inline.as_deref(), deferred.as_deref()),
        )
    });
    if apple_scan { apple } else { portable }
}

fn deferred_mode(apple_scan: bool, inline: Option<&str>, deferred: Option<&str>) -> bool {
    let on = |value: Option<&str>| {
        value.is_some_and(|v| matches!(v.trim().to_lowercase().as_str(), "1" | "true" | "on" | "yes"))
    };
    !on(inline) && (apple_scan || on(deferred))
}

/// What the scan saw, kept for the later RAM++ pass: the Swift engine's
/// `ScanCatchUp.VisionContext` (`k`, `c`, `u`, `t`), which either engine can
/// read, plus optional keys the Swift decoder ignores.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Context {
    /// The tags the scan stored in place of RAM++'s: the Vision classifier's
    /// on the Mac, the CLIP scene labels elsewhere.
    pub k: Vec<String>,
    /// Classifier confidences the tag agreement reads, as `f32` bit patterns.
    pub c: Vec<Label>,
    /// The classifier answered at all.
    pub u: bool,
    /// Classifier plus RAM++ tags, once known; the careful read needs them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<Vec<String>>,
    /// Face detection ran (the portable people-word rule needs to know).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub f: Option<bool>,
    /// The photo names a camera (the portable text-read rule).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub m: Option<bool>,
    /// The Year and camera tags the scan appended, so the pass appends the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Label {
    pub n: String,
    pub b: u32,
}

/// `TagAgreement.vetoable`, as classifier keys.
const VETO_KEYS: [&str; 3] = ["canoe", "skateboard", "frisbee"];
/// `TagAgreement.vetoBelow`.
const VETO_BELOW: f32 = 0.01;
/// The confidence from which the agreement raises a RAM++ score.
const AGREEMENT_FROM: f32 = 0.30;

impl Context {
    /// Exactly the classifier output `TagAgreement.combine` reads: every label
    /// at 0.30 or above and the vetoable labels at 0.01 or above.
    pub fn from_classifier(tags: &[String], confidences: impl IntoIterator<Item = (String, f32)>) -> Self {
        let mut answered = false;
        let mut c: Vec<Label> = confidences
            .into_iter()
            .inspect(|_| answered = true)
            .filter(|(name, value)| *value >= AGREEMENT_FROM || (*value >= VETO_BELOW && VETO_KEYS.contains(&name.as_str())))
            .map(|(n, value)| Label { n, b: value.to_bits() })
            .collect();
        c.sort_by(|a, b| a.n.cmp(&b.n));
        Self { k: tags.to_vec(), c, u: answered, ..Self::default() }
    }

    /// The confidence table for the agreement. An empty table means "the
    /// classifier did not answer", so an answer with nothing kept still gets
    /// one (meaningless) entry, as in the Swift engine.
    pub fn confidences(&self) -> std::collections::BTreeMap<String, f64> {
        let mut out: std::collections::BTreeMap<String, f64> =
            self.c.iter().map(|label| (label.n.clone(), f64::from(f32::from_bits(label.b)))).collect();
        if self.u && out.is_empty() {
            out.insert(String::new(), 0.0);
        }
        out
    }

    pub fn encoded(&self) -> Option<String> {
        serde_json::to_string(self).ok()
    }

    pub fn decode(text: Option<&str>) -> Option<Self> {
        serde_json::from_str(text?).ok()
    }
}

/// The stored tag list of an image: RAM++'s tags when it produced any, else
/// the fallback (classifier or scene labels), then the Year and camera tags,
/// without repeats and capped. The scan and the catch-up pass both store this.
pub fn assemble_tags(
    ram: Vec<(String, f32)>,
    fallback: Vec<(String, Option<f32>)>,
    extras: impl IntoIterator<Item = String>,
    cap: usize,
    fold: fn(&str) -> String,
) -> Vec<(String, Option<f32>)> {
    let mut tags: Vec<(String, Option<f32>)> =
        if ram.is_empty() { fallback } else { ram.into_iter().map(|(tag, score)| (tag, Some(score))).collect() };
    tags.extend(extras.into_iter().map(|tag| (tag, None)));
    let mut seen = std::collections::HashSet::new();
    tags.retain(|(tag, _)| seen.insert(fold(tag)));
    tags.truncate(cap);
    tags
}

pub fn ascii_fold(tag: &str) -> String {
    tag.to_ascii_lowercase()
}

pub fn unicode_fold(tag: &str) -> String {
    tag.to_lowercase()
}

/// What a text read returned (the Mac adapter's reply).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TextRead {
    pub ran: bool,
    pub text: Option<String>,
    pub handwritten: bool,
}

/// The portable rules (Windows, Linux) once RAM++ has answered for an owed
/// image. `ram` already went through the people-word rule. RAM++'s tags, when
/// it has any, replace the scene labels the scan stored; with none the stored
/// tags stand. The text read was left for now, so it is decided here with the
/// tags the inline scan would have decided it with.
pub fn portable_update(
    file: &Pending,
    context: &Context,
    ram: Vec<(String, f32)>,
    extras: Vec<String>,
    cap: usize,
    text_gate: impl FnOnce(&[&str]) -> bool,
    read_text: impl FnOnce() -> Option<String>,
) -> Update {
    let names: Vec<&str> = if ram.is_empty() {
        context.k.iter().map(String::as_str).collect()
    } else {
        ram.iter().map(|(tag, _)| tag.as_str()).collect()
    };
    let text = text_gate(&names)
        .then(|| TextUpdate { text: read_text().filter(|text| !text.trim().is_empty()), handwritten: false });
    let tags = (!ram.is_empty()).then(|| assemble_tags(ram, Vec::new(), extras, cap, ascii_fold));
    Update { tags, text, stages: file.stages & !TAGS_OWED, vision: file.vision.clone() }
}

/// The Mac rules (`ScanCatchUp.process`, tags phase) once RAM++ has answered:
/// `ram` is the agreed RAM++ output. The text is read again whenever the new
/// tags could change the read (or it was not read, or a careful read is owed);
/// a read that runs replaces the scan's text and settles the careful read.
pub fn classifier_update(
    file: &Pending,
    context: &Context,
    ram: Vec<(String, f32)>,
    extras: Vec<String>,
    read_text: impl FnOnce(&[String]) -> TextRead,
) -> Update {
    let text_tags: Vec<String> = context.k.iter().cloned().chain(ram.iter().map(|(tag, _)| tag.clone())).collect();
    let mut stages = file.stages & !TAGS_OWED;
    let mut text = None;
    if file.stages & TEXT_READ_AT_SCAN == 0 || file.stages & CAREFUL_TEXT_OWED != 0 || !ram.is_empty() {
        let read = read_text(&text_tags);
        if read.ran {
            text = Some(TextUpdate { handwritten: read.handwritten && read.text.is_some(), text: read.text });
            stages = (stages & !CAREFUL_TEXT_OWED) | TEXT_READ_AT_SCAN;
        }
    }
    let fallback = context.k.iter().map(|tag| (tag.clone(), None)).collect();
    let tags = assemble_tags(ram, fallback, extras, 16, unicode_fold);
    let vision = Context { t: Some(text_tags), ..context.clone() }.encoded();
    Update { tags: Some(tags), text, stages, vision }
}

/// The careful-read phase for an image: read with the tags the tag phase
/// left (or the classifier's), and the read is no longer owed.
pub fn careful_update(file: &Pending, context: &Context, read_text: impl FnOnce(&[String]) -> TextRead) -> Update {
    let text_tags = context.t.clone().unwrap_or_else(|| context.k.clone());
    let read = read_text(&text_tags);
    let text = read.ran.then(|| TextUpdate { handwritten: read.handwritten && read.text.is_some(), text: read.text });
    Update { tags: None, text, stages: file.stages & !CAREFUL_TEXT_OWED, vision: file.vision.clone() }
}

/// One file the pass has to visit.
#[derive(Debug, Clone)]
pub struct Pending {
    pub file_id: i64,
    pub path: std::path::PathBuf,
    pub kind: String,
    pub size_bytes: i64,
    pub modified_at: Option<f64>,
    pub created_at: Option<f64>,
    pub camera_model: Option<String>,
    pub stages: i64,
    pub vision: Option<String>,
    pub face_count: i64,
    mark_size: i64,
    mark_modified: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Tags,
    CarefulText,
}

impl Phase {
    fn bit(self) -> i64 {
        match self {
            Phase::Tags => TAGS_OWED,
            Phase::CarefulText => CAREFUL_TEXT_OWED,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TextUpdate {
    pub text: Option<String>,
    pub handwritten: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// The complete stored tag list; `None` leaves the scan's tags in place.
    pub tags: Option<Vec<(String, Option<f32>)>>,
    /// The text read again with the final tags; `None` leaves the text as it is.
    pub text: Option<TextUpdate>,
    /// What the row still says afterwards; nothing owed removes the row.
    pub stages: i64,
    pub vision: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Changed, missing or unreachable: the next scan deals with it.
    Leave,
    /// The row no longer describes the analysis the marker was written for.
    Stale,
    Failed,
    /// The reader went away; stop the pass and keep what is owed.
    Blocked,
    Done(Update),
}

/// Decodes and analyses one owed file. The scan's own decoder and models
/// stand behind it (`tagging::catch_up_analyzer`); tests use a stub.
pub trait Analyzer: Sync {
    /// Whether this phase can run now (RAM++ loaded, careful reader ready).
    fn ready(&self, phase: Phase) -> bool;
    /// Files analysed at once.
    fn parallelism(&self) -> usize;
    fn analyse(&self, file: &Pending, phase: Phase, context: Option<&Context>) -> Outcome;
}

#[derive(Debug, Clone, Copy)]
pub enum Progress {
    Total(u64),
    Advance(u64),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub owed_tags: u64,
    pub owed_careful: u64,
    pub tagged: u64,
    pub text_read: u64,
    pub left: u64,
    pub failed: u64,
    pub cancelled: bool,
}

fn scope(root: Option<&std::path::Path>) -> Option<(String, String)> {
    let lo = crate::scan_session::root_descendant_prefix(root?);
    let hi = crate::scan_session::prefix_upper_bound(&lo)?;
    Some((lo, hi))
}

/// `(tags owed, careful reads owed)` under `root` (everywhere when `None`).
pub fn owed(conn: &rusqlite::Connection, root: Option<&std::path::Path>) -> (u64, u64) {
    let base = "SELECT COALESCE(SUM((c.stages & 1) != 0), 0), COALESCE(SUM((c.stages & 2) != 0), 0) \
                FROM scan_catchup c JOIN files f ON f.id = c.file_id WHERE 1 = 1";
    let row = |r: &rusqlite::Row| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, i64>(1)? as u64));
    match scope(root) {
        Some((lo, hi)) => conn.query_row(
            &format!("{base} AND f.path_text >= ?1 AND f.path_text < ?2"),
            rusqlite::params![lo, hi],
            row,
        ),
        None => conn.query_row(base, [], row),
    }
    .unwrap_or((0, 0))
}

fn same_time(a: Option<f64>, b: Option<f64>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => (a - b).abs() < 0.000_001,
        (None, None) => true,
        _ => false,
    }
}

fn next_chunk(
    conn: &rusqlite::Connection,
    root: Option<&std::path::Path>,
    phase: Phase,
    after: i64,
    limit: usize,
) -> rusqlite::Result<Vec<Pending>> {
    let scope = scope(root);
    let sql = format!(
        "SELECT c.file_id, c.stages, c.size_bytes, c.modified_at, c.vision, c.face_count, \
                f.path_text, f.kind, f.size_bytes, f.modified_at, f.created_at, f.camera_model \
         FROM scan_catchup c JOIN files f ON f.id = c.file_id \
         WHERE c.file_id > ?1 AND (c.stages & ?2) != 0 AND f.failed = 0{} \
         ORDER BY c.file_id LIMIT ?3",
        if scope.is_some() { " AND f.path_text >= ?4 AND f.path_text < ?5" } else { "" }
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let row = |r: &rusqlite::Row| {
        Ok(Pending {
            file_id: r.get(0)?,
            stages: r.get(1)?,
            mark_size: r.get(2)?,
            mark_modified: r.get(3)?,
            vision: r.get(4)?,
            face_count: r.get(5)?,
            path: std::path::PathBuf::from(r.get::<_, String>(6)?),
            kind: r.get(7)?,
            size_bytes: r.get(8)?,
            modified_at: r.get(9)?,
            created_at: r.get(10)?,
            camera_model: r.get(11)?,
        })
    };
    let limit = limit as i64;
    match &scope {
        Some((lo, hi)) => stmt.query_map(rusqlite::params![after, phase.bit(), limit, lo, hi], row)?.collect(),
        None => stmt.query_map(rusqlite::params![after, phase.bit(), limit], row)?.collect(),
    }
}

/// The checks every analyzer shares, then the analyzer itself.
fn process(file: &Pending, phase: Phase, analyzer: &dyn Analyzer) -> Outcome {
    if file.mark_size != file.size_bytes || !same_time(file.mark_modified, file.modified_at) {
        return Outcome::Stale;
    }
    let on_disk = std::fs::metadata(crate::util::path_safety::to_extended_length(&file.path)).ok().map(|meta| {
        (meta.len() as i64, meta.modified().ok().and_then(crate::util::catalog_unix_seconds))
    });
    match on_disk {
        Some((size, modified)) if size == file.size_bytes && same_time(modified, file.modified_at) => {}
        _ => return Outcome::Leave,
    }
    let context = Context::decode(file.vision.as_deref());
    match (file.kind.as_str(), phase) {
        ("pdf", Phase::CarefulText) => analyzer.analyse(file, phase, None),
        ("image", _) if context.is_some() => analyzer.analyse(file, phase, context.as_ref()),
        _ => Outcome::Stale,
    }
}

fn apply(tx: &rusqlite::Connection, file: &Pending, outcome: &Outcome, summary: &mut Summary) -> rusqlite::Result<()> {
    use rusqlite::OptionalExtension;
    // The row or the file changed while it was being analysed: this result
    // describes something else.
    let current: Option<(i64, i64, Option<f64>)> = tx
        .prepare_cached(
            "SELECT c.stages, f.size_bytes, f.modified_at FROM scan_catchup c JOIN files f ON f.id = c.file_id \
             WHERE c.file_id = ?1",
        )?
        .query_row([file.file_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    match current {
        Some((stages, size, modified))
            if stages == file.stages && size == file.size_bytes && same_time(modified, file.modified_at) => {}
        _ => return Ok(()),
    }
    match outcome {
        Outcome::Leave | Outcome::Blocked => summary.left += 1,
        Outcome::Stale => {
            tx.prepare_cached("DELETE FROM scan_catchup WHERE file_id = ?1")?.execute([file.file_id])?;
            summary.left += 1;
        }
        Outcome::Failed => {
            tx.prepare_cached("UPDATE scan_catchup SET attempts = attempts + 1 WHERE file_id = ?1")?.execute([file.file_id])?;
            tx.prepare_cached("DELETE FROM scan_catchup WHERE file_id = ?1 AND attempts >= ?2")?
                .execute(rusqlite::params![file.file_id, MAX_ATTEMPTS])?;
            summary.failed += 1;
        }
        Outcome::Done(update) => {
            if let Some(tags) = &update.tags {
                tx.prepare_cached("DELETE FROM tags WHERE file_id = ?1 AND source = 'auto'")?.execute([file.file_id])?;
                let mut insert = tx
                    .prepare_cached("INSERT OR REPLACE INTO tags (file_id, tag, source, score) VALUES (?1, ?2, 'auto', ?3)")?;
                for (tag, score) in tags {
                    let trimmed = tag.trim();
                    if !trimmed.is_empty() {
                        insert.execute(rusqlite::params![file.file_id, trimmed, score.map(f64::from)])?;
                    }
                }
                summary.tagged += 1;
            }
            if let Some(text) = &update.text {
                tx.prepare_cached("DELETE FROM ocr_text WHERE file_id = ?1 AND source = 'ocr'")?.execute([file.file_id])?;
                if let Some(value) = text.text.as_deref().filter(|value| !value.trim().is_empty()) {
                    tx.prepare_cached(
                        "INSERT INTO ocr_text (file_id, text, handwritten) SELECT ?1, ?2, ?3 \
                         WHERE NOT EXISTS (SELECT 1 FROM ocr_text WHERE file_id = ?1)",
                    )?
                    .execute(rusqlite::params![file.file_id, value, text.handwritten])?;
                }
                tx.prepare_cached("UPDATE files SET has_text = EXISTS(SELECT 1 FROM ocr_text WHERE file_id = ?1) WHERE id = ?1")?
                    .execute([file.file_id])?;
                summary.text_read += 1;
            }
            if update.stages & OWED_MASK == 0 {
                tx.prepare_cached("DELETE FROM scan_catchup WHERE file_id = ?1")?.execute([file.file_id])?;
            } else {
                tx.prepare_cached("UPDATE scan_catchup SET stages = ?2, vision = ?3, attempts = 0 WHERE file_id = ?1")?
                    .execute(rusqlite::params![file.file_id, update.stages, update.vision])?;
            }
        }
    }
    Ok(())
}

/// Runs the tag pass, then the careful-read pass, over what files under
/// `root` (everything when `None`) are owed. The `scan_catchup` row is the
/// cursor: a cancel or a crash leaves the rest for the next scan. Safe to call
/// with nothing owed.
pub fn run(
    conn: &Mutex<rusqlite::Connection>,
    root: Option<&std::path::Path>,
    analyzer: &dyn Analyzer,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    is_paused: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(Progress) + Sync),
) -> Summary {
    let mut summary = Summary::default();
    let (owed_tags, owed_careful) = owed(&conn.lock(), root);
    if owed_tags + owed_careful == 0 {
        return summary;
    }
    summary.owed_tags = owed_tags;
    summary.owed_careful = owed_careful;
    let started = std::time::Instant::now();
    tracing::info!(tags = owed_tags, careful_text = owed_careful, "[CATCH-UP] started");
    progress(Progress::Total(owed_tags + owed_careful));

    let mut visited = 0;
    if owed_tags > 0 {
        if analyzer.ready(Phase::Tags) {
            visited += pass(conn, root, Phase::Tags, analyzer, is_cancelled, is_paused, progress, &mut summary);
        } else {
            tracing::info!("[CATCH-UP] tags skipped: the tagger is not available");
        }
    }
    let careful = owed(&conn.lock(), root).1;
    if careful > 0 && !summary.cancelled && !is_cancelled() {
        progress(Progress::Total(visited + careful));
        if analyzer.ready(Phase::CarefulText) {
            pass(conn, root, Phase::CarefulText, analyzer, is_cancelled, is_paused, progress, &mut summary);
        } else {
            tracing::info!(owed = careful, "[CATCH-UP] careful text skipped: the careful reader is not ready");
        }
    }
    if is_cancelled() {
        summary.cancelled = true;
    }
    let (still_tags, still_careful) = owed(&conn.lock(), root);
    tracing::info!(
        tagged = summary.tagged,
        text_read = summary.text_read,
        left = summary.left,
        failed = summary.failed,
        cancelled = summary.cancelled,
        still_owed_tags = still_tags,
        still_owed_careful_text = still_careful,
        seconds = started.elapsed().as_secs_f64(),
        "[CATCH-UP] finished"
    );
    summary
}

#[allow(clippy::too_many_arguments)]
fn pass(
    conn: &Mutex<rusqlite::Connection>,
    root: Option<&std::path::Path>,
    phase: Phase,
    analyzer: &dyn Analyzer,
    is_cancelled: &(dyn Fn() -> bool + Sync),
    is_paused: &(dyn Fn() -> bool + Sync),
    progress: &(dyn Fn(Progress) + Sync),
    summary: &mut Summary,
) -> u64 {
    let scheduler = crate::resource_scheduler::ResourceScheduler::shared();
    let chunk_size = analyzer.parallelism().clamp(1, 16);
    let mut cursor = 0;
    let mut visited = 0;
    loop {
        while is_paused() && !is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        if is_cancelled() {
            summary.cancelled = true;
            break;
        }
        let chunk = match next_chunk(&conn.lock(), root, phase, cursor, chunk_size) {
            Ok(chunk) => chunk,
            Err(error) => {
                tracing::warn!(%error, "[CATCH-UP] could not read what is owed");
                break;
            }
        };
        let Some(last) = chunk.last() else { break };
        cursor = last.file_id;

        let outcomes: Vec<Outcome> = std::thread::scope(|threads| {
            let workers: Vec<_> = chunk
                .iter()
                .map(|file| {
                    let scheduler = &scheduler;
                    threads.spawn(move || {
                        if is_cancelled() {
                            return Outcome::Leave;
                        }
                        // The same background admission as a scan file: a
                        // search or a resident model goes first.
                        let lease = scheduler.reserve_patient(
                            crate::resource_scheduler::Demand { memory_mb: 128, cpu: 1, io: 0 },
                            crate::resource_scheduler::Priority::Background,
                            is_cancelled,
                            std::time::Duration::from_secs(600),
                        );
                        if lease.is_err() {
                            return Outcome::Leave;
                        }
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| process(file, phase, analyzer)))
                            .unwrap_or(Outcome::Failed)
                    })
                })
                .collect();
            workers.into_iter().map(|worker| worker.join().unwrap_or(Outcome::Failed)).collect()
        });

        let cancelled_midway = is_cancelled();
        let written = (|| -> rusqlite::Result<Summary> {
            let conn = conn.lock();
            let tx = conn.unchecked_transaction()?;
            let mut counts = Summary::default();
            for (file, outcome) in chunk.iter().zip(&outcomes) {
                apply(&tx, file, outcome, &mut counts)?;
            }
            tx.commit()?;
            Ok(counts)
        })();
        match written {
            Ok(counts) => {
                summary.tagged += counts.tagged;
                summary.text_read += counts.text_read;
                summary.left += counts.left;
                summary.failed += counts.failed;
            }
            Err(error) => {
                tracing::warn!(%error, "[CATCH-UP] could not store results; the rest stays owed");
                break;
            }
        }
        if outcomes.iter().any(|outcome| matches!(outcome, Outcome::Blocked)) {
            tracing::info!("[CATCH-UP] paused: the reader stopped answering");
            break;
        }
        if cancelled_midway {
            summary.cancelled = true;
            break;
        }
        visited += chunk.len() as u64;
        progress(Progress::Advance(chunk.len() as u64));
    }
    visited
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordinator::ScanCoordinator;
    use crate::pipeline::dbwriter::DbWriter;
    use crate::pipeline::discovery::{DiscoveredFile, FileKind};
    use crate::pipeline::tagging::TaggedFile;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn ram_plus_is_deferred_by_default_on_the_mac_and_on_request_elsewhere() {
        assert!(deferred_mode(true, None, None));
        assert!(!deferred_mode(false, None, None));
        assert!(deferred_mode(false, None, Some("1")));
        assert!(deferred_mode(false, Some("0"), Some(" Yes ")));
        assert!(!deferred_mode(false, None, Some("0")));
        for inline in ["1", "true", "ON", " yes "] {
            assert!(!deferred_mode(true, Some(inline), None), "{inline}");
            assert!(!deferred_mode(false, Some(inline), Some("1")), "the inline control wins");
        }
        assert!(deferred_mode(true, Some("0"), None));
    }

    #[test]
    fn the_context_reads_and_writes_the_swift_engines_json() {
        // As `ScanCatchUp.VisionContext` encodes it (any key order, no `t` yet).
        let swift = r#"{"u":true,"c":[{"n":"dog","b":1063675494},{"n":"frisbee","b":1017370378}],"k":["dog","outdoor"]}"#;
        let context = Context::decode(Some(swift)).expect("Swift-written context");
        assert_eq!(context.k, ["dog", "outdoor"]);
        assert_eq!(context.x, None);
        let confidences = context.confidences();
        assert!((confidences["dog"] - 0.9).abs() < 1e-6 && (confidences["frisbee"] - 0.02).abs() < 1e-6);

        // What this engine writes decodes with the Swift struct's required keys
        // present and no nulls; the extra keys are ones that decoder ignores.
        let written = Context { x: Some(vec!["Year_2021".into()]), ..context.clone() }.encoded().unwrap();
        let value: serde_json::Value = serde_json::from_str(&written).unwrap();
        let object = value.as_object().unwrap();
        assert!(["k", "c", "u"].iter().all(|key| object.contains_key(*key)));
        assert!(object.values().all(|v| !v.is_null()) && !object.contains_key("t"));
        assert_eq!(object["c"][0], serde_json::json!({"n": "dog", "b": 1_063_675_494u32}));
        assert_eq!(Context::decode(Some(&written)).unwrap().x, Some(vec!["Year_2021".to_string()]));
        assert_eq!(Context::decode(Some("{\"failedBy\":\"x\"}")), None);
        assert_eq!(Context::decode(None), None);
    }

    #[test]
    fn the_context_keeps_exactly_what_the_tag_agreement_reads() {
        let tags = vec!["outdoor".to_string()];
        let context = Context::from_classifier(
            &tags,
            [("tree", 0.31f32), ("park", 0.29), ("canoe", 0.02), ("skateboard", 0.005), ("animal", 0.30), ("frisbee", 0.9)]
                .map(|(name, value)| (name.to_string(), value)),
        );
        let kept: Vec<&str> = context.c.iter().map(|label| label.n.as_str()).collect();
        assert_eq!(kept, ["animal", "canoe", "frisbee", "tree"], "sorted; 0.30 and above, or a vetoable label at 0.01 and above");
        assert_eq!(context.c[1].b, 0.02f32.to_bits(), "confidences round-trip bit for bit");
        assert!(context.u);

        // The classifier answered but nothing was kept: still "answered".
        let quiet = Context::from_classifier(&tags, [("park".to_string(), 0.1f32)]);
        assert!(quiet.u && quiet.c.is_empty());
        assert_eq!(quiet.confidences().len(), 1);
        let silent = Context::from_classifier(&tags, std::iter::empty());
        assert!(!silent.u && silent.confidences().is_empty());
    }

    #[test]
    fn assembled_tags_prefer_ram_plus_then_dedupe_and_cap() {
        let fallback = vec![("outdoor".to_string(), None), ("Dog".to_string(), None)];
        let extras = || vec!["Year_2021".to_string(), "Canon".to_string()];
        let with_ram = assemble_tags(vec![("dog".into(), 0.9), ("DOG".into(), 0.5), ("ball".into(), 0.4)], fallback.clone(), extras(), 16, ascii_fold);
        assert_eq!(
            with_ram,
            vec![("dog".to_string(), Some(0.9)), ("ball".to_string(), Some(0.4)), ("Year_2021".to_string(), None), ("Canon".to_string(), None)]
        );
        let without = assemble_tags(Vec::new(), fallback, extras(), 3, ascii_fold);
        assert_eq!(without.iter().map(|(tag, _)| tag.as_str()).collect::<Vec<_>>(), ["outdoor", "Dog", "Year_2021"]);
        assert_eq!(unicode_fold("ÉCOLE"), "école");
    }

    // MARK: the pass over a real catalog, with a stub in place of the models

    struct Library {
        dir: PathBuf,
        conn: Arc<Mutex<rusqlite::Connection>>,
    }

    impl Library {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("fileid-catch-up-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir, conn: Arc::new(Mutex::new(crate::db::test_support::memory_db())) }
        }

        /// A scanned image row for a real file, before any tags.
        fn image(&self, name: &str) -> TaggedFile {
            let path = self.dir.join(name);
            std::fs::write(&path, name.as_bytes()).unwrap();
            let meta = std::fs::metadata(&path).unwrap();
            let mut row = TaggedFile::bare(&DiscoveredFile {
                path,
                kind: FileKind::Image,
                size_bytes: meta.len(),
                modified_unix: crate::util::catalog_unix_seconds(meta.modified().unwrap()).unwrap(),
                created_unix: None,
                online_only: false,
                file_ref: None,
                moved_from_known_row: false,
            });
            row.tags_evaluated = true;
            row
        }

        async fn write(&self, rows: Vec<TaggedFile>) {
            let (tx, rx) = tokio::sync::mpsc::channel(rows.len().max(1));
            for row in rows {
                tx.send(row).await.unwrap();
            }
            drop(tx);
            DbWriter::new(self.conn.clone(), ScanCoordinator::new()).run(rx, |_| {}).await.unwrap();
        }

        fn run(&self, analyzer: &dyn Analyzer) -> Summary {
            run(&self.conn, Some(&self.dir), analyzer, &|| false, &|| false, &|_| {})
        }

        /// (tags with scores, text, handwritten, has_text, owed stages)
        #[allow(clippy::type_complexity)]
        fn stored(&self, name: &str) -> (Vec<(String, Option<f64>)>, Option<String>, bool, bool, Option<i64>) {
            let conn = self.conn.lock();
            let path = self.dir.join(name).to_string_lossy().into_owned();
            let (id, has_text): (i64, bool) =
                conn.query_row("SELECT id, has_text FROM files WHERE path_text = ?1", [&path], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            let tags = conn
                .prepare("SELECT tag, score FROM tags WHERE file_id = ?1 AND source = 'auto' ORDER BY tag")
                .unwrap()
                .query_map([id], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            use rusqlite::OptionalExtension;
            let text: Option<(String, bool)> = conn
                .query_row("SELECT text, handwritten FROM ocr_text WHERE file_id = ?1", [id], |r| Ok((r.get(0)?, r.get(1)?)))
                .optional()
                .unwrap();
            let owed = conn.query_row("SELECT stages FROM scan_catchup WHERE file_id = ?1", [id], |r| r.get(0)).optional().unwrap();
            let handwritten = text.as_ref().is_some_and(|(_, handwritten)| *handwritten);
            (tags, text.map(|(text, _)| text), handwritten, has_text, owed)
        }
    }

    impl Drop for Library {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn marker(stages: i64, context: Context, face_count: i64) -> Option<Marker> {
        Some(Marker { stages, vision: context.encoded(), face_count })
    }

    fn name_of(file: &Pending) -> String {
        file.path.file_name().unwrap().to_string_lossy().into_owned()
    }

    /// RAM++ output per file name, as the model (after the platform's own
    /// agreement rule) would return it.
    type StubRam = fn(&str) -> Vec<(String, f32)>;

    fn scored(tags: &[(&str, f32)]) -> Vec<(String, f32)> {
        tags.iter().map(|(tag, score)| ((*tag).to_string(), *score)).collect()
    }

    // The portable rules: scene labels at scan time, the text read left for later.

    fn portable_gate(names: &[&str]) -> bool {
        names.contains(&"receipt")
    }

    fn portable_ocr(name: &str) -> Option<String> {
        Some(format!("text of {name}"))
    }

    fn drop_people_words(ram: Vec<(String, f32)>, no_face_found: bool) -> Vec<(String, f32)> {
        ram.into_iter()
            .filter(|(tag, _)| !(no_face_found && crate::models::ram_plus::PEOPLE_WORDS.contains(&tag.as_str())))
            .collect()
    }

    struct PortableStub(StubRam);

    impl Analyzer for PortableStub {
        fn ready(&self, phase: Phase) -> bool {
            phase == Phase::Tags
        }
        fn parallelism(&self) -> usize {
            2
        }
        fn analyse(&self, file: &Pending, _phase: Phase, context: Option<&Context>) -> Outcome {
            let context = context.unwrap();
            let name = name_of(file);
            let ram = drop_people_words((self.0)(&name), context.f == Some(true) && file.face_count == 0);
            let extras = context.x.clone().unwrap_or_default();
            Outcome::Done(portable_update(file, context, ram, extras, 16, portable_gate, || portable_ocr(&name)))
        }
    }

    fn portable_ram(name: &str) -> Vec<(String, f32)> {
        match name {
            "receipt.png" => scored(&[("receipt", 0.9), ("Paper", 0.8), ("man", 0.7), ("paper", 0.6)]),
            "dog.png" => scored(&[("dog", 0.95), ("man", 0.9), ("grass", 0.5)]),
            _ => Vec::new(),
        }
    }

    /// The row as the inline scan writes it: RAM++ in the scan, then the text read.
    fn portable_inline(library: &Library, name: &str, scene: &[(&str, f32)], extras: &[&str]) -> TaggedFile {
        let mut row = library.image(name);
        row.faces_evaluated = true;
        let ram = drop_people_words(portable_ram(name), true);
        let fallback: Vec<(String, Option<f32>)> = if ram.is_empty() {
            scene.iter().map(|(tag, score)| ((*tag).to_string(), Some(*score))).collect()
        } else {
            ram.into_iter().map(|(tag, score)| (tag, Some(score))).collect()
        };
        let names: Vec<&str> = fallback.iter().map(|(tag, _)| tag.as_str()).collect();
        if portable_gate(&names) {
            row.ocr_stage_ran = true;
            row.ocr_text = portable_ocr(name);
            row.has_text = row.ocr_text.is_some();
        }
        row.tags = assemble_tags(Vec::new(), fallback, extras.iter().map(|tag| (*tag).to_string()), 16, ascii_fold);
        row
    }

    /// The row as the deferred scan writes it: scene labels, no text read, a marker.
    fn portable_deferred(library: &Library, name: &str, scene: &[(&str, f32)], extras: &[&str]) -> TaggedFile {
        let mut row = library.image(name);
        row.faces_evaluated = true;
        let extras: Vec<String> = extras.iter().map(|tag| (*tag).to_string()).collect();
        let scene: Vec<(String, Option<f32>)> = scene.iter().map(|(tag, score)| ((*tag).to_string(), Some(*score))).collect();
        row.catch_up = marker(
            TAGS_OWED,
            Context {
                k: scene.iter().map(|(tag, _)| tag.clone()).collect(),
                f: Some(true),
                m: Some(false),
                x: Some(extras.clone()),
                ..Context::default()
            },
            0,
        );
        row.tags = assemble_tags(Vec::new(), scene, extras, 16, ascii_fold);
        row
    }

    #[tokio::test]
    async fn portable_tags_and_text_after_the_pass_equal_the_inline_scan() {
        let cases: [(&str, &[(&str, f32)], &[&str]); 3] = [
            ("receipt.png", &[("indoor", 0.4), ("receipt", 0.2)], &["Year_2021"]),
            ("dog.png", &[("park", 0.5)], &["Year_2019", "Canon"]),
            ("abstract.png", &[("receipt", 0.3), ("pattern", 0.2)], &[]),
        ];
        let (inline, deferred) = (Library::new("portable-inline"), Library::new("portable-deferred"));
        inline.write(cases.iter().map(|(name, scene, extras)| portable_inline(&inline, name, scene, extras)).collect()).await;
        deferred.write(cases.iter().map(|(name, scene, extras)| portable_deferred(&deferred, name, scene, extras)).collect()).await;

        // Before the pass the deferred library holds the scene labels and owes the rest.
        let (tags, text, _, _, owed) = deferred.stored("dog.png");
        assert_eq!(tags.iter().map(|(tag, _)| tag.as_str()).collect::<Vec<_>>(), ["Canon", "Year_2019", "park"]);
        assert_eq!((text, owed), (None, Some(TAGS_OWED)));

        let summary = deferred.run(&PortableStub(portable_ram));
        assert_eq!((summary.owed_tags, summary.tagged, summary.failed, summary.left), (3, 2, 0, 0));
        for (name, _, _) in cases {
            let after = deferred.stored(name);
            assert_eq!(after.4, None, "{name}: nothing is owed afterwards");
            assert_eq!(after, inline.stored(name), "{name}");
        }
        // RAM++ found nothing for the abstract image: the scene labels stand,
        // and the text read was decided with them, as inline.
        assert_eq!(deferred.stored("abstract.png").1.as_deref(), Some("text of abstract.png"));
        assert_eq!(deferred.stored("dog.png").0.iter().map(|(tag, _)| tag.as_str()).collect::<Vec<_>>(), ["Canon", "Year_2019", "dog", "grass"]);
        assert_eq!(deferred.run(&PortableStub(portable_ram)), Summary::default(), "a second pass has nothing to do");
    }

    // The Mac rules: the classifier's tags at scan time, with a first text read.

    fn mac_reader(name: &str, text_tags: &[String]) -> TextRead {
        let ran = text_tags.iter().any(|tag| tag == "document" || tag == "receipt");
        TextRead {
            ran,
            text: ran.then(|| format!("{name} read with {}", text_tags.join("+"))),
            handwritten: text_tags.iter().any(|tag| tag == "handwriting"),
        }
    }

    fn mac_ram(name: &str) -> Vec<(String, f32)> {
        match name {
            "note.heic" => scored(&[("receipt", 0.9), ("handwriting", 0.8), ("Outdoor", 0.7)]),
            "hike.heic" => scored(&[("mountain", 0.97), ("trail", 0.6)]),
            "scan.heic" => scored(&[("paper", 0.5)]),
            _ => Vec::new(),
        }
    }

    struct MacStub;

    impl Analyzer for MacStub {
        fn ready(&self, _phase: Phase) -> bool {
            true
        }
        fn parallelism(&self) -> usize {
            3
        }
        fn analyse(&self, file: &Pending, phase: Phase, context: Option<&Context>) -> Outcome {
            let context = context.unwrap();
            let name = name_of(file);
            Outcome::Done(match phase {
                Phase::Tags => classifier_update(file, context, mac_ram(&name), context.x.clone().unwrap_or_default(), |tags| {
                    mac_reader(&name, tags)
                }),
                Phase::CarefulText => careful_update(file, context, |tags| mac_reader(&name, tags)),
            })
        }
    }

    fn mac_row(library: &Library, name: &str, classify: &[&str], extras: &[&str], deferred: bool) -> TaggedFile {
        let mut row = library.image(name);
        let classify: Vec<String> = classify.iter().map(|tag| (*tag).to_string()).collect();
        let extras: Vec<String> = extras.iter().map(|tag| (*tag).to_string()).collect();
        let ram = if deferred { Vec::new() } else { mac_ram(name) };
        let text_tags: Vec<String> = classify.iter().cloned().chain(ram.iter().map(|(tag, _)| tag.clone())).collect();
        let read = mac_reader(name, &text_tags);
        row.ocr_stage_ran = read.ran;
        row.ocr_handwritten = read.handwritten && read.text.is_some();
        row.has_text = read.text.is_some();
        row.ocr_text = read.text;
        if deferred {
            let context = Context::from_classifier(&classify, [("document".to_string(), 0.8f32)]);
            row.catch_up = marker(
                TAGS_OWED | if read.ran { TEXT_READ_AT_SCAN } else { 0 },
                Context { x: Some(extras.clone()), ..context },
                1,
            );
        }
        let fallback = classify.into_iter().map(|tag| (tag, None)).collect();
        row.tags = assemble_tags(ram, fallback, extras, 16, unicode_fold);
        row
    }

    #[tokio::test]
    async fn mac_tags_and_text_after_the_pass_equal_the_inline_scan() {
        let cases: [(&str, &[&str], &[&str]); 4] = [
            // RAM++ adds a text hint the classifier lacked: the text is read in the pass.
            ("note.heic", &["outdoor"], &["Year_2024", "iPhone"]),
            // No text either way.
            ("hike.heic", &["outdoor", "sky"], &["Year_2023"]),
            // Read at scan time; RAM++'s tags change what the read is told.
            ("scan.heic", &["document"], &[]),
            // RAM++ has nothing to add: the classifier's tags stand.
            ("blank.heic", &["document", "white"], &["Year_2020"]),
        ];
        let (inline, deferred) = (Library::new("mac-inline"), Library::new("mac-deferred"));
        inline.write(cases.iter().map(|(name, classify, extras)| mac_row(&inline, name, classify, extras, false)).collect()).await;
        deferred.write(cases.iter().map(|(name, classify, extras)| mac_row(&deferred, name, classify, extras, true)).collect()).await;
        assert_eq!(deferred.stored("scan.heic").4, Some(TAGS_OWED | TEXT_READ_AT_SCAN));
        assert_eq!(deferred.stored("note.heic").4, Some(TAGS_OWED));

        let summary = deferred.run(&MacStub);
        assert_eq!((summary.owed_tags, summary.tagged, summary.text_read, summary.failed), (4, 4, 2, 0));
        for (name, _, _) in cases {
            let after = deferred.stored(name);
            assert_eq!(after.4, None, "{name}: nothing is owed afterwards");
            assert_eq!(after, inline.stored(name), "{name}");
        }
        let note = deferred.stored("note.heic");
        assert!(note.2 && note.3, "the handwriting tag reached the text read");
        assert_eq!(note.1.as_deref(), Some("note.heic read with outdoor+receipt+handwriting+Outdoor"));
    }

    #[tokio::test]
    async fn a_careful_read_the_swift_engine_left_owed_is_finished_and_its_tags_are_kept() {
        let library = Library::new("careful");
        let mut row = library.image("letter.heic");
        row.tags = vec![("document".to_string(), None), ("Year_2022".to_string(), None)];
        row.ocr_stage_ran = true;
        row.ocr_text = Some("fast read".to_string());
        row.has_text = true;
        // As the Swift scan writes it once RAM++ ran inline: the text tags are known.
        let swift = r#"{"k":["document"],"c":[],"u":true,"t":["document","handwriting"]}"#;
        row.catch_up = Some(Marker { stages: CAREFUL_TEXT_OWED | TEXT_READ_AT_SCAN, vision: Some(swift.to_string()), face_count: 0 });
        library.write(vec![row]).await;

        let summary = library.run(&MacStub);
        assert_eq!((summary.owed_careful, summary.text_read, summary.tagged), (1, 1, 0));
        let (tags, text, handwritten, has_text, owed) = library.stored("letter.heic");
        assert_eq!(tags.iter().map(|(tag, _)| tag.as_str()).collect::<Vec<_>>(), ["Year_2022", "document"]);
        assert_eq!(text.as_deref(), Some("letter.heic read with document+handwriting"));
        assert!(handwritten && has_text);
        assert_eq!(owed, None);
    }

    struct Counting {
        calls: AtomicUsize,
        fail: bool,
    }

    impl Analyzer for Counting {
        fn ready(&self, phase: Phase) -> bool {
            phase == Phase::Tags
        }
        fn parallelism(&self) -> usize {
            2
        }
        fn analyse(&self, file: &Pending, _phase: Phase, context: Option<&Context>) -> Outcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Outcome::Failed;
            }
            Outcome::Done(portable_update(file, context.unwrap(), scored(&[("tagged", 0.9)]), Vec::new(), 16, |_| false, || None))
        }
    }

    fn owing(library: &Library, name: &str) -> TaggedFile {
        let mut row = library.image(name);
        row.tags = vec![("scene".to_string(), Some(0.5))];
        row.catch_up = marker(TAGS_OWED, Context { k: vec!["scene".into()], ..Context::default() }, 0);
        row
    }

    #[tokio::test]
    async fn a_cancelled_pass_keeps_the_rest_owed_and_the_next_one_finishes_it() {
        let library = Library::new("resume");
        library.write((0..7).map(|i| owing(&library, &format!("photo{i}.png"))).collect()).await;
        let analyzer = Counting { calls: AtomicUsize::new(0), fail: false };
        let progress = Mutex::new(Vec::new());
        // Cancelled once the first two files have been analysed.
        let summary = run(
            &library.conn,
            Some(&library.dir),
            &analyzer,
            &|| analyzer.calls.load(Ordering::SeqCst) >= 2,
            &|| false,
            &|event| progress.lock().push(format!("{event:?}")),
        );
        assert!(summary.cancelled);
        assert_eq!(summary.tagged, 2, "what was analysed before the cancel is kept");
        assert_eq!(owed(&library.conn.lock(), Some(&library.dir)), (5, 0));
        assert_eq!(*progress.lock(), ["Total(7)"], "a cancelled chunk is not reported as progress");

        let events = Mutex::new(Vec::new());
        let summary = run(&library.conn, Some(&library.dir), &analyzer, &|| false, &|| false, &|event| events.lock().push(format!("{event:?}")));
        assert_eq!((summary.owed_tags, summary.tagged, summary.cancelled), (5, 5, false));
        assert_eq!(*events.lock(), ["Total(5)", "Advance(2)", "Advance(2)", "Advance(1)"]);
        assert_eq!(owed(&library.conn.lock(), None), (0, 0));
        for i in 0..7 {
            assert_eq!(library.stored(&format!("photo{i}.png")).0, vec![("tagged".to_string(), Some(f64::from(0.9f32)))]);
        }
    }

    #[tokio::test]
    async fn stale_changed_failing_and_out_of_scope_files_are_handled_without_touching_their_tags() {
        let library = Library::new("edge");
        let elsewhere = Library { dir: library.dir.with_file_name(format!("{}-elsewhere", library.dir.file_name().unwrap().to_string_lossy())), conn: library.conn.clone() };
        std::fs::create_dir_all(&elsewhere.dir).unwrap();
        let mut not_an_image = owing(&library, "notes.png");
        not_an_image.kind = FileKind::Doc;
        library
            .write(vec![
                owing(&library, "stale.png"),
                owing(&library, "edited.png"),
                owing(&library, "broken.png"),
                not_an_image,
                owing(&elsewhere, "outside.png"),
            ])
            .await;
        let scene = vec![("scene".to_string(), Some(0.5))];
        // The row was rewritten since the marker (a different size on record).
        library.conn.lock().execute("UPDATE scan_catchup SET size_bytes = size_bytes + 1 WHERE file_id = (SELECT id FROM files WHERE path_text LIKE '%stale.png')", []).unwrap();
        // The file changed on disk after the scan.
        std::fs::write(library.dir.join("edited.png"), b"a longer file than before").unwrap();

        let good = Counting { calls: AtomicUsize::new(0), fail: false };
        let summary = library.run(&good);
        assert_eq!(good.calls.load(Ordering::SeqCst), 1, "only the intact image under the root reaches the models");
        assert_eq!((summary.tagged, summary.left), (1, 3));
        assert_eq!(library.stored("stale.png"), (scene.clone(), None, false, false, None), "a stale marker is dropped");
        assert_eq!(library.stored("notes.png").4, None, "a marker on something that is not an image is dropped");
        assert_eq!(library.stored("edited.png").4, Some(TAGS_OWED), "a changed file waits for the next scan");
        assert_eq!(library.stored("edited.png").0, scene);
        assert_eq!(elsewhere.stored("outside.png").4, Some(TAGS_OWED), "another folder's files are not this scan's");

        // A file the models cannot analyse is given up after three passes and keeps its tags.
        std::fs::remove_file(library.dir.join("edited.png")).unwrap();
        library.write(vec![owing(&library, "broken.png")]).await;
        let failing = Counting { calls: AtomicUsize::new(0), fail: true };
        for attempt in 1..=3 {
            assert_eq!(library.run(&failing).failed, 1);
            let attempts: Option<i64> = {
                use rusqlite::OptionalExtension;
                library.conn.lock().query_row("SELECT attempts FROM scan_catchup c JOIN files f ON f.id = c.file_id WHERE f.path_text LIKE '%broken.png'", [], |r| r.get(0)).optional().unwrap()
            };
            assert_eq!(attempts, (attempt < 3).then_some(attempt));
        }
        assert_eq!(library.stored("broken.png").0, scene);
        assert_eq!(failing.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_pass_without_a_tagger_leaves_everything_owed() {
        struct NotReady;
        impl Analyzer for NotReady {
            fn ready(&self, _phase: Phase) -> bool {
                false
            }
            fn parallelism(&self) -> usize {
                1
            }
            fn analyse(&self, _file: &Pending, _phase: Phase, _context: Option<&Context>) -> Outcome {
                panic!("never analysed");
            }
        }
        let library = Library::new("not-ready");
        library.write(vec![owing(&library, "a.png")]).await;
        assert_eq!(library.run(&NotReady).tagged, 0);
        assert_eq!(library.stored("a.png").4, Some(TAGS_OWED));
    }

    #[test]
    fn failure_context_is_json_naming_this_build() {
        let parsed: serde_json::Value = serde_json::from_str(failure_context()).unwrap();
        let name = parsed["failedBy"].as_str().unwrap();
        assert!(name.starts_with("fileid-engine ") && name.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn an_unopenable_file_does_not_count_as_a_failure_of_the_file() {
        let dir = std::env::temp_dir().join(format!("fileid-failure-memory-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.jpg");
        std::fs::write(&broken, b"not a jpeg").unwrap();
        assert!(failure_is_about_the_file(&broken));
        assert!(!failure_is_about_the_file(&dir.join("missing.jpg")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
