//! Reviewed evidence a smart name may use, read live and validated again at
//! commit. Anything stale, ignored, unknown, malformed or incompatible is left out,
//! and a source that changed on disk fails the load so the namer abstains.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Label {
    pub text: String,
    pub source: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NamingContext {
    pub file_id: i64,
    pub path: String,
    pub revision: String,
    pub face_model: Option<String>,
    pub processing: String,
    pub subjects: Vec<String>,
    pub label: Option<Label>,
}

#[derive(Debug)]
pub(crate) struct SourceChanged;

impl std::fmt::Display for SourceChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("A file changed or can't be reached")
    }
}

impl std::error::Error for SourceChanged {}

#[derive(Debug)]
pub(crate) struct EvidenceChanged;

impl std::fmt::Display for EvidenceChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Naming evidence changed before the name was saved")
    }
}

impl std::error::Error for EvidenceChanged {}

pub(crate) fn source_revision(size: i64, modified: Option<f64>) -> String {
    format!("{size}:{}", modified.map(|m| m.to_bits().to_string()).unwrap_or_else(|| "unknown".into()))
}

fn live_revision(path: &str) -> Option<String> {
    let metadata = std::fs::metadata(Path::new(path)).ok()?;
    if !metadata.is_file() {
        return None;
    }
    let modified = crate::util::catalog_unix_seconds(metadata.modified().ok()?)?;
    let size = i64::try_from(metadata.len()).ok()?;
    (size >= 0 && modified.is_finite()).then(|| source_revision(size, Some(modified)))
}

fn is_current(
    embedding: Option<&[u8]>,
    model: Option<&str>,
    processing: Option<&str>,
    source: Option<&str>,
    catalog: Option<&str>,
    size: Option<i64>,
    modified: Option<f64>,
    selected_model: &str,
    selected_processing: &str,
) -> bool {
    let (Some(size), Some(modified), Some(source), Some(embedding)) = (size, modified, source, embedding) else {
        return false;
    };
    !selected_model.is_empty()
        && selected_model.chars().count() <= 150
        && !selected_processing.is_empty()
        && model == Some(selected_model)
        && processing == Some(selected_processing)
        && size >= 0
        && modified.is_finite()
        && Some(source) == catalog
        && source == source_revision(size, Some(modified))
        && crate::commands::catalog_names::normalized(embedding)
}

fn placeholder(name: &str) -> bool {
    let lower = name.to_lowercase();
    let Some(rest) = lower.strip_prefix("person").or_else(|| lower.strip_prefix("unknown")) else {
        return false;
    };
    if rest.is_empty() {
        return true;
    }
    let digits = rest.trim_start_matches([' ', '_', '-']);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

const GENERIC: [&str; 10] = ["untitled", "chapter", "moment", "marker", "event", "file", "photo", "image", "video", "document"];

fn usable_labels(values: &[String]) -> Vec<String> {
    let mut unique: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for value in values {
        let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let words: Vec<&str> = text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
        let usable = !text.is_empty()
            && text.chars().count() <= 60
            && text.len() <= 200
            && words.len() <= 8
            && words.iter().any(|w| w.chars().any(char::is_alphabetic) && !GENERIC.contains(&w.to_lowercase().as_str()));
        if usable {
            unique.entry(text.to_lowercase()).or_insert(text);
        }
    }
    let mut labels: Vec<String> = unique.into_values().collect();
    labels.sort();
    labels
}

fn titles(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> rusqlite::Result<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(args, |r| r.get::<_, String>(0))?;
    rows.collect()
}

pub(crate) fn load(
    conn: &Connection,
    file_id: i64,
    face_model: Option<&str>,
    processing: &str,
) -> anyhow::Result<NamingContext> {
    let row: Option<(String, i64, Option<f64>)> = conn
        .query_row("SELECT path_text,size_bytes,modified_at FROM files WHERE id=?1", [file_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .optional()?;
    let Some((path, size, modified)) = row else { return Err(SourceChanged.into()) };
    let revision = source_revision(size, modified);
    let valid_stat = size >= 0
        && modified.is_some_and(f64::is_finite)
        && live_revision(&path).is_some_and(|live| crate::util::same_source_revision(&live, &revision));
    if !valid_stat {
        return Err(SourceChanged.into());
    }
    let chapters = usable_labels(&titles(
        conn,
        "SELECT title FROM catalog_chapters WHERE file_id=?1 AND user_edited=1 AND stale=0 AND source_revision=?2 ORDER BY id",
        params![file_id, revision],
    )?);
    let events = usable_labels(&titles(
        conn,
        "SELECT e.title FROM catalog_events e JOIN catalog_event_files ef ON ef.event_id=e.id \
         JOIN catalog_take_scores s ON s.event_id=e.id AND s.file_id=ef.file_id \
         WHERE ef.file_id=?1 AND e.user_edited=1 AND s.model_version='user' AND s.stale=0 AND s.source_revision=?2 AND s.confidence=1 \
         ORDER BY e.id",
        params![file_id, revision],
    )?);
    let label = match (chapters.as_slice(), events.as_slice()) {
        ([one], _) => Some(Label { text: one.clone(), source: "chapter" }),
        ([], [one]) => Some(Label { text: one.clone(), source: "event" }),
        _ => None,
    };
    let mut subjects = BTreeSet::new();
    if let Some(model) = face_model.filter(|m| !m.is_empty()) {
        let mut statement = conn.prepare(
            "SELECT p.title,p.first_name,p.name,fp.arcface_embedding,fp.embedding_model,fp.processing_version,fp.source_revision,r.revision,f.size_bytes,f.modified_at \
             FROM face_prints fp JOIN persons p ON p.id=fp.person_id \
             JOIN files f ON f.id=fp.file_id JOIN catalog_revisions r ON r.file_id=f.id \
             WHERE fp.file_id=?1 AND COALESCE(fp.excluded,0)=0 AND COALESCE(f.failed,0)=0 AND IFNULL(p.is_unknown,0)=0 \
               AND fp.embedding_model=?2 AND fp.processing_version=?3 AND fp.source_revision=?4 AND r.revision=?4 \
             ORDER BY fp.id",
        )?;
        let mut rows = statement.query(params![file_id, model, processing, revision])?;
        while let Some(row) = rows.next()? {
            let embedding: Option<Vec<u8>> = row.get(3)?;
            let current = is_current(
                embedding.as_deref(),
                row.get::<_, Option<String>>(4)?.as_deref(),
                row.get::<_, Option<String>>(5)?.as_deref(),
                row.get::<_, Option<String>>(6)?.as_deref(),
                row.get::<_, Option<String>>(7)?.as_deref(),
                row.get(8)?,
                row.get(9)?,
                model,
                processing,
            );
            if !current {
                continue;
            }
            let part = |index: usize| -> rusqlite::Result<String> {
                Ok(row.get::<_, Option<String>>(index)?.unwrap_or_default().trim().to_string())
            };
            let (title, first, legacy) = (part(0)?, part(1)?, part(2)?);
            let name = if !title.is_empty() && !first.is_empty() {
                format!("{title} {first}")
            } else if !first.is_empty() {
                first
            } else if !title.is_empty() {
                title
            } else {
                legacy
            };
            if !name.is_empty() && !placeholder(&name) {
                subjects.insert(name);
            }
        }
    }
    Ok(NamingContext {
        file_id,
        path,
        revision,
        face_model: face_model.map(str::to_owned),
        processing: processing.to_owned(),
        subjects: subjects.into_iter().collect(),
        label,
    })
}

impl NamingContext {
    pub(crate) fn proposed_name(&self) -> Option<String> {
        let label = self.label.as_ref()?;
        let stem = crate::util::smart_name::readable_stem(&label.text, &self.subjects)?;
        let original = Path::new(&self.path).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let key = |value: &str| value.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
        (key(&stem) != key(&original)).then_some(stem)
    }

    pub(crate) fn validate(&self, conn: &Connection) -> anyhow::Result<()> {
        match load(conn, self.file_id, self.face_model.as_deref(), &self.processing) {
            Ok(current) if current == *self => Ok(()),
            _ => Err(EvidenceChanged.into()),
        }
    }
}

/// Organize Folders: a reviewed label's name replaces the stored model name; any load failure or abstention keeps it.
pub(crate) fn apply_reviewed_names(conn: &Connection, files: &mut [crate::pipeline::restructure::FileForClassify]) {
    let labelled = match labelled_files(conn, files) {
        Ok(ids) if !ids.is_empty() => ids,
        _ => return,
    };
    let (model, processing) = selected_space();
    for file in files.iter_mut().filter(|file| labelled.contains(&file.file_id)) {
        if let Some(name) = load(conn, file.file_id, model.as_deref(), processing).ok().and_then(|c| c.proposed_name()) {
            file.vlm_proposed = Some(name);
        }
    }
}

/// `load` stats the file and runs several queries, so a plan over a large library asks it only about files that can have a reviewed label at all; this set is wider than `load` accepts.
fn labelled_files(
    conn: &Connection,
    files: &[crate::pipeline::restructure::FileForClassify],
) -> rusqlite::Result<std::collections::HashSet<i64>> {
    let ids = || files.iter().map(|file| file.file_id);
    let (Some(first), Some(last)) = (ids().min(), ids().max()) else { return Ok(std::collections::HashSet::new()) };
    conn.prepare_cached(
        "SELECT file_id FROM catalog_chapters WHERE user_edited=1 AND file_id BETWEEN ?1 AND ?2
         UNION
         SELECT ef.file_id FROM catalog_event_files ef JOIN catalog_events e ON e.id=ef.event_id
         WHERE e.user_edited=1 AND ef.file_id BETWEEN ?1 AND ?2",
    )?
    .query_map(params![first, last], |row| row.get(0))?
    .collect()
}

pub(crate) fn selected_space() -> (Option<String>, &'static str) {
    (crate::commands::face_refresh::current_model(), crate::commands::face_refresh::processing())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_support::memory_db;

    const PROCESSING: &str = "fixture-processing";

    struct Fixture {
        conn: Connection,
        dir: std::path::PathBuf,
        source: std::path::PathBuf,
        revision: String,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn unit_vector() -> Vec<u8> {
        let mut blob = 1.0f32.to_le_bytes().to_vec();
        blob.resize(512, 0);
        blob
    }

    fn fixture(name: &str) -> Fixture {
        let dir = std::env::temp_dir().join(format!("fileid-naming-context-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("IMG_001.jpg");
        std::fs::write(&source, [1u8]).unwrap();
        let live = live_revision(source.to_str().unwrap()).unwrap();
        let (size, bits) = live.split_once(':').unwrap();
        let modified = f64::from_bits(bits.parse().unwrap());
        let conn = memory_db();
        conn.execute(
            "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,kind,extension,scanned_at) VALUES(1,?1,1,?2,?3,'image','jpg',1)",
            params![source.to_string_lossy(), size.parse::<i64>().unwrap(), modified],
        )
        .unwrap();
        conn.execute("INSERT INTO catalog_revisions VALUES(1,?1,'file-stat-v1',1)", [&live]).unwrap();
        Fixture { conn, dir, source, revision: live }
    }

    fn chapter(f: &Fixture, title: &str, id: &str, edited: bool, stale: bool) {
        f.conn
            .execute(
                "INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,source_revision,model_version,confidence,user_edited,stale) VALUES(?1,1,0,1,?2,?3,'user',1,?4,?5)",
                params![id, title, f.revision, edited, stale],
            )
            .unwrap();
    }

    fn person(f: &Fixture, first: &str) {
        f.conn.execute("INSERT INTO persons(id,first_name,created_at) VALUES(1,?1,1)", [first]).unwrap();
        f.conn
            .execute(
                "INSERT INTO face_prints(file_id,person_id,print_data,bbox,arcface_embedding,embedding_model,processing_version,source_revision) VALUES(1,1,X'00','[0.1,0.1,0.3,0.3]',?1,'fixture',?2,?3)",
                params![unit_vector(), PROCESSING, f.revision],
            )
            .unwrap();
    }

    fn context(f: &Fixture, model: Option<&str>) -> anyhow::Result<NamingContext> {
        load(&f.conn, 1, model, PROCESSING)
    }

    #[test]
    fn reviewed_title_produces_a_short_specific_name() {
        let f = fixture("title");
        chapter(&f, "Maya hits a home run", "chapter", true, false);
        let name = context(&f, None).unwrap().proposed_name().unwrap();
        assert!(name.to_lowercase().contains("maya") && name.to_lowercase().contains("home"));
        assert!(name.chars().count() <= 80 && !name.contains(".jpg"));
    }

    #[test]
    fn reviewed_event_with_a_user_score_names_the_file() {
        let f = fixture("event");
        f.conn.execute("INSERT INTO catalog_events(id,title,goal,user_edited) VALUES('event','Baseball game','',1)", []).unwrap();
        f.conn.execute("INSERT INTO catalog_event_files VALUES('event',1)", []).unwrap();
        f.conn
            .execute(
                "INSERT INTO catalog_take_scores(event_id,file_id,confidence,explanation,source_revision,model_version) VALUES('event',1,1,'',?1,'user')",
                [&f.revision],
            )
            .unwrap();
        let context = context(&f, None).unwrap();
        assert_eq!(context.label.as_ref().map(|l| l.source), Some("event"));
        assert_eq!(context.proposed_name().as_deref(), Some("Baseball Game"));
    }

    #[test]
    fn ambiguous_or_unreviewed_evidence_abstains() {
        for fault in ["draft", "stale", "generic", "conflict", "goal", "event-without-score", "event-stale-score"] {
            let f = fixture(fault);
            match fault {
                "draft" => chapter(&f, "Maya hits a home run", "chapter", false, false),
                "stale" => chapter(&f, "Maya hits a home run", "chapter", true, true),
                "generic" => chapter(&f, "Video", "chapter", true, false),
                "conflict" => {
                    chapter(&f, "Home run", "chapter", true, false);
                    chapter(&f, "Birthday gift", "other", true, false);
                }
                "goal" => {
                    f.conn.execute("INSERT INTO catalog_events(id,title,goal,user_edited) VALUES('event','Baseball','gets a hit',1)", []).unwrap();
                    f.conn.execute("INSERT INTO catalog_event_files VALUES('event',1)", []).unwrap();
                }
                _ => {
                    f.conn.execute("INSERT INTO catalog_events(id,title,goal,user_edited) VALUES('event','Baseball game','',1)", []).unwrap();
                    f.conn.execute("INSERT INTO catalog_event_files VALUES('event',1)", []).unwrap();
                    if fault == "event-stale-score" {
                        f.conn
                            .execute(
                                "INSERT INTO catalog_take_scores(event_id,file_id,confidence,explanation,source_revision,model_version,stale) VALUES('event',1,1,'',?1,'user',1)",
                                [&f.revision],
                            )
                            .unwrap();
                    }
                }
            }
            assert_eq!(context(&f, None).unwrap().proposed_name(), None, "{fault}");
        }
    }

    #[test]
    fn organize_folders_prefers_the_reviewed_name_and_keeps_the_stored_one_otherwise() {
        let f = fixture("restructure");
        chapter(&f, "Maya hits a home run", "chapter", true, false);
        let file = |stored: Option<&str>| crate::pipeline::restructure::FileForClassify {
            file_id: 1,
            source: f.source.clone(),
            kind: crate::pipeline::discovery::FileKind::Image,
            modified_unix: 0.0,
            created_unix: None,
            person_name: None,
            location_lat: None,
            location_lon: None,
            has_text: false,
            vlm_proposed: stored.map(str::to_owned),
            identity: None,
        };
        let mut files = [file(Some("sunset-over-the-bay")), file(None)];
        let reviewed = context(&f, None).unwrap().proposed_name();
        apply_reviewed_names(&f.conn, &mut files);
        assert!(reviewed.is_some());
        assert_eq!(files[0].vlm_proposed, reviewed);
        assert_eq!(files[1].vlm_proposed, reviewed);

        f.conn.execute("UPDATE catalog_chapters SET user_edited=0", []).unwrap();
        let mut files = [file(Some("sunset-over-the-bay"))];
        apply_reviewed_names(&f.conn, &mut files);
        assert_eq!(files[0].vlm_proposed.as_deref(), Some("sunset-over-the-bay"));

        chapter(&f, "Birthday gift", "other", true, false);
        std::fs::write(&f.source, [1u8, 2]).unwrap();
        let mut files = [file(Some("sunset-over-the-bay"))];
        apply_reviewed_names(&f.conn, &mut files);
        assert_eq!(files[0].vlm_proposed.as_deref(), Some("sunset-over-the-bay"), "a changed source keeps the stored name");
    }

    #[test]
    fn a_label_that_only_repeats_the_file_name_abstains() {
        let f = fixture("same-name");
        chapter(&f, "IMG 001", "chapter", true, false);
        assert_eq!(context(&f, None).unwrap().proposed_name(), None);
    }

    #[test]
    fn late_evidence_changes_fail_validation() {
        for fault in ["source", "label", "person", "model-space"] {
            let f = fixture(&format!("late-{fault}"));
            chapter(&f, "Home run", "chapter", true, false);
            person(&f, "Maya");
            let held = context(&f, Some("fixture")).unwrap();
            assert_eq!(held.subjects, ["Maya"]);
            held.validate(&f.conn).unwrap();
            match fault {
                "source" => std::fs::write(&f.source, [1u8, 2]).unwrap(),
                "label" => {
                    f.conn.execute("UPDATE catalog_chapters SET title='Birthday gift'", []).unwrap();
                }
                "person" => {
                    f.conn.execute("UPDATE persons SET first_name='Alex'", []).unwrap();
                }
                _ => {
                    f.conn.execute("UPDATE face_prints SET embedding_model='other'", []).unwrap();
                }
            }
            let error = held.validate(&f.conn).unwrap_err();
            assert!(error.is::<EvidenceChanged>(), "{fault}");
        }
    }

    #[test]
    fn a_catalog_that_stored_the_other_engines_rounding_of_the_time_still_loads() {
        let mut f = fixture("rounding");
        let (size, bits) = f.revision.split_once(':').unwrap();
        let stored = f64::from_bits(bits.parse::<u64>().unwrap() + 1);
        f.revision = format!("{size}:{}", stored.to_bits());
        f.conn.execute("UPDATE files SET modified_at=?1", [stored]).unwrap();
        f.conn.execute("UPDATE catalog_revisions SET revision=?1", [&f.revision]).unwrap();
        chapter(&f, "Maya hits a home run", "chapter", true, false);
        assert!(context(&f, None).unwrap().proposed_name().is_some());
    }

    #[test]
    fn a_changed_or_missing_source_fails_the_load() {
        let f = fixture("source");
        std::fs::write(&f.source, [1u8, 2]).unwrap();
        assert!(context(&f, None).unwrap_err().is::<SourceChanged>());
        std::fs::remove_file(&f.source).unwrap();
        assert!(context(&f, None).unwrap_err().is::<SourceChanged>());
        assert!(load(&f.conn, 99, None, PROCESSING).unwrap_err().is::<SourceChanged>());
    }

    #[test]
    fn a_missing_or_malformed_stored_date_fails_the_load() {
        let f = fixture("stat");
        f.conn.execute("UPDATE files SET modified_at=NULL", []).unwrap();
        assert!(context(&f, None).unwrap_err().is::<SourceChanged>());
        let f = fixture("negative-size");
        f.conn.execute("UPDATE files SET size_bytes=-1", []).unwrap();
        assert!(context(&f, None).unwrap_err().is::<SourceChanged>());
    }

    #[test]
    fn valid_identity_evidence_is_used() {
        let f = fixture("valid");
        chapter(&f, "Home run", "chapter", true, false);
        person(&f, "Maya");
        let context = context(&f, Some("fixture")).unwrap();
        assert_eq!(context.subjects, ["Maya"]);
        assert_eq!(context.proposed_name().as_deref(), Some("Maya - Home Run"));
    }

    #[test]
    fn identity_without_a_selected_model_is_not_used() {
        let f = fixture("no-model");
        person(&f, "Maya");
        assert!(context(&f, None).unwrap().subjects.is_empty());
        assert!(context(&f, Some("")).unwrap().subjects.is_empty());
    }

    #[test]
    fn invalid_identity_hints_are_omitted() {
        let faults = [
            ("excluded", "UPDATE face_prints SET excluded=1"),
            ("unknown", "UPDATE persons SET is_unknown=1"),
            ("model", "UPDATE face_prints SET embedding_model='other'"),
            ("processing", "UPDATE face_prints SET processing_version='rust-align112-v1'"),
            ("revision", "UPDATE face_prints SET source_revision='stale'"),
            ("catalog-revision", "UPDATE catalog_revisions SET revision='stale'"),
            ("no-catalog-revision", "DELETE FROM catalog_revisions"),
            ("failed", "UPDATE files SET failed=1"),
            ("short-vector", "UPDATE face_prints SET arcface_embedding=X'0000803f'"),
            ("zero-vector", "UPDATE face_prints SET arcface_embedding=zeroblob(512)"),
            ("unnormalized", "UPDATE face_prints SET arcface_embedding=X'0000004000000000'"),
            ("no-vector", "UPDATE face_prints SET arcface_embedding=NULL"),
            ("generic", "UPDATE persons SET first_name='Person 12'"),
            ("generic-bare", "UPDATE persons SET first_name='unknown'"),
            ("empty", "UPDATE persons SET first_name='  '"),
        ];
        for (fault, sql) in faults {
            let f = fixture(&format!("hint-{fault}"));
            person(&f, "Maya");
            f.conn.execute(sql, []).unwrap();
            assert!(context(&f, Some("fixture")).unwrap().subjects.is_empty(), "{fault}");
        }
    }

    #[test]
    fn nonfinite_vectors_are_omitted() {
        let f = fixture("nan");
        person(&f, "Maya");
        let mut blob = f32::NAN.to_le_bytes().to_vec();
        blob.resize(512, 0);
        f.conn.execute("UPDATE face_prints SET arcface_embedding=?1", [blob]).unwrap();
        assert!(context(&f, Some("fixture")).unwrap().subjects.is_empty());
    }

    #[test]
    fn placeholder_names_match_the_swift_pattern() {
        for name in ["Person", "person 12", "Unknown_3", "UNKNOWN-7", "person12"] {
            assert!(placeholder(name), "{name}");
        }
        for name in ["Personal", "Person-", "Maya", "person 1 2", "Unknown Soldier"] {
            assert!(!placeholder(name), "{name}");
        }
    }

    #[test]
    fn title_and_first_name_form_the_subject() {
        let f = fixture("title-name");
        person(&f, "Maya");
        f.conn.execute("UPDATE persons SET title='Dr'", []).unwrap();
        assert_eq!(context(&f, Some("fixture")).unwrap().subjects, ["Dr Maya"]);
    }
}
