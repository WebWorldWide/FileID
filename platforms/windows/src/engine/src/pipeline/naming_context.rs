use anyhow::{ensure, Result};
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    pub source: &'static str,
}

#[derive(Clone, Debug, PartialEq)]
pub struct NamingContext {
    pub file_id: i64,
    pub path: String,
    pub revision: String,
    pub face_model: Option<String>,
    pub subjects: Vec<String>,
    pub label: Option<Label>,
}

pub fn selected_face_model() -> Option<String> {
    crate::models::sface::default_weights_path().ok()
        .and_then(|path| crate::models::sface::SFace::fingerprint(path).ok())
}

impl NamingContext {
    pub fn load(conn: &Connection, file_id: i64, face_model: Option<&str>) -> Result<Self> {
        let (path, size, modified): (String, i64, Option<f64>) = conn.query_row(
            "SELECT path_text,size_bytes,modified_at FROM files WHERE id=?1", [file_id],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))?;
        let revision = format!("{size}:{}",modified.map_or_else(|| "unknown".into(),|value| value.to_bits().to_string()));
        let mut statement = conn.prepare("SELECT title FROM catalog_chapters WHERE file_id=?1 AND user_edited=1 AND stale=0 AND source_revision=?2 ORDER BY id")?;
        let chapters = usable_labels(statement.query_map(params![file_id,revision],|row| row.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?);
        let mut statement = conn.prepare("SELECT e.title FROM catalog_events e JOIN catalog_event_files ef ON ef.event_id=e.id JOIN catalog_take_scores s ON s.event_id=e.id AND s.file_id=ef.file_id WHERE ef.file_id=?1 AND e.user_edited=1 AND s.model_version='user' AND s.stale=0 AND s.source_revision=?2 AND s.confidence=1 ORDER BY e.id")?;
        let events = usable_labels(statement.query_map(params![file_id,revision],|row| row.get(0))?.collect::<rusqlite::Result<Vec<String>>>()?);
        let valid_stat = size >= 0 && modified.is_some_and(f64::is_finite);
        let label = if !valid_stat { None } else if chapters.len() == 1 { Some(Label { text: chapters[0].clone(), source: "chapter" }) }
            else if events.len() == 1 { Some(Label { text: events[0].clone(), source: "event" }) } else { None };
        let mut subjects = BTreeSet::new();
        if let Some(model) = face_model.filter(|model| !model.is_empty() && valid_stat) {
            let mut statement = conn.prepare("SELECT p.title,p.first_name,p.name,fp.arcface_embedding FROM face_prints fp JOIN persons p ON p.id=fp.person_id JOIN files f ON f.id=fp.file_id JOIN catalog_revisions r ON r.file_id=f.id WHERE fp.file_id=?1 AND fp.excluded=0 AND f.failed=0 AND IFNULL(p.is_unknown,0)=0 AND fp.embedding_model=?2 AND fp.processing_version=?3 AND fp.source_revision=?4 AND r.revision=?4 ORDER BY fp.id")?;
            let rows = statement.query_map(params![file_id,model,crate::pipeline::face_analysis_cache::PROCESSING_VERSION,revision],|row| {
                Ok((row.get::<_,Option<String>>(0)?,row.get::<_,Option<String>>(1)?,row.get::<_,Option<String>>(2)?,row.get::<_,Option<Vec<u8>>>(3)?))
            })?;
            for row in rows {
                let (title,first,legacy,embedding) = row?;
                if !embedding.is_some_and(|blob| crate::pipeline::face_analysis_cache::normalized(&blob)) { continue; }
                let title = title.as_deref().unwrap_or("").trim();
                let first = first.as_deref().unwrap_or("").trim();
                let legacy = legacy.as_deref().unwrap_or("").trim();
                let name = if !title.is_empty() && !first.is_empty() { format!("{title} {first}") }
                    else if !first.is_empty() { first.into() } else if !title.is_empty() { title.into() } else { legacy.into() };
                if !name.is_empty() { subjects.insert(name); }
            }
        }
        Ok(Self { file_id,path,revision,face_model:face_model.map(str::to_owned),subjects:subjects.into_iter().collect(),label })
    }

    pub fn proposed_name(&self) -> Option<String> {
        let label = self.label.as_ref()?;
        let proposed = crate::util::smart_name::readable_stem(&label.text,&self.subjects)?;
        let original = std::path::Path::new(&self.path).file_stem()?.to_string_lossy();
        let normalize = |value: &str| value.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect::<String>();
        (normalize(&proposed) != normalize(&original)).then_some(proposed)
    }

    pub fn require_current(&self, conn: &Connection) -> Result<()> {
        ensure!(Self::load(conn,self.file_id,self.face_model.as_deref())? == *self,"Naming evidence or source changed during analysis");
        Ok(())
    }
}

fn usable_labels(values: Vec<String>) -> Vec<String> {
    let generic = ["untitled","chapter","moment","marker","event","file","photo","image","video","document"];
    let mut unique = BTreeMap::new();
    for value in values {
        let text = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let words: Vec<_> = text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).collect();
        if text.is_empty() || text.chars().count() > 60 || text.len() > 200 || words.len() > 8
            || !words.iter().any(|word| word.chars().any(char::is_alphabetic) && !generic.contains(&word.to_lowercase().as_str())) { continue; }
        unique.entry(text.to_lowercase()).or_insert(text);
    }
    unique.into_values().collect()
}
