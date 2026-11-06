//! `proposeNames`: preview smart names in a saved style from stored catalog
//! evidence only. No model call, no file or row change. Lockstep with macOS
//! `CatalogNameProposals.swift`.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{bail, Result};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ipc::{CatalogHit, CatalogRequest};
use crate::util::smart_name_style::{self as style, Evidence, Options, Style};

const MAX_FILES: usize = 1000;

pub(crate) fn propose(conn: &Connection, request: &CatalogRequest) -> Result<Vec<CatalogHit>> {
    let invalid = || anyhow::anyhow!("Invalid naming request");
    let naming = request.naming.as_ref().ok_or_else(invalid)?;
    let style_kind = Style::parse(&naming.style).ok_or_else(invalid)?;
    let max_length = naming.max_length as usize;
    let ids = request.file_ids.as_deref().unwrap_or(&[]);
    if !(style::MIN_LENGTH..=style::MAX_LENGTH).contains(&max_length) || ids.is_empty() || ids.len() > MAX_FILES {
        bail!(invalid());
    }
    let options = Options {
        style: style_kind,
        max_length,
        people: naming.include_people,
        event: naming.include_event,
        date: naming.include_date,
    };
    let mut person_words = person_words(conn)?;
    let mut listings: HashMap<String, HashSet<String>> = HashMap::new();
    let mut claimed: HashMap<String, HashSet<String>> = HashMap::new();
    let mut seen = HashSet::new();
    let mut hits = Vec::new();
    for &id in ids {
        if !seen.insert(id) {
            continue;
        }
        let Some(mut evidence) = load(conn, id)? else { continue };
        let path = evidence.path.clone();
        evidence.evidence.person_words = std::mem::take(&mut person_words);
        let outcome = style::propose(&evidence.evidence, &options);
        person_words = std::mem::take(&mut evidence.evidence.person_words);
        let (text, reason) = match outcome.stem {
            Some(stem) => {
                let parent = Path::new(&path).parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
                let own = style::lower(&evidence.name);
                let taken = listings.entry(parent.clone()).or_insert_with(|| listing(&parent));
                let batch = claimed.entry(parent).or_default();
                let free = (1..=99).map(|bump| {
                    let stem = if bump == 1 { stem.clone() } else { format!("{stem}_{bump}") };
                    if evidence.extension.is_empty() { stem } else { format!("{stem}.{}", evidence.extension) }
                }).find(|candidate| {
                    let key = style::lower(candidate);
                    key == own || (!taken.contains(&key) && !batch.contains(&key))
                });
                match free {
                    Some(name) if style::lower(&name) == own => (String::new(), "unchanged"),
                    Some(name) => {
                        batch.insert(style::lower(&name));
                        (name, outcome.reason)
                    }
                    None => (String::new(), "collision"),
                }
            }
            None => (String::new(), outcome.reason),
        };
        hits.push(CatalogHit {
            file_id: id,
            path,
            kind: "proposedName".into(),
            text,
            evidence_id: Some(reason.into()),
            start_seconds: None,
            page: None,
        });
    }
    Ok(hits)
}

fn listing(directory: &str) -> HashSet<String> {
    std::fs::read_dir(directory)
        .map(|entries| entries.flatten().map(|e| style::lower(&e.file_name().to_string_lossy())).collect())
        .unwrap_or_default()
}

fn person_words(conn: &Connection) -> Result<HashSet<String>> {
    let mut words = HashSet::new();
    let mut statement = conn.prepare("SELECT title,first_name,middle_name,last_name,suffix,name FROM persons")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        for index in 0..6 {
            if let Some(part) = row.get::<_, Option<String>>(index)? {
                style::add_person_words(&mut words, &part);
            }
        }
    }
    Ok(words)
}

struct Loaded {
    path: String,
    name: String,
    extension: String,
    evidence: Evidence,
}

fn load(conn: &Connection, id: i64) -> Result<Option<Loaded>> {
    type FileRow = (String, i64, Option<f64>, Option<f64>, i64, Option<String>, Option<String>);
    let row: Option<FileRow> = conn
        .query_row(
            "SELECT path_text,size_bytes,modified_at,created_at,COALESCE(failed,0),vlm_description,vlm_proposed_name FROM files WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?)),
        )
        .optional()?;
    let Some((path, size, modified, created, failed, description, stored)) = row else { return Ok(None) };
    let revision = format!("{size}:{}", modified.map(|m| m.to_bits().to_string()).unwrap_or_else(|| "unknown".into()));
    let name = Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let (stem, extension) = style::split_name(&name);
    let chapters = strings(conn, "SELECT title FROM catalog_chapters WHERE file_id=?1 AND user_edited=1 AND stale=0 AND source_revision=?2 ORDER BY id", params![id, revision])?;
    let events = strings(conn, "SELECT e.title FROM catalog_events e JOIN catalog_event_files ef ON ef.event_id=e.id WHERE ef.file_id=?1 AND e.user_edited=1 ORDER BY e.id", params![id])?;
    let tags = strings(conn, "SELECT tag FROM tags WHERE file_id=?1 ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END, score DESC, tag", params![id])?
        .into_iter()
        .filter(|tag| !style::lower(tag).starts_with("year_"))
        .collect();
    let evidence = Evidence {
        original_stem: stem,
        chapter: style::single_label(&chapters),
        stored_name: stored,
        description,
        tags,
        person_words: HashSet::new(),
        people: confirmed_people(conn, id, &revision, failed != 0)?,
        event: style::single_label(&events),
        date: created.and_then(capture_date),
    };
    Ok(Some(Loaded { path, name, extension, evidence }))
}

fn strings(conn: &Connection, sql: &str, args: impl rusqlite::Params) -> Result<Vec<String>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map(args, |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

fn confirmed_people(conn: &Connection, id: i64, revision: &str, failed: bool) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut accept = |title: Option<String>, first: Option<String>, legacy: Option<String>| {
        if style::is_user_named(&style::person_display(title.as_deref(), first.as_deref(), legacy.as_deref()))
            && let Some(token) = style::person_token(first.as_deref(), legacy.as_deref())
        {
            tokens.push(token);
        }
    };
    if !failed {
        let mut statement = conn.prepare(
            "SELECT p.title,p.first_name,p.name,fp.arcface_embedding,fp.embedding_model,fp.processing_version,fp.source_revision,rep.embedding_model,rep.processing_version \
             FROM face_prints fp JOIN persons p ON p.id=fp.person_id JOIN face_prints rep ON rep.id=p.representative_face_id \
             WHERE fp.file_id=?1 AND COALESCE(fp.excluded,0)=0 AND COALESCE(rep.excluded,0)=0 AND IFNULL(p.is_unknown,0)=0 ORDER BY fp.id",
        )?;
        let mut rows = statement.query([id])?;
        while let Some(row) = rows.next()? {
            let embedding: Option<Vec<u8>> = row.get(3)?;
            let model: Option<String> = row.get(4)?;
            let processing: Option<String> = row.get(5)?;
            let source: Option<String> = row.get(6)?;
            let rep_model: Option<String> = row.get(7)?;
            let rep_processing: Option<String> = row.get(8)?;
            let compatible = model.as_deref().is_some_and(|m| !m.is_empty())
                && processing.as_deref().is_some_and(|p| !p.is_empty())
                && source.as_deref() == Some(revision)
                && rep_model == model
                && rep_processing == processing
                && embedding.as_deref().is_some_and(normalized);
            if compatible {
                accept(row.get(0)?, row.get(1)?, row.get(2)?);
            }
        }
    }
    let mut statement = conn.prepare(
        "SELECT p.title,p.first_name,p.name FROM catalog_observations o JOIN persons p ON p.id=o.person_id \
         WHERE o.file_id=?1 AND o.user_edited=1 AND o.stale=0 AND o.source_revision=?2 AND IFNULL(p.is_unknown,0)=0 ORDER BY o.id",
    )?;
    let mut rows = statement.query(params![id, revision])?;
    while let Some(row) = rows.next()? {
        accept(row.get(0)?, row.get(1)?, row.get(2)?);
    }
    tokens.sort();
    tokens.dedup();
    Ok(tokens)
}

fn normalized(blob: &[u8]) -> bool {
    if blob.len() != 512 {
        return false;
    }
    let mut sum = 0.0f64;
    for chunk in blob.as_chunks::<4>().0 {
        let value = f32::from_le_bytes(*chunk);
        if !value.is_finite() {
            return false;
        }
        sum += f64::from(value) * f64::from(value);
    }
    (0.95..=1.05).contains(&sum)
}

fn capture_date(created: f64) -> Option<String> {
    use chrono::{Datelike, Local, TimeZone};
    if !created.is_finite() || created <= 0.0 {
        return None;
    }
    let date = match Local.timestamp_opt(created.floor() as i64, 0) {
        chrono::LocalResult::Single(date) | chrono::LocalResult::Ambiguous(date, _) => date,
        chrono::LocalResult::None => return None,
    };
    (1900..2100).contains(&date.year()).then(|| date.format("%Y-%m-%d").to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(value: serde_json::Value) -> CatalogRequest {
        serde_json::from_value(value).unwrap()
    }

    fn unit_face() -> Vec<u8> {
        let mut blob = 1.0f32.to_le_bytes().to_vec();
        blob.resize(512, 0);
        blob
    }

    #[test]
    fn confirmed_tokens_collisions_and_validation() {
        let dir = std::env::temp_dir().join(format!("fileid-names-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("IMG_1.jpg");
        let other = dir.join("IMG_2.jpg");
        std::fs::write(&photo, b"a").unwrap();
        std::fs::write(&other, b"b").unwrap();
        std::fs::write(dir.join("sam-baseball-hit.jpg"), b"c").unwrap();
        let conn = crate::db::test_support::memory_db();
        for (id, path) in [(1, &photo), (2, &other)] {
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,created_at,scanned_at,kind,extension,failed,vlm_proposed_name) VALUES(?1,?2,?1,1,10.0,1688428800.0,0,'image','jpg',0,'Sam - Baseball Hit')",
                params![id, path.to_string_lossy()],
            )
            .unwrap();
        }
        let revision = format!("1:{}", 10.0f64.to_bits());
        conn.execute("INSERT INTO persons(id,first_name,created_at) VALUES(1,'Sam',0),(2,'Alex',0)", []).unwrap();
        for (face, file, person, model) in [(1, 1, 1, "m"), (2, 2, 2, "other"), (3, 1, 2, "m")] {
            conn.execute(
                "INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,embedding_model,processing_version,source_revision) VALUES(?1,?2,?3,X'00','[0,0,1,1]',?4,?5,'p',?6)",
                params![face, file, person, unit_face(), model, revision],
            )
            .unwrap();
        }
        conn.execute("UPDATE persons SET representative_face_id=1 WHERE id=1", []).unwrap();
        conn.execute("UPDATE persons SET representative_face_id=2 WHERE id=2", []).unwrap();
        let hits = propose(
            &conn,
            &request(serde_json::json!({"requestID":"n","action":"proposeNames","fileIDs":[1,2,1,99],"naming":{"style":"short","maxLength":40,"includePeople":true}})),
        )
        .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].text, "sam-baseball-hit_2.jpg");
        assert_eq!(hits[0].evidence_id.as_deref(), Some("proposedName"));
        assert_eq!(hits[1].text, "alex-baseball-hit.jpg");
        let bad = request(serde_json::json!({"requestID":"n","action":"proposeNames","fileIDs":[1],"naming":{"style":"short","maxLength":3}}));
        assert!(propose(&conn, &bad).is_err());
        let bad = request(serde_json::json!({"requestID":"n","action":"proposeNames","fileIDs":[],"naming":{"style":"short","maxLength":30}}));
        assert!(propose(&conn, &bad).is_err());
        assert!(propose(&conn, &request(serde_json::json!({"requestID":"n","action":"proposeNames","fileIDs":[1]}))).is_err());
        assert_eq!(capture_date(f64::NAN), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
