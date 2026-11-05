//! Structured Chat answers from fixed SQL templates. Mirrors macOS
//! `ChatFacts.swift`. No model ever writes SQL: every predicate below is a
//! constant with bound parameters.

use super::chat_intent::{bytes_label, count_label, noun, DateFilter, Question};
use crate::ipc::CatalogHit;
use anyhow::Result;
use rusqlite::{types::Value, Connection};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub(crate) const ANALYZABLE: [&str; 6] = ["image", "video", "pdf", "doc", "audio", "model"];
pub(crate) const SHOWN: usize = 50;
/// Above this many matches a listing walks the date or size index; below it,
/// SQLite sorts the matches. Same threshold in macOS `ChatFacts`.
pub(crate) const LARGE: i64 = 2_000;
const TAKEN: &str = "COALESCE(f.created_at,f.modified_at)";

/// "RAW" for the RAW family, else the first extension in capitals ("JPG", "PNG").
pub(crate) fn format_label(extensions: &[String]) -> String {
    if extensions.iter().any(|e| e == "arw") && extensions.iter().any(|e| e == "cr2") {
        "RAW".into()
    } else {
        extensions.first().map(|e| e.to_uppercase()).unwrap_or_default()
    }
}

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
    /// Lowercase file extensions ("raw" covers every RAW type).
    pub extensions: Vec<String>,
    /// Every named person must appear ("Kyle and Maya"), not any of them.
    pub all_people: bool,
    /// The subject words behind `ids`, for the reply.
    pub subject: Option<String>,
    /// People the question leaves out ("without Maya").
    pub excluded_person_ids: Vec<i64>,
    pub excluded_person_names: Vec<String>,
    /// Exclusive byte bounds.
    pub min_bytes: Option<i64>,
    pub max_bytes: Option<i64>,
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
        if !self.extensions.is_empty() {
            parts.push("lower(f.extension) IN (SELECT value FROM json_each(?))".into());
            args.push(json(&self.extensions));
        }
        if self.all_people && self.person_ids.len() > 1 {
            let mut people = self.person_ids.clone();
            people.sort_unstable();
            people.dedup();
            for person in people {
                parts.push("f.id IN (SELECT file_id FROM face_prints WHERE person_id=? UNION SELECT file_id FROM catalog_observations WHERE person_id=? AND stale=0)".into());
                args.push(Value::Integer(person));
                args.push(Value::Integer(person));
            }
        } else if !self.person_ids.is_empty() {
            parts.push("f.id IN (SELECT file_id FROM face_prints WHERE person_id IN (SELECT value FROM json_each(?)) UNION SELECT file_id FROM catalog_observations WHERE person_id IN (SELECT value FROM json_each(?)) AND stale=0)".into());
            args.push(json(&self.person_ids));
            args.push(json(&self.person_ids));
        }
        if !self.excluded_person_ids.is_empty() {
            parts.push("f.id NOT IN (SELECT file_id FROM face_prints WHERE person_id IN (SELECT value FROM json_each(?)) UNION SELECT file_id FROM catalog_observations WHERE person_id IN (SELECT value FROM json_each(?)) AND stale=0)".into());
            args.push(json(&self.excluded_person_ids));
            args.push(json(&self.excluded_person_ids));
        }
        if let Some(min) = self.min_bytes {
            parts.push("f.size_bytes>?".into());
            args.push(Value::Integer(min));
        }
        if let Some(max) = self.max_bytes {
            parts.push("f.size_bytes<?".into());
            args.push(Value::Integer(max));
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
            // Windows catalogs store `\` separators; accept either.
            parts.push("(f.path_text LIKE ? ESCAPE '\\' OR f.path_text LIKE ? ESCAPE '\\')".into());
            for separator in ["/", "\\\\"] {
                args.push(Value::Text(match folder.strip_prefix("named:") {
                    Some(name) => format!("%{separator}{}{separator}%", like_escape(name)),
                    None => format!("{}{separator}%", like_escape(folder.trim_end_matches(['/', '\\']))),
                }));
            }
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
        if let Some(subject) = self.subject.as_deref().filter(|subject| !subject.is_empty()) {
            let _ = write!(text, " matching “{subject}”");
        }
        if !self.extensions.is_empty() {
            let _ = write!(text, " in {} format", format_label(&self.extensions));
        }
        if !self.person_names.is_empty() {
            let separator = if self.all_people || self.person_names.len() == 1 { " and " } else { " or " };
            let _ = write!(text, " of {}", self.person_names.join(separator));
        }
        if !self.excluded_person_names.is_empty() {
            let _ = write!(text, " without {}", self.excluded_person_names.join(" or "));
        }
        if let Some(min) = self.min_bytes {
            let _ = write!(text, " over {}", bytes_label(min));
        }
        if let Some(max) = self.max_bytes {
            let _ = write!(text, " under {}", bytes_label(max));
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

pub(crate) struct FolderTotal {
    pub name: String,
    pub path: String,
    pub count: i64,
    pub bytes: i64,
}

/// Folders holding the most files (or bytes), from the stored paths. Both path
/// separators are folded so a catalog scanned on Windows reads the same.
pub(crate) fn folders(conn: &Connection, by_size: bool, limit: i64) -> Result<Vec<FolderTotal>> {
    let path = "replace(f.path_text,char(92),'/')";
    let order = if by_size { "b DESC,c DESC" } else { "c DESC,b DESC" };
    let sql = format!("SELECT rtrim({path},replace({path},'/','')) AS dir,COUNT(*) AS c,COALESCE(SUM(f.size_bytes),0) AS b FROM files f WHERE f.failed=0 GROUP BY dir ORDER BY {order},dir ASC LIMIT ?1");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map([limit], |row| {
        let mut dir: String = row.get(0)?;
        while dir.len() > 1 && dir.ends_with('/') {
            dir.pop();
        }
        Ok(FolderTotal { name: dir.rsplit('/').find(|part| !part.is_empty()).unwrap_or(&dir).to_owned(), path: dir, count: row.get(1)?, bytes: row.get(2)? })
    })?;
    let mut found = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    // Widen the label one folder at a time until no two folders share it.
    for depth in 2..8 {
        let mut seen = std::collections::HashSet::new();
        let repeated: std::collections::HashSet<String> =
            found.iter().filter(|folder| !seen.insert(folder.name.clone())).map(|folder| folder.name.clone()).collect();
        if repeated.is_empty() {
            break;
        }
        for folder in &mut found {
            if repeated.contains(&folder.name) {
                let parts: Vec<&str> = folder.path.split('/').filter(|part| !part.is_empty()).collect();
                if parts.len() >= depth {
                    folder.name = parts[parts.len() - depth..].join("/");
                }
            }
        }
    }
    Ok(found)
}

fn folder_phrase(folder: &FolderTotal) -> String {
    format!("{} ({} {}, {})", folder.name, count_label(folder.count), if folder.count == 1 { "file" } else { "files" }, bytes_label(folder.bytes))
}

fn library_answer(conn: &Connection, question: &Question) -> Result<(String, Vec<CatalogHit>)> {
    let everything = Filter::default();
    let (count, bytes) = totals(conn, &everything)?;
    let hits = listed(conn, &everything, "newest", SHOWN, count)?;
    if count == 0 {
        return Ok(("Your library is empty. Scan a folder to get started.".into(), hits));
    }
    if question.measure == "folders" {
        let by_size = question.order == "largest";
        let top = folders(conn, by_size, 5)?;
        let list = top.iter().map(folder_phrase).collect::<Vec<_>>().join(", ");
        return Ok((format!("{}: {list}.", if by_size { "Biggest folders" } else { "Folders with the most files" }), hits));
    }
    if question.measure == "formats" {
        let mut statement = conn.prepare(
            "SELECT upper(COALESCE(NULLIF(f.extension,''),'none')) AS e,COUNT(*) AS c FROM files f WHERE f.failed=0 \
             GROUP BY lower(COALESCE(NULLIF(f.extension,''),'none')) ORDER BY c DESC,e ASC",
        )?;
        let formats = statement
            .query_map([], |row| {
                let (ext, n): (String, i64) = (row.get(0)?, row.get(1)?);
                Ok(format!("{} {ext}", count_label(n)))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let more = if formats.len() > 10 { format!(" and {} more", formats.len() - 10) } else { String::new() };
        return Ok((
            format!(
                "Your {} {} {} {}: {}{more}.",
                count_label(count),
                if count == 1 { "file uses" } else { "files use" },
                formats.len(),
                if formats.len() == 1 { "format" } else { "formats" },
                formats.iter().take(10).cloned().collect::<Vec<_>>().join(", ")
            ),
            hits,
        ));
    }
    let mut statement = conn.prepare("SELECT f.kind,COUNT(*) AS c FROM files f WHERE f.failed=0 GROUP BY f.kind ORDER BY c DESC,f.kind ASC")?;
    let kinds = statement
        .query_map([], |row| {
            let (kind, n): (String, i64) = (row.get(0)?, row.get(1)?);
            Ok(format!("{} {}", count_label(n), noun(&[kind], n)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut message = format!(
        "Your library has {} {} using {}: {}.",
        count_label(count),
        if count == 1 { "file" } else { "files" },
        bytes_label(bytes),
        kinds.join(", ")
    );
    let range: (Option<String>, Option<String>) = conn.query_row(
        &format!("SELECT strftime('%Y-%m-%d',MIN({TAKEN}),'unixepoch','localtime'),strftime('%Y-%m-%d',MAX({TAKEN}),'unixepoch','localtime') FROM files f WHERE f.failed=0"),
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if let (Some(lo), Some(hi)) = range {
        {
            use std::fmt::Write;
            let _ = if lo == hi { write!(message, " Taken on {lo}.") } else { write!(message, " Dates run from {lo} to {hi}.") };
        }
    }
    let top = folders(conn, false, 3)?;
    if !top.is_empty() {
        {
            use std::fmt::Write;
            let _ = write!(message, " Busiest folders: {}.", top.iter().map(folder_phrase).collect::<Vec<_>>().join(", "));
        }
    }
    Ok((message, hits))
}

/// Answers a question that is fully structured: no subject words remain.
pub(crate) fn answer(conn: &Connection, question: &Question, filter: &Filter) -> Result<(String, Vec<CatalogHit>)> {
    if matches!(question.measure.as_str(), "overview" | "folders" | "formats") {
        return library_answer(conn, question);
    }
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
    let shown = question.limit.map_or(SHOWN, |limit| usize::try_from(limit).unwrap_or(SHOWN).min(SHOWN));
    let hits = listed(conn, filter, &question.order, shown, count)?;
    if count == 0 {
        return Ok((format!("No {}{qualifiers}.", noun(&kinds, 2)), hits));
    }
    if let Some(limit) = question.limit
        && question.measure == "list"
    {
        let names = hits.iter().map(|hit| format!("{} ({})", file_name(&hit.path), hit.text)).collect::<Vec<_>>().join(", ");
        let top = limit.min(count);
        let number = if top == 1 { String::new() } else { format!("{} ", count_label(top)) };
        let ordered = if question.explicit_order { format!("{} ", question.order) } else { String::new() };
        return Ok((format!("The {number}{ordered}{}{qualifiers}: {names}.", noun(&kinds, top)), hits));
    }
    if let Some(unit) = &question.breakdown {
        let mut args = Vec::new();
        let clause = filter.clause(&mut args);
        let format = if unit == "year" { "%Y" } else { "%Y-%m" };
        let mut statement = conn.prepare(&format!(
            "SELECT strftime('{format}',{TAKEN},'unixepoch','localtime') AS g,COUNT(*) AS c,COALESCE(SUM(f.size_bytes),0) AS b FROM files f WHERE {clause} GROUP BY g ORDER BY g IS NULL,g DESC"
        ))?;
        let groups = statement
            .query_map(rusqlite::params_from_iter(args), |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let parts: Vec<String> = groups
            .iter()
            .take(24)
            .map(|(label, n, b)| {
                let label = label.as_deref().unwrap_or("no date");
                if question.measure == "size" { format!("{label}: {}", bytes_label(*b)) } else { format!("{label}: {}", count_label(*n)) }
            })
            .collect();
        let more = if groups.len() > 24 { format!(" and {} more", groups.len() - 24) } else { String::new() };
        return Ok((format!("{}{qualifiers} by {unit}: {}{more}.", capitalized(&noun(&kinds, 2)), parts.join(", ")), hits));
    }
    let message = match question.measure.as_str() {
        "size" => {
            let largest = if question.explicit_order && question.order == "largest" && !hits.is_empty() {
                let names: Vec<String> =
                    hits.iter().take(3).map(|hit| format!("{} ({})", file_name(&hit.path), hit.text.split(" · ").next().unwrap_or_default())).collect();
                format!(" Largest: {}.", names.join(", "))
            } else {
                String::new()
            };
            format!(
                "{}{qualifiers} use {} ({} {}).{largest}",
                capitalized(&noun(&kinds, 2)),
                bytes_label(bytes),
                count_label(count),
                noun(&kinds, count)
            )
        }
        "count" => format!("Found {} {}{qualifiers}, using {}.", count_label(count), noun(&kinds, count), bytes_label(bytes)),
        _ => {
            let order = if question.explicit_order { format!(" {} first.", capitalized(&question.order)) } else { String::new() };
            format!("Found {} {}{qualifiers}.{order}", count_label(count), noun(&kinds, count))
        }
    };
    Ok((message, hits))
}

/// Words that only join a subject to the question ("about taxes", "photos showing a dog").
const SUBJECT_FILLERS: [&str; 49] = [
    "about", "of", "the", "a", "an", "and", "with", "in", "on", "at", "to", "for", "from", "showing", "shows", "show", "containing",
    "contains", "contain", "that", "which", "any", "some", "whats", "what", "is", "are", "there", "where", "who", "me", "my", "i", "do",
    "does", "have", "has", "s", "mentioning", "mention", "mentions", "named", "called", "titled", "labeled", "labelled", "regarding",
    "related", "",
];
pub(crate) const SUBJECT_LIMIT: usize = 200_000;

/// Subject words split into alternatives on "or": "beach or sunset" → [[beach], [sunset]].
pub(crate) fn subject_groups(query: &str) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = vec![vec![]];
    for word in query.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if word == "or" {
            if groups.last().is_some_and(|group| !group.is_empty()) {
                groups.push(vec![]);
            }
        } else if !SUBJECT_FILLERS.contains(&word)
            && let Some(group) = groups.last_mut()
        {
            group.push(word.to_owned());
        }
    }
    groups.retain(|group| !group.is_empty());
    groups
}

/// Every file the subject describes, so counts, sizes and orders see all
/// matches rather than the best hundred search keeps. A word matches a file's
/// name, description, scan tags, document or photo text, or current video
/// evidence, in its written or singular form; every word must match, and "or"
/// joins alternatives. `None` when no subject word remains. Mirrors macOS
/// `ChatFacts.subjectIDs`.
pub(crate) fn subject_ids(conn: &Connection, query: &str) -> Result<Option<Vec<i64>>> {
    let groups = subject_groups(query);
    if groups.is_empty() {
        return Ok(None);
    }
    let mut cache: HashMap<String, HashSet<i64>> = HashMap::new();
    let mut result: HashSet<i64> = HashSet::new();
    for group in groups {
        let mut matched: Option<HashSet<i64>> = None;
        for word in group {
            if !cache.contains_key(&word) {
                let found = files_matching(conn, &word)?;
                cache.insert(word.clone(), found);
            }
            let found = &cache[&word];
            let next: HashSet<i64> = match matched {
                Some(previous) => previous.intersection(found).copied().collect(),
                None => found.clone(),
            };
            let empty = next.is_empty();
            matched = Some(next);
            if empty {
                break;
            }
        }
        result.extend(matched.unwrap_or_default());
        if result.len() >= SUBJECT_LIMIT {
            break;
        }
    }
    let mut ids: Vec<i64> = result.into_iter().collect();
    ids.sort_unstable();
    ids.truncate(SUBJECT_LIMIT);
    Ok(Some(ids))
}

fn files_matching(conn: &Connection, word: &str) -> Result<HashSet<i64>> {
    let forms = super::catalog_retrieval::word_forms(word);
    let matching = forms.iter().map(|form| format!("\"{}\"", form.replace('"', "\"\""))).collect::<Vec<_>>().join(" OR ");
    let limit = SUBJECT_LIMIT as i64;
    let mut ids = HashSet::new();
    let mut collect = |sql: &str, args: Vec<Value>| -> Result<()> {
        let mut statement = conn.prepare(sql)?;
        for id in statement.query_map(rusqlite::params_from_iter(args), |row| row.get::<_, i64>(0))? {
            ids.insert(id?);
        }
        Ok(())
    };
    for table in ["catalog_file_fts", "doc_fts", "ocr_fts"] {
        collect(&format!("SELECT rowid FROM {table} WHERE {table} MATCH ? LIMIT ?"), vec![Value::Text(matching.clone()), Value::Integer(limit)])?;
    }
    collect(
        "SELECT DISTINCT CAST(e.file_id AS INTEGER) FROM catalog_evidence_fts e \
         LEFT JOIN catalog_chapters c ON c.id=e.evidence_id AND e.kind='chapter' \
         LEFT JOIN catalog_passages p ON p.id=e.evidence_id AND e.kind='passage' \
         WHERE catalog_evidence_fts MATCH ? AND (c.stale=0 OR p.stale=0) LIMIT ?",
        vec![Value::Text(matching.clone()), Value::Integer(limit)],
    )?;
    if word.chars().count() >= 3 {
        let likes = vec!["(' ' || REPLACE(LOWER(tag),'_',' ')) LIKE ?"; forms.len()].join(" OR ");
        let mut args: Vec<Value> = forms.iter().map(|form| Value::Text(format!("% {form}%"))).collect();
        args.push(Value::Integer(limit));
        collect(&format!("SELECT DISTINCT file_id FROM tags WHERE source IN ('auto','user','vlm') AND tag IN (SELECT tag FROM catalog_tag_vocab WHERE {likes}) LIMIT ?"), args)?;
    }
    Ok(ids)
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
    #[test]
    fn folders_with_the_same_name_are_told_apart_by_their_parent() {
        let conn = crate::db::test_support::memory_db();
        for (id, path) in [(1, "/a/Videos/Downloads/x.mov"), (2, "/a/Videos/Downloads/y.mov"), (3, "/a/Photos/Downloads/z.jpg"), (4, "/a/Docs/n.txt")] {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?1,?2,?1,1000,0,'image','x')", rusqlite::params![id, path]).unwrap();
        }
        let names: Vec<String> = folders(&conn, false, 5).unwrap().into_iter().map(|folder| folder.name).collect();
        assert_eq!(names, vec!["Videos/Downloads", "Docs", "Photos/Downloads"]);
    }

    #[test]
    fn folder_totals_walk_the_directory_index() {
        let conn = crate::db::test_support::memory_db();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,'C:\\a\\b.txt',1,5,0,'doc','txt')", []).unwrap();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(2,'C:/a/c.txt',2,5,0,'doc','txt')", []).unwrap();
        let found = folders(&conn, false, 5).unwrap();
        assert_eq!((found.len(), found[0].count, found[0].bytes), (1, 2, 10));
        let path = "replace(f.path_text,char(92),'/')";
        let plan: Vec<String> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN SELECT rtrim({path},replace({path},'/','')) AS dir,COUNT(*),COALESCE(SUM(f.size_bytes),0) FROM files f WHERE f.failed=0 GROUP BY dir"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert!(plan.iter().any(|line| line.contains("catalog_files_dir")), "{plan:?}");
        assert!(!plan.iter().any(|line| line.contains("TEMP B-TREE FOR GROUP BY")), "{plan:?}");
    }

    #[test]
    fn folders_that_still_collide_get_more_of_their_path() {
        let conn = crate::db::test_support::memory_db();
        for (id, path) in [(1, "/a/2019/Videos/Downloads/x.mov"), (2, "/a/2019/Videos/Downloads/y.mov"), (3, "/a/2020/Videos/Downloads/z.mov")] {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?1,?2,?1,1000,0,'video','x')", rusqlite::params![id, path]).unwrap();
        }
        let names: Vec<String> = folders(&conn, false, 5).unwrap().into_iter().map(|folder| folder.name).collect();
        assert_eq!(names, vec!["2019/Videos/Downloads", "2020/Videos/Downloads"]);
    }

    #[test]
    fn a_named_library_gets_the_overview_but_a_library_of_things_is_a_search() {
        let measure = |text: &str| super::super::chat_intent::parse_question(text).measure;
        for text in ["tell me about my Adlon library", "what is in my vacation library", "summarize my Adlon library"] {
            assert_eq!(measure(text), "overview", "{text}");
        }
        for text in ["describe my photo collection", "show me my library of dogs", "tell me about my dog photos", "what is in my library of recipes", "give me my library photos"] {
            assert_ne!(measure(text), "overview", "{text}");
        }
    }

    #[test]
    fn per_type_questions_get_the_library_breakdown() {
        let measure = |text: &str| super::super::chat_intent::parse_question(text).measure;
        for text in ["how many files are there and how many of each file type", "break down my files by type", "what file types do I have", "how many of every kind of file"] {
            assert_eq!(measure(text), "overview", "{text}");
        }
        for text in ["photos by Kyle", "what kind of camera took this", "how many photos from the type of event", "how many files in my Downloads folder by type"] {
            assert_ne!(measure(text), "overview", "{text}");
        }
    }

    #[test]
    fn format_words_filter_by_extension() {
        let conn = crate::db::test_support::memory_db();
        for (id, path, kind, size, ext) in [
            (1, "/p/a.jpg", "image", 3_000, "jpg"),
            (2, "/p/b.arw", "image", 9_000, "arw"),
            (3, "/p/c.JPG", "image", 3_000, "JPG"),
            (4, "/p/d.png", "image", 1_000, "png"),
        ] {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,scanned_at,kind,extension) VALUES(?1,?2,?1,?3,1,0,?4,?5)", rusqlite::params![id, path, size, kind, ext]).unwrap();
        }
        let ask = |text: &str| {
            let question = super::super::chat_intent::parse_question(text);
            let kinds = if question.reduced.to_lowercase().contains("photo") { vec!["image".to_owned()] } else { vec![] };
            let filter = Filter { kinds, extensions: question.formats.clone().unwrap_or_default(), ..Filter::default() };
            answer(&conn, &question, &filter).unwrap().0
        };
        assert_eq!(ask("how many RAW photos are there"), "Found 1 photo in RAW format, using 9.0 KB.");
        assert_eq!(ask("how many jpgs"), "Found 2 files in JPG format, using 6.0 KB.");
        assert_eq!(ask("how many pngs"), "Found 1 file in PNG format, using 1.0 KB.");
    }

    #[test]
    fn library_overview_and_folder_questions_read_totals_not_words() {
        let conn = crate::db::test_support::memory_db();
        for (id, path, kind, size, created) in [
            (1, "/Volumes/Adlon/Camera/Game/a.jpg", "image", 3_000, 1_000_000.0),
            (2, "/Volumes/Adlon/Camera/Game/b.arw", "image", 9_000, 1_000_100.0),
            (3, "/Volumes/Adlon/Camera/Game/c.jpg", "image", 3_000, 1_000_200.0),
            (4, "/Volumes/Adlon/Cabinet/clip.mp4", "video", 50_000, 2_000_000.0),
            (5, "C:\\Users\\a\\Pictures\\d.png", "image", 1_000, 3_000_000.0),
        ] {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,scanned_at,kind,extension) VALUES(?1,?2,?1,?3,?4,0,?5,'x')", rusqlite::params![id, path, size, created, kind]).unwrap();
        }
        let ask = |text: &str| {
            let question = super::super::chat_intent::parse_question(text);
            answer(&conn, &question, &Filter::default()).unwrap().0
        };
        let overview = ask("what is in my library");
        assert!(overview.starts_with("Your library has 5 files using 66.0 KB: 4 photos, 1 video."), "{overview}");
        assert!(overview.contains("Dates run from 1970-01-12 to 1970-02-"), "{overview}");
        assert!(overview.contains("Busiest folders: Game (3 files, 15.0 KB), Cabinet (1 file, 50.0 KB), Pictures (1 file, 1.0 KB)."), "{overview}");
        assert!(ask("which folder has the most files").starts_with("Folders with the most files: Game (3 files, 15.0 KB), Cabinet (1 file, 50.0 KB)"));
        assert!(ask("what are my biggest folders").starts_with("Biggest folders: Cabinet (1 file, 50.0 KB), Game (3 files, 15.0 KB)"));
        let parse = |text: &str| super::super::chat_intent::parse_question(text).measure;
        assert_eq!(parse("how many files in my library"), "count");
        assert_eq!(parse("photos of my library dog"), "list");
        assert_eq!(parse("what is the biggest file"), "list");
        assert_eq!(parse("show me the folder from Downloads"), "list");
    }

    use super::*;
    use crate::commands::chat_intent::parse_question;

    #[test]
    fn folder_filters_match_windows_separators() {
        let conn = crate::db::test_support::memory_db();
        for (id, path) in [(1, "C:\\Users\\a\\Downloads\\x.jpg"), (2, "C:\\Users\\a\\Downloads\\sub\\y.jpg"), (3, "C:\\Users\\a\\Pictures\\z.jpg"), (4, "/lib/Downloads/w.jpg")] {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?1,?2,?1,1000,0,'image','jpg')", rusqlite::params![id, path]).unwrap();
        }
        let count = |folder: &str| totals(&conn, &Filter { folder: Some(folder.into()), ..Filter::default() }).unwrap().0;
        assert_eq!(count("named:Downloads"), 3);
        assert_eq!(count("C:\\Users\\a\\Downloads"), 2);
        assert_eq!(count("C:\\Users\\a\\Downloads\\"), 2);
        assert_eq!(count("/lib/Downloads"), 1);
        assert_eq!(count("C:\\Users\\a\\Down"), 0);
    }

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
