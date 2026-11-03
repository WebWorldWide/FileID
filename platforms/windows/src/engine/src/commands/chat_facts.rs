//! Structured Chat answers from fixed SQL templates. Mirrors macOS
//! `ChatFacts.swift`. No model ever writes SQL: every predicate below is a
//! constant with bound parameters.

use super::chat_intent::{bytes_label, count_label, noun, DateFilter, Question};
use crate::ipc::CatalogHit;
use anyhow::Result;
use rusqlite::{types::Value, Connection};
use std::path::Path;

pub(crate) const ANALYZABLE: [&str; 6] = ["image", "video", "pdf", "doc", "audio", "model"];
pub(crate) const SHOWN: usize = 50;
/// Above this many matches a listing walks the date or size index; below it,
/// SQLite sorts the matches. Same threshold in macOS `ChatFacts`.
pub(crate) const LARGE: i64 = 2_000;
const TAKEN: &str = "COALESCE(f.created_at,f.modified_at)";

/// Which files a structured answer or action covers.
#[derive(Clone, Debug, Default)]
pub(crate) struct Filter {
    pub kinds: Vec<String>,
    pub person_ids: Vec<i64>,
    pub person_names: Vec<String>,
    pub date: Option<DateFilter>,
    /// `named:<folder>` or an absolute path.
    pub folder: Option<String>,
    pub undescribed: bool,
    pub ids: Option<Vec<i64>>,
}

fn json<T: serde::Serialize>(value: &T) -> Value {
    Value::Text(serde_json::to_string(value).unwrap_or_else(|_| "[]".into()))
}

pub(crate) fn like_escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")
}

fn modifiers(parts: &[String], args: &mut Vec<Value>) -> String {
    args.extend(parts.iter().cloned().map(Value::Text));
    let marks = vec!["?"; parts.len()].join(",");
    format!("CAST(strftime('%s',{marks},'utc') AS REAL)")
}

impl Filter {
    pub fn effective_kinds(&self) -> Vec<String> {
        if self.kinds.is_empty() && self.undescribed {
            ANALYZABLE.iter().map(|kind| (*kind).to_owned()).collect()
        } else {
            self.kinds.clone()
        }
    }

    /// The SQL predicate over `files f`, appending its parameters.
    pub fn clause(&self, args: &mut Vec<Value>) -> String {
        self.predicate(args, false)
    }

    /// `ordered` hides the kind column from the planner (`+f.kind`) so a
    /// large listing walks the ORDER BY index instead of sorting every match.
    fn predicate(&self, args: &mut Vec<Value>, ordered: bool) -> String {
        let mut parts = vec!["f.failed=0".to_owned()];
        let kinds = self.effective_kinds();
        if !kinds.is_empty() {
            parts.push(if ordered { "+f.kind IN (SELECT value FROM json_each(?))" } else { "f.kind IN (SELECT value FROM json_each(?))" }.into());
            args.push(json(&kinds));
        }
        if !self.person_ids.is_empty() {
            parts.push("f.id IN (SELECT file_id FROM face_prints WHERE person_id IN (SELECT value FROM json_each(?)) UNION SELECT file_id FROM catalog_observations WHERE person_id IN (SELECT value FROM json_each(?)) AND stale=0)".into());
            args.push(json(&self.person_ids));
            args.push(json(&self.person_ids));
        }
        if let Some(ids) = &self.ids {
            parts.push("f.id IN (SELECT value FROM json_each(?))".into());
            args.push(json(ids));
        }
        if let Some(date) = &self.date {
            if let Some(from) = &date.from {
                let bound = modifiers(from, args);
                parts.push(format!("{TAKEN}>={bound}"));
            }
            if let Some(to) = &date.to {
                let bound = modifiers(to, args);
                parts.push(format!("{TAKEN}<{bound}"));
            }
            if let Some(month_day) = &date.month_day {
                parts.push(format!("strftime('%m-%d',{TAKEN},'unixepoch','localtime')=?"));
                args.push(Value::Text(month_day.clone()));
            }
            if let Some(month) = &date.month {
                parts.push(format!("strftime('%m',{TAKEN},'unixepoch','localtime')=?"));
                args.push(Value::Text(month.clone()));
            }
        }
        if let Some(folder) = &self.folder {
            parts.push("f.path_text LIKE ? ESCAPE '\\'".into());
            args.push(Value::Text(match folder.strip_prefix("named:") {
                Some(name) => format!("%/{}/%", like_escape(name)),
                None => format!("{}/%", like_escape(folder.trim_end_matches(['/', '\\']))),
            }));
        }
        if self.undescribed {
            parts.push("(f.vlm_description IS NULL OR trim(f.vlm_description)='')".into());
        }
        parts.join(" AND ")
    }

    /// " of Kyle in Downloads from 2023 not described yet".
    pub fn qualifiers(&self) -> String {
        use std::fmt::Write;
        let mut text = String::new();
        if !self.person_names.is_empty() {
            let _ = write!(text, " of {}", self.person_names.join(" and "));
        }
        if let Some(folder) = &self.folder {
            let _ = write!(text, " in {}", folder.strip_prefix("named:").unwrap_or(folder));
        }
        if let Some(date) = &self.date {
            let _ = write!(text, " {}", date.phrase);
        }
        if self.undescribed {
            text += " not described yet";
        }
        text
    }
}

pub(crate) fn file_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into_owned())
}

pub(crate) fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

/// Count and total size of the matching files.
pub(crate) fn totals(conn: &Connection, filter: &Filter) -> Result<(i64, i64)> {
    let mut args = Vec::new();
    let clause = filter.clause(&mut args);
    Ok(conn.query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM(f.size_bytes),0) FROM files f WHERE {clause}"),
        rusqlite::params_from_iter(args),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

/// Matching files as citations, in the requested order. `count` is the
/// number of matches, from [`totals`].
pub(crate) fn listed(conn: &Connection, filter: &Filter, order: &str, limit: usize, count: i64) -> Result<Vec<CatalogHit>> {
    let mut args = Vec::new();
    let clause = filter.predicate(&mut args, count > LARGE);
    let order = match order {
        "largest" => "f.size_bytes DESC,f.id DESC".to_owned(),
        "smallest" => "f.size_bytes ASC,f.id ASC".to_owned(),
        "oldest" => format!("{TAKEN} IS NULL,{TAKEN} ASC,f.id ASC"),
        _ => format!("{TAKEN} DESC,f.id DESC"),
    };
    args.push(Value::Integer(limit as i64));
    let sql = format!("SELECT f.id,f.path_text,f.kind,f.size_bytes,strftime('%Y-%m-%d',{TAKEN},'unixepoch','localtime') FROM files f WHERE {clause} ORDER BY {order} LIMIT ?");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params_from_iter(args), |row| {
        let size: i64 = row.get(3)?;
        let date: Option<String> = row.get(4)?;
        Ok(CatalogHit {
            file_id: row.get(0)?,
            path: row.get(1)?,
            kind: row.get(2)?,
            text: match date {
                Some(date) => format!("{} · {date}", bytes_label(size)),
                None => bytes_label(size),
            },
            evidence_id: None,
            start_seconds: None,
            page: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Extra copies of files that share a stored content hash and size, with the
/// copy Cleanup would keep. The keeper order mirrors Cleanup's: best-looking,
/// largest, oldest, shortest path.
pub(crate) struct Extras {
    pub groups: i64,
    pub count: i64,
    pub bytes: i64,
    /// (file, keeper, path, keeper path, size), largest first.
    pub copies: Vec<(i64, i64, String, String, i64)>,
}

pub(crate) fn extras(conn: &Connection, filter: &Filter, limit: usize) -> Result<Extras> {
    let mut args = Vec::new();
    let clause = filter.clause(&mut args);
    let window = "PARTITION BY f.content_hash,f.size_bytes ORDER BY COALESCE(f.aesthetic,0) DESC,f.size_bytes DESC,COALESCE(f.created_at,1e308) ASC,length(f.path_text) ASC,f.path_text ASC,f.id ASC";
    let base = format!(
        "WITH twins AS (SELECT content_hash AS h,size_bytes AS s FROM files WHERE content_hash IS NOT NULL AND failed=0 GROUP BY content_hash,size_bytes HAVING COUNT(*)>1), \
         ranked AS (SELECT f.id,f.path_text,f.size_bytes,ROW_NUMBER() OVER ({window}) AS rank,FIRST_VALUE(f.id) OVER ({window}) AS keeper,FIRST_VALUE(f.path_text) OVER ({window}) AS keeper_path \
         FROM files f JOIN twins ON f.content_hash=twins.h AND f.size_bytes=twins.s WHERE f.failed=0), \
         chosen AS (SELECT r.* FROM ranked r JOIN files f ON f.id=r.id WHERE r.rank>1 AND {clause})"
    );
    let mut statement = conn.prepare(&format!("{base} SELECT id,keeper,path_text,keeper_path,size_bytes FROM chosen ORDER BY size_bytes DESC,path_text ASC,id ASC"))?;
    let mut keepers = std::collections::HashSet::new();
    let (mut count, mut bytes) = (0i64, 0i64);
    let mut copies = Vec::new();
    let mut rows = statement.query(rusqlite::params_from_iter(args))?;
    while let Some(row) = rows.next()? {
        let (keeper, size): (i64, i64) = (row.get(1)?, row.get(4)?);
        keepers.insert(keeper);
        count += 1;
        bytes += size;
        if copies.len() < limit {
            copies.push((row.get(0)?, keeper, row.get(2)?, row.get(3)?, size));
        }
    }
    Ok(Extras { groups: keepers.len() as i64, count, bytes, copies })
}

pub(crate) fn extra_hits(extras: &Extras) -> Vec<CatalogHit> {
    extras
        .copies
        .iter()
        .take(SHOWN)
        .map(|(id, _, path, keeper, size)| CatalogHit {
            file_id: *id,
            path: path.clone(),
            kind: "duplicate".into(),
            text: format!("Extra copy of {} · {}", file_name(keeper), bytes_label(*size)),
            evidence_id: None,
            start_seconds: None,
            page: None,
        })
        .collect()
}

pub(crate) fn duplicates_sentence(extras: &Extras, qualifiers: &str) -> String {
    if extras.count == 0 {
        return format!("No identical copies found{qualifiers}.");
    }
    let copies = if extras.count == 1 { "extra copy" } else { "extra copies" };
    let files = if extras.groups == 1 { "file" } else { "files" };
    format!(
        "Found {} {copies} of {} {files}{qualifiers}, using {}. Cleanup checks every copy byte by byte before anything moves.",
        count_label(extras.count),
        count_label(extras.groups),
        bytes_label(extras.bytes)
    )
}

/// Answers a question that is fully structured: no subject words remain.
pub(crate) fn answer(conn: &Connection, question: &Question, filter: &Filter) -> Result<(String, Vec<CatalogHit>)> {
    let qualifiers = filter.qualifiers();
    if question.duplicates {
        let extras = extras(conn, filter, SHOWN)?;
        let message = if question.measure == "size" && extras.count > 0 {
            format!(
                "Extra copies of identical files{qualifiers} use {} ({} {} of {} {}). Cleanup checks every copy byte by byte before anything moves.",
                bytes_label(extras.bytes),
                count_label(extras.count),
                if extras.count == 1 { "copy" } else { "copies" },
                count_label(extras.groups),
                if extras.groups == 1 { "file" } else { "files" }
            )
        } else {
            duplicates_sentence(&extras, &qualifiers)
        };
        return Ok((message, extra_hits(&extras)));
    }
    let kinds = filter.kinds.clone();
    let (count, bytes) = totals(conn, filter)?;
    let hits = listed(conn, filter, &question.order, SHOWN, count)?;
    if count == 0 {
        return Ok((format!("No {}{qualifiers}.", noun(&kinds, 2)), hits));
    }
    let message = match question.measure.as_str() {
        "size" => format!(
            "{}{qualifiers} use {} ({} {}).",
            capitalized(&noun(&kinds, 2)),
            bytes_label(bytes),
            count_label(count),
            noun(&kinds, count)
        ),
        "count" => format!("Found {} {}{qualifiers}, using {}.", count_label(count), noun(&kinds, count), bytes_label(bytes)),
        _ => {
            let order = if question.explicit_order { format!(" {} first.", capitalized(&question.order)) } else { String::new() };
            format!("Found {} {}{qualifiers}.{order}", count_label(count), noun(&kinds, count))
        }
    };
    Ok((message, hits))
}

/// Keeps only hits whose files pass the structured filter, preserving order.
pub(crate) fn restrict(conn: &Connection, hits: Vec<CatalogHit>, filter: &Filter) -> Result<Vec<CatalogHit>> {
    if hits.is_empty() {
        return Ok(hits);
    }
    let mut ids: Vec<i64> = hits.iter().map(|hit| hit.file_id).collect();
    ids.sort_unstable();
    ids.dedup();
    let scoped = Filter { ids: Some(ids), ..filter.clone() };
    let mut args = Vec::new();
    let clause = scoped.clause(&mut args);
    let mut statement = conn.prepare(&format!("SELECT f.id FROM files f WHERE {clause}"))?;
    let keep: std::collections::HashSet<i64> =
        statement.query_map(rusqlite::params_from_iter(args), |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(hits.into_iter().filter(|hit| keep.contains(&hit.file_id)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::chat_intent::parse_question;

    fn library() -> Connection {
        let conn = crate::db::test_support::memory_db();
        conn.execute_batch(
            "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,scanned_at,kind,extension,content_hash,aesthetic,vlm_description) VALUES
             (1,'/lib/Videos/party.mov',1,3000000000,1726747200,0,'video','mov',X'01',NULL,'Birthday party'),
             (2,'/lib/Videos/party copy.mov',2,3000000000,1726747300,0,'video','mov',X'01',NULL,NULL),
             (3,'/lib/Downloads/IMG_1.jpg',3,2000000,1695124800,0,'image','jpg',X'02',0.9,NULL),
             (4,'/lib/Downloads/IMG_1 (1).jpg',4,2000000,1695124800,0,'image','jpg',X'02',0.1,NULL),
             (5,'/lib/Photos/IMG_5.jpg',5,4000000,1704110400,0,'image','jpg',X'05',NULL,'Snow'),
             (6,'/lib/Docs/receipt.pdf',6,50000,1704110400,0,'pdf','pdf',X'06',NULL,NULL);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn counts_sizes_dates_and_duplicates() {
        let conn = library();
        let question = parse_question("how many videos from September 19?");
        let filter = Filter { kinds: vec!["video".into()], date: question.date.clone(), ..Filter::default() };
        let (message, hits) = answer(&conn, &question, &filter).unwrap();
        assert_eq!(message, "Found 2 videos from September 19 (any year), using 6.0 GB.");
        assert_eq!(hits.iter().map(|hit| hit.file_id).collect::<Vec<_>>(), vec![2, 1]);
        let question = parse_question("how much space do duplicates use?");
        let (message, hits) = answer(&conn, &question, &Filter::default()).unwrap();
        assert_eq!(message, "Extra copies of identical files use 3.0 GB (2 copies of 2 files). Cleanup checks every copy byte by byte before anything moves.");
        assert_eq!(hits.iter().map(|hit| hit.file_id).collect::<Vec<_>>(), vec![2, 4]);
        assert_eq!(hits[1].text, "Extra copy of IMG_1.jpg · 2.0 MB");
        let question = parse_question("largest files in Downloads");
        let filter = Filter { folder: question.folder.clone(), ..Filter::default() };
        assert_eq!(answer(&conn, &question, &filter).unwrap().0, "Found 2 files in Downloads. Largest first.");
        let question = parse_question("how many files are not described yet");
        let filter = Filter { undescribed: true, ..Filter::default() };
        assert_eq!(answer(&conn, &question, &filter).unwrap().0, "Found 4 files not described yet, using 3.0 GB.");
        let question = parse_question("how many photos from 2023");
        let filter = Filter { kinds: vec!["image".into()], date: question.date.clone(), ..Filter::default() };
        assert_eq!(answer(&conn, &question, &filter).unwrap().0, "Found 2 photos from 2023, using 4.0 MB.");
        let question = parse_question("how many videos from 1999");
        let filter = Filter { kinds: vec!["video".into()], date: question.date.clone(), ..Filter::default() };
        assert_eq!(answer(&conn, &question, &filter).unwrap().0, "No videos from 1999.");
    }
}
