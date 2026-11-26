//! Structured Chat answers from fixed SQL templates. No model ever writes SQL: every
//! predicate is a constant with bound parameters.

use super::chat_intent::{bytes_label, count_label, count_noun, noun, DateFilter, Question, MONTH_NAMES};
use super::roots::RootScope;
use crate::ipc::CatalogHit;
use anyhow::Result;
use rusqlite::{types::Value, Connection};
use std::collections::{HashMap, HashSet};
use std::path::Path;

pub(crate) const ANALYZABLE: [&str; 6] = ["image", "video", "pdf", "doc", "audio", "model"];
pub(crate) const SHOWN: usize = 50;
/// Above this many matches a listing walks the date or size index; below it SQLite sorts.
pub(crate) const LARGE: i64 = 2_000;
const TAKEN: &str = crate::util::SQL_TAKEN_DATE;

pub(crate) fn format_label(extensions: &[String]) -> String {
    if extensions.iter().any(|e| e == "arw") && extensions.iter().any(|e| e == "cr2") {
        "RAW".into()
    } else {
        extensions.first().map(|e| e.to_uppercase()).unwrap_or_default()
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Filter {
    pub kinds: Vec<String>,
    pub person_ids: Vec<i64>,
    pub person_names: Vec<String>,
    pub date: Option<DateFilter>,
    pub folder: Option<String>,
    pub undescribed: bool,
    pub faces: bool,
    pub ids: Option<Vec<i64>>,
    pub extensions: Vec<String>,
    pub all_people: bool,
    pub subject: Option<String>,
    pub excluded_person_ids: Vec<i64>,
    pub excluded_person_names: Vec<String>,
    pub min_bytes: Option<i64>,
    pub max_bytes: Option<i64>,
    pub camera: Option<CameraPick>,
    pub root: Option<RootScope>,
    /// Only files showing someone ("photos of people"): a face that is not excluded, or a person observed by the user.
    pub with_faces: bool,
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

    pub fn clause(&self, args: &mut Vec<Value>) -> String {
        self.predicate(args, false)
    }

    /// `ordered` hides the kind column from the planner (`+f.kind`) so a large listing walks the ORDER BY index instead
    /// of sorting every match.
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
        match &self.camera {
            Some(CameraPick::Models { models, .. }) => {
                parts.push("lower(trim(f.camera_model)) IN (SELECT value FROM json_each(?))".into());
                args.push(json(&models.iter().map(|model| model.to_lowercase()).collect::<Vec<_>>()));
            }
            Some(CameraPick::Missing) => parts.push("(f.camera_model IS NULL OR trim(f.camera_model)='')".into()),
            None => {}
        }
        if self.with_faces {
            parts.push("f.id IN (SELECT file_id FROM face_prints WHERE COALESCE(excluded,0)=0 UNION SELECT file_id FROM catalog_observations WHERE person_id IS NOT NULL AND stale=0)".into());
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
            if let Some(months) = &date.months {
                parts.push(format!("strftime('%m',{TAKEN},'unixepoch','localtime') IN (SELECT value FROM json_each(?))"));
                args.push(json(months));
            }
        }
        if let Some(folder) = &self.folder {
            // Windows catalogs store `\` separators; accept either.
            parts.push("(f.path_text LIKE ? ESCAPE '\\' OR f.path_text LIKE ? ESCAPE '\\')".into());
            for separator in ["/", "\\\\"] {
                args.push(Value::Text(match (folder.strip_prefix("named:"), folder.strip_prefix(STARTS_WITH)) {
                    (Some(name), _) => format!("%{separator}{}{separator}%", like_escape(name)),
                    (None, Some(start)) => format!("%{separator}{}%{separator}%", like_escape(start)),
                    (None, None) => format!("{}{separator}%", like_escape(folder.trim_end_matches(['/', '\\']))),
                }));
            }
        }
        if self.undescribed {
            parts.push("(f.vlm_description IS NULL OR trim(f.vlm_description)='')".into());
        }
        if self.faces {
            parts.push("f.has_faces=1".into());
        }
        if let Some(root) = &self.root {
            parts.push(super::roots::clause("f.path_text", "?"));
            args.push(Value::Text(root.param().to_owned()));
        }
        parts.join(" AND ")
    }

    pub fn qualifiers(&self) -> String {
        use std::fmt::Write;
        let mut text = String::new();
        if let Some(subject) = self.subject.as_deref().filter(|subject| !subject.is_empty()) {
            let _ = write!(text, " matching “{subject}”");
        }
        if self.with_faces {
            text += " with people";
        }
        if !self.extensions.is_empty() {
            let _ = write!(text, " in {} format", format_label(&self.extensions));
        }
        match &self.camera {
            Some(CameraPick::Models { label, .. }) => {
                let _ = write!(text, " taken with {label}");
            }
            Some(CameraPick::Missing) => text += " with no camera information",
            None => {}
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
            let _ = write!(text, " in {}", folder.strip_prefix("named:").or_else(|| folder.strip_prefix(STARTS_WITH)).unwrap_or(folder));
        }
        if let Some(date) = &self.date {
            let _ = write!(text, " {}", date.phrase);
        }
        if self.undescribed {
            text += " not described yet";
        }
        if self.faces {
            text += " with faces";
        }
        text
    }
}

/// What a question says about the camera: models the catalog knows (one model, or every model of a brand) or files
/// that record none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CameraPick {
    Models { label: String, models: Vec<String> },
    Missing,
}

/// A camera choice named in a question; `rest` is the question without those words.
pub(crate) struct CameraAsk {
    pub pick: CameraPick,
    pub rest: String,
}

fn alphanumerics(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

const CAMERA_LEAD_WORDS: [&str; 13] = ["taken", "shot", "captured", "recorded", "made", "with", "on", "by", "using", "from", "the", "a", "an"];

/// (words that name the brand, label, lowercase model prefixes or fragments). Models carry no make column, so a brand
/// is its known model families.
const CAMERA_BRANDS: [(&[&str], &str, &[&str]); 12] = [
    (&["sony"], "Sony", &["ilce", "ilca", "ilme", "dsc", "nex", "slt", "zv-", "dcr", "hdr-", "fdr", "dslr-a", "sony"]),
    (&["canon"], "Canon", &["canon", "eos", "powershot", "ixus"]),
    (&["nikon"], "Nikon", &["nikon", "coolpix"]),
    (&["iphone", "apple"], "Apple", &["iphone", "ipad", "apple"]),
    (&["samsung", "galaxy"], "Samsung", &["samsung", "sm-", "galaxy"]),
    (&["pixel", "google"], "Google", &["pixel"]),
    (&["fujifilm", "fuji"], "Fujifilm", &["fujifilm", "finepix", "x-t", "x-e", "x-pro", "x100", "gfx"]),
    (&["panasonic", "lumix"], "Panasonic", &["panasonic", "dmc-", "dc-"]),
    (&["olympus"], "Olympus", &["olympus", "e-m", "tg-", "om-"]),
    (&["gopro"], "GoPro", &["gopro", "hero"]),
    (&["leica"], "Leica", &["leica"]),
    (&["pentax"], "Pentax", &["pentax"]),
];

const MISSING_WORDS: [&str; 7] = ["no", "without", "missing", "lacking", "lack", "lacks", "nothing"];
const CAMERA_DETAIL_WORDS: [&str; 14] = ["camera", "cameras", "information", "info", "data", "model", "models", "metadata", "exif", "recorded", "listed", "details", "set", "saved"];

fn lowered(word: &str) -> String {
    super::chat_intent::clean(word).replace('\'', "")
}

pub(crate) fn camera_asked_about(conn: &Connection, text: &str) -> Result<Option<CameraAsk>> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let lower: Vec<String> = words.iter().map(|word| lowered(word)).collect();
    let camera_word = lower.iter().enumerate().any(|(index, word)| {
        (word == "camera" || word == "cameras") && !lower.get(index + 1).is_some_and(|next| next == "folder" || next == "directory")
    });
    if camera_word && lower.iter().any(|word| MISSING_WORDS.contains(&word.as_str())) {
        let drop: Vec<bool> = lower
            .iter()
            .enumerate()
            .map(|(index, word)| {
                MISSING_WORDS.contains(&word.as_str())
                    || CAMERA_DETAIL_WORDS.contains(&word.as_str())
                    || (word == "with" && lower.get(index + 1).is_some_and(|next| MISSING_WORDS.contains(&next.as_str())))
            })
            .collect();
        let rest = words.iter().zip(&drop).filter(|(_, dropped)| !**dropped).map(|(word, _)| *word).collect::<Vec<_>>().join(" ");
        return Ok(Some(CameraAsk { pick: CameraPick::Missing, rest }));
    }
    let mut statement = conn.prepare("SELECT DISTINCT trim(camera_model) FROM files WHERE failed=0 AND camera_model IS NOT NULL AND trim(camera_model)<>''")?;
    let models = statement.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut best: Option<(&String, usize, usize)> = None;
    for model in &models {
        let wanted = alphanumerics(model);
        if wanted.len() < 3 {
            continue;
        }
        for start in 0..words.len() {
            for end in start + 1..=(start + 4).min(words.len()) {
                if alphanumerics(&words[start..end].concat()) == wanted && best.is_none_or(|(known, _, _)| model.len() > known.len()) {
                    best = Some((model, start, end));
                    break;
                }
            }
        }
    }
    let strip = |start: usize, end: usize| {
        let mut first = start;
        while first > 0 && CAMERA_LEAD_WORDS.contains(&lower[first - 1].as_str()) {
            first -= 1;
        }
        let mut last = end;
        if lower.get(last).is_some_and(|word| word == "camera" || word == "cameras") {
            last += 1;
        }
        words.iter().enumerate().filter(|(index, _)| !(first..last).contains(index)).map(|(_, word)| *word).collect::<Vec<_>>().join(" ")
    };
    if let Some((model, start, end)) = best {
        return Ok(Some(CameraAsk { pick: CameraPick::Models { label: model.clone(), models: vec![model.clone()] }, rest: strip(start, end) }));
    }
    let context = lower.iter().any(|word| ["camera", "cameras", "taken", "shot", "took", "captured", "photographed", "take"].contains(&word.as_str()));
    if !context {
        return Ok(None);
    }
    for (names, label, families) in CAMERA_BRANDS {
        let Some(at) = lower.iter().position(|word| names.contains(&word.as_str())) else { continue };
        let known: Vec<String> = models
            .iter()
            .filter(|model| {
                let model = model.to_lowercase();
                families.iter().any(|family| model.starts_with(family) || (family.len() > 4 && model.contains(family)))
            })
            .cloned()
            .collect();
        if known.is_empty() {
            continue;
        }
        let article = if label.starts_with(['A', 'E', 'I', 'O', 'U']) { "an" } else { "a" };
        return Ok(Some(CameraAsk { pick: CameraPick::Models { label: format!("{article} {label} camera"), models: known }, rest: strip(at, at + 1) }));
    }
    Ok(None)
}

pub(crate) fn file_name(path: &str) -> String {
    Path::new(path).file_name().map_or_else(|| path.to_owned(), |name| name.to_string_lossy().into_owned())
}

pub(crate) fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
}

pub(crate) fn totals(conn: &Connection, filter: &Filter) -> Result<(i64, i64)> {
    let mut args = Vec::new();
    let clause = filter.clause(&mut args);
    Ok(conn.query_row(
        &format!("SELECT COUNT(*),COALESCE(SUM(f.size_bytes),0) FROM files f WHERE {clause}"),
        rusqlite::params_from_iter(args),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?)
}

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
            selection_match: None,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Extra copies sharing a stored content hash and size, with the copy Cleanup would keep (best-looking, largest,
/// oldest, shortest path).
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
            selection_match: None,
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

/// Both path separators are folded so a catalog scanned on Windows reads the same.
pub(crate) fn folders(conn: &Connection, by_size: bool, limit: i64, root: Option<&RootScope>) -> Result<Vec<FolderTotal>> {
    let path = "replace(f.path_text,char(92),'/')";
    let order = if by_size { "b DESC,c DESC" } else { "c DESC,b DESC" };
    let within = super::roots::clause("f.path_text", "?2");
    let sql = format!("SELECT rtrim({path},replace({path},'/','')) AS dir,COUNT(*) AS c,COALESCE(SUM(f.size_bytes),0) AS b FROM files f WHERE f.failed=0 AND {within} GROUP BY dir ORDER BY {order},dir ASC LIMIT ?1");
    let mut statement = conn.prepare(&sql)?;
    let rows = statement.query_map(rusqlite::params![limit, root.map(RootScope::param)], |row| {
        let mut dir: String = row.get(0)?;
        while dir.len() > 1 && dir.ends_with('/') {
            dir.pop();
        }
        Ok(FolderTotal { name: dir.rsplit('/').find(|part| !part.is_empty()).unwrap_or(&dir).to_owned(), path: dir, count: row.get(1)?, bytes: row.get(2)? })
    })?;
    let mut found = rows.collect::<rusqlite::Result<Vec<_>>>()?;
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

fn folder_count(conn: &Connection, root: Option<&RootScope>) -> Result<i64> {
    let path = "replace(f.path_text,char(92),'/')";
    let within = super::roots::clause("f.path_text", "?1");
    let sql = format!("SELECT COUNT(DISTINCT rtrim({path},replace({path},'/',''))) FROM files f WHERE f.failed=0 AND {within}");
    Ok(conn.query_row(&sql, [root.map(RootScope::param)], |row| row.get(0))?)
}

fn folder_phrase(folder: &FolderTotal) -> String {
    format!("{} ({} {}, {})", folder.name, count_label(folder.count), if folder.count == 1 { "file" } else { "files" }, bytes_label(folder.bytes))
}

fn library_answer(conn: &Connection, question: &Question, root: Option<&RootScope>) -> Result<(String, Vec<CatalogHit>)> {
    let everything = Filter { root: root.cloned(), ..Filter::default() };
    let within = super::roots::clause("f.path_text", "?1");
    let param = [root.map(RootScope::param)];
    let (count, bytes) = totals(conn, &everything)?;
    let hits = listed(conn, &everything, "newest", SHOWN, count)?;
    if count == 0 {
        return Ok(("Your library is empty. Scan a folder to get started.".into(), hits));
    }
    if question.measure == "folders" {
        let by_size = question.order == "largest";
        let top = folders(conn, by_size, 5, root)?;
        let list = top.iter().map(folder_phrase).collect::<Vec<_>>().join(", ");
        return Ok((format!("{}: {list}.", if by_size { "Biggest folders" } else { "Folders with the most files" }), hits));
    }
    if question.measure == "folder_count" {
        let folders = folder_count(conn, root)?;
        return Ok((format!("Your library has {} holding {}.", count_noun(folders, "folder", "folders"), count_noun(count, "file", "files")), hits));
    }
    if question.measure == "formats" {
        let mut statement = conn.prepare(&format!(
            "SELECT upper(COALESCE(NULLIF(f.extension,''),'none')) AS e,COUNT(*) AS c FROM files f WHERE f.failed=0 AND {within} \
             GROUP BY lower(COALESCE(NULLIF(f.extension,''),'none')) ORDER BY c DESC,e ASC"
        ))?;
        let formats = statement
            .query_map(param, |row| {
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
    let mut statement = conn.prepare(&format!("SELECT f.kind,COUNT(*) AS c FROM files f WHERE f.failed=0 AND {within} GROUP BY f.kind ORDER BY c DESC,f.kind ASC"))?;
    let kinds = statement
        .query_map(param, |row| {
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
        &format!("SELECT strftime('%Y-%m-%d',MIN({TAKEN}),'unixepoch','localtime'),strftime('%Y-%m-%d',MAX({TAKEN}),'unixepoch','localtime') FROM files f WHERE f.failed=0 AND {within}"),
        param,
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if let (Some(lo), Some(hi)) = range {
        {
            use std::fmt::Write;
            let _ = if lo == hi { write!(message, " Taken on {lo}.") } else { write!(message, " Dates run from {lo} to {hi}.") };
        }
    }
    let top = folders(conn, false, 3, root)?;
    if !top.is_empty() {
        {
            use std::fmt::Write;
            let _ = write!(message, " Busiest folders: {}.", top.iter().map(folder_phrase).collect::<Vec<_>>().join(", "));
        }
    }
    Ok((message, hits))
}

struct PersonGroup {
    id: i64,
    name: Option<String>,
    files: i64,
}

fn person_groups(conn: &Connection, root: Option<&RootScope>) -> Result<Vec<PersonGroup>> {
    let within = super::roots::clause("f.path_text", "?1");
    let mut statement = conn.prepare(&format!(
        "SELECT p.id,p.name,p.first_name,p.last_name,COUNT(DISTINCT fp.file_id) AS n FROM persons p \
         JOIN face_prints fp ON fp.person_id=p.id AND COALESCE(fp.excluded,0)=0 \
         JOIN files f ON f.id=fp.file_id AND f.failed=0 \
         WHERE COALESCE(p.is_unknown,0)=0 AND {within} GROUP BY p.id ORDER BY n DESC,p.id ASC"
    ))?;
    let rows = statement.query_map([root.map(RootScope::param)], |row| {
        let name: Option<String> = row.get(1)?;
        let first: Option<String> = row.get(2)?;
        let last: Option<String> = row.get(3)?;
        let composed = [first, last].into_iter().flatten().map(|part| part.trim().to_owned()).filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" ");
        let name = name.map(|name| name.trim().to_owned()).filter(|name| !name.is_empty()).or((!composed.is_empty()).then_some(composed));
        Ok(PersonGroup { id: row.get(0)?, name, files: row.get(4)? })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub(crate) fn people_unnamed(conn: &Connection, root: Option<&RootScope>) -> Result<bool> {
    let groups = person_groups(conn, root)?;
    Ok(!groups.is_empty() && groups.iter().all(|group| group.name.is_none()))
}

/// Answered from the face groups, never from the word "people"; files with a counted face are listed.
pub(crate) fn people_answer(conn: &Connection, question: &Question, root: Option<&RootScope>) -> Result<(String, Vec<CatalogHit>)> {
    let groups = person_groups(conn, root)?;
    if groups.is_empty() {
        return Ok(("No people are grouped yet. Faces are grouped after a scan finds them; open People to see the groups.".into(), vec![]));
    }
    let within = super::roots::clause("f.path_text", "?1");
    let files: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT fp.file_id) FROM face_prints fp JOIN persons p ON p.id=fp.person_id AND COALESCE(p.is_unknown,0)=0 \
             JOIN files f ON f.id=fp.file_id AND f.failed=0 WHERE COALESCE(fp.excluded,0)=0 AND {within}"
        ),
        [root.map(RootScope::param)],
        |row| row.get(0),
    )?;
    let filter = Filter { person_ids: groups.iter().map(|group| group.id).collect(), root: root.cloned(), ..Filter::default() };
    let hits = listed(conn, &filter, "newest", SHOWN, files)?;
    let named: Vec<&PersonGroup> = groups.iter().filter(|group| group.name.is_some()).collect();
    let unnamed = groups.len() - named.len();
    let in_files = count_noun(files as i64, "file", "files");
    let message = if named.is_empty() {
        format!("Your library has {} of people in {in_files}, but none are named yet. Open People to name them.", count_noun(groups.len() as i64, "group", "groups"))
    } else if question.people.as_deref() == Some("who") {
        let names: Vec<String> =
            named.iter().take(8).map(|group| format!("{} ({})", group.name.as_deref().unwrap_or_default(), count_noun(group.files, "file", "files"))).collect();
        let more = if named.len() > 8 { format!(" and {} more", named.len() - 8) } else { String::new() };
        let rest = if unnamed == 0 { String::new() } else { format!(" {} not named yet; open People to name them.", count_noun(unnamed as i64, "more group is", "more groups are")) };
        format!("People in your library: {}{more}.{rest}", names.join(", "))
    } else {
        let names: Vec<&str> = named.iter().take(5).filter_map(|group| group.name.as_deref()).collect();
        let more = if named.len() > 5 { format!(" and {} more", named.len() - 5) } else { String::new() };
        let rest = if unnamed == 0 { String::new() } else { format!(", {unnamed} not named yet. Open People to name the rest") };
        format!(
            "Your library has {} of people in {in_files}: {} named ({}{more}){rest}.",
            count_noun(groups.len() as i64, "group", "groups"),
            count_label(named.len() as i64),
            names.join(", ")
        )
    };
    Ok((message, hits))
}

fn peak_lead(groups: &[(Option<String>, i64, i64)], question: &Question, peak: &str, unit: &str, noun_phrase: &str) -> String {
    let by_size = question.measure == "size";
    let metric = |group: &(Option<String>, i64, i64)| if by_size { group.2 } else { group.1 };
    let dated: Vec<(&str, i64)> = groups.iter().filter_map(|group| group.0.as_deref().map(|label| (label, metric(group)))).collect();
    let target = if peak == "least" { dated.iter().map(|group| group.1).min() } else { dated.iter().map(|group| group.1).max() };
    let Some(target) = target else {
        let missing = if unit == "camera" { "a camera recorded" } else { "a capture date" };
        return format!("None of these files has {missing}, so I can't rank them by {unit}.");
    };
    let label = |text: &str| match (unit, text.split_once('-')) {
        ("month", Some((year, month))) => month.parse::<usize>().ok().and_then(|n| MONTH_NAMES.get(n.wrapping_sub(1))).map_or_else(|| text.to_owned(), |name| format!("{name} {year}")),
        _ => text.to_owned(),
    };
    let winners: Vec<String> = dated.iter().filter(|group| group.1 == target).map(|group| label(group.0)).collect();
    let extreme = match (peak, by_size) {
        ("least", true) => "the least",
        ("least", false) => "the fewest",
        _ => "the most",
    };
    let amount = if by_size { bytes_label(target) } else { count_label(target) };
    let what = if by_size { "space".to_owned() } else { noun_phrase.to_owned() };
    match winners.as_slice() {
        [only] => format!("{only} {} {extreme} {what} ({amount}).", if by_size { "uses" } else { "has" }),
        [head @ .., last] => format!("{} and {last} are tied for {extreme} {what} ({amount} each).", head.join(", ")),
        [] => String::new(),
    }
}

/// Folder filter for names that begin with the asked name ("Ryan Homecoming" for "Ryan Homecoming Photos 9-26-26").
const STARTS_WITH: &str = "startswith:";

/// A folder asked about by a short name that no folder is exactly called widens to the folders that begin with it.
pub(crate) fn widened_folder(conn: &Connection, folder: Option<String>, root: Option<&RootScope>) -> Result<Option<String>> {
    let Some(name) = folder.as_deref().and_then(|folder| folder.strip_prefix("named:")) else { return Ok(folder) };
    let exact = Filter { folder: folder.clone(), root: root.cloned(), ..Filter::default() };
    if totals(conn, &exact)?.0 > 0 {
        return Ok(folder);
    }
    let widened = format!("{STARTS_WITH}{name}");
    let starts = Filter { folder: Some(widened.clone()), root: root.cloned(), ..Filter::default() };
    Ok(if totals(conn, &starts)?.0 > 0 { Some(widened) } else { folder })
}

/// The folder filter for a name that exact or longer folder names carry, `None` when no folder does.
pub(crate) fn existing_folder(conn: &Connection, name: &str, root: Option<&RootScope>) -> Result<Option<String>> {
    let folder = widened_folder(conn, Some(format!("named:{name}")), root)?;
    let filter = Filter { folder: folder.clone(), root: root.cloned(), ..Filter::default() };
    Ok(if totals(conn, &filter)?.0 > 0 { folder } else { None })
}

/// `None` when the catalog has no such folder, so the question is searched as words instead.
pub(crate) fn folder_answer(conn: &Connection, name: &str, root: Option<&RootScope>) -> Result<Option<(String, Vec<CatalogHit>)>> {
    let folder = widened_folder(conn, Some(format!("named:{name}")), root)?;
    let filter = Filter { folder, root: root.cloned(), ..Filter::default() };
    let (count, bytes) = totals(conn, &filter)?;
    if count == 0 {
        return Ok(None);
    }
    let mut args = Vec::new();
    let clause = filter.clause(&mut args);
    let mut statement = conn.prepare(&format!("SELECT f.kind,COUNT(*) AS c FROM files f WHERE {clause} GROUP BY f.kind ORDER BY c DESC,f.kind ASC"))?;
    let kinds = statement
        .query_map(rusqlite::params_from_iter(args), |row| {
            let (kind, n): (String, i64) = (row.get(0)?, row.get(1)?);
            Ok(format!("{} {}", count_label(n), noun(&[kind], n)))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let hits = listed(conn, &filter, "newest", SHOWN, count)?;
    let files = if count == 1 { "file" } else { "files" };
    Ok(Some((format!("“{name}” holds {} {files} ({}): {}.", count_label(count), bytes_label(bytes), kinds.join(", ")), hits)))
}

/// "Photos" can name several folders (one per scanned location); say so instead of silently merging their files.
pub(crate) fn same_named_folders_note(conn: &Connection, folder: &str, root: Option<&RootScope>) -> Result<Option<String>> {
    let Some(name) = folder.strip_prefix("named:") else { return Ok(None) };
    let filter = Filter { folder: Some(folder.to_owned()), root: root.cloned(), ..Filter::default() };
    let mut args = vec![Value::Text(format!("/{name}/")), Value::Text(format!("/{name}/"))];
    let clause = filter.clause(&mut args);
    let mut statement = conn.prepare(&format!(
        "SELECT substr(f.path_text,1,instr(lower(replace(f.path_text,char(92),'/')),lower(?))+length(?)-2) AS folder,COUNT(*) AS c FROM files f WHERE {clause} GROUP BY folder ORDER BY c DESC,folder ASC LIMIT 6"
    ))?;
    let found = statement
        .query_map(rusqlite::params_from_iter(args), |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if found.len() < 2 {
        return Ok(None);
    }
    let places: Vec<String> = found
        .iter()
        .map(|(path, count)| {
            let parts: Vec<&str> = path.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
            let shown = parts[parts.len().saturating_sub(2)..].join("/");
            format!("{shown} ({})", count_noun(*count, "file", "files"))
        })
        .collect();
    Ok(Some(format!("{} folders are called “{name}” and their files are counted together: {}.", count_label(found.len() as i64), places.join(", "))))
}

type UnreadableRow = (i64, String, String, String);

fn unreadable_rows(conn: &Connection, root: Option<&RootScope>) -> Result<Vec<UnreadableRow>> {
    let mut args = vec![
        Value::Text(crate::scan_session::HIDDEN_ROW_MESSAGE.to_owned()),
        Value::Text(crate::db::zero_byte::ZERO_BYTE_REASON.to_owned()),
    ];
    let within = match root {
        Some(root) => {
            args.push(Value::Text(root.param().to_owned()));
            format!(" AND {}", super::roots::clause("f.path_text", "?3"))
        }
        None => String::new(),
    };
    let mut statement = conn.prepare(&format!(
        "SELECT f.id,f.path_text,f.kind,COALESCE(f.error_message,'') FROM files f WHERE f.failed=1 \
         AND COALESCE(f.error_message,'') NOT IN (?1,?2){within} ORDER BY f.path_text ASC,f.id ASC"
    ))?;
    let rows = statement
        .query_map(rusqlite::params_from_iter(args), |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn unreadable_hits(rows: Vec<UnreadableRow>) -> Vec<CatalogHit> {
    rows.into_iter()
        .take(SHOWN)
        .map(|(file_id, path, kind, reason)| CatalogHit {
            file_id,
            path,
            kind,
            text: if reason.trim().is_empty() { "Couldn't be read".to_owned() } else { reason.trim().chars().take(120).collect() },
            evidence_id: None,
            start_seconds: None,
            page: None,
            selection_match: None,
        })
        .collect()
}

/// The files the unreadable note counts, named, for "which files failed?" and "how many files couldn't be read?".
pub(crate) fn unreadable_answer(conn: &Connection, root: Option<&RootScope>) -> Result<(String, Vec<CatalogHit>)> {
    let rows = unreadable_rows(conn, root)?;
    if rows.is_empty() {
        return Ok(("No files failed to read.".into(), vec![]));
    }
    let count = rows.len() as i64;
    let hits = unreadable_hits(rows);
    let shown = if count as usize > hits.len() { format!(" Showing the first {}.", hits.len()) } else { String::new() };
    let (noun, pronoun) = if count == 1 { ("file", "it's") } else { ("files", "they're") };
    Ok((format!("{} {noun} couldn't be read, so {pronoun} not in other answers.{shown}", count_label(count)), hits))
}

/// "Why couldn't they be read?": the recorded reasons with how many files each covers, most common first.
pub(crate) fn unreadable_reasons(conn: &Connection, root: Option<&RootScope>) -> Result<(String, Vec<CatalogHit>)> {
    let rows = unreadable_rows(conn, root)?;
    if rows.is_empty() {
        return Ok(("No files failed to read.".into(), vec![]));
    }
    let mut reasons: HashMap<String, i64> = HashMap::new();
    for (_, _, _, reason) in &rows {
        let reason = reason.trim();
        *reasons.entry(if reason.is_empty() { "No reason was recorded".to_owned() } else { reason.to_owned() }).or_default() += 1;
    }
    let mut reasons: Vec<(String, i64)> = reasons.into_iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let listed = reasons.iter().take(5).map(|(reason, files)| format!("“{reason}” ({})", count_noun(*files, "file", "files"))).collect::<Vec<_>>().join(", ");
    let more = if reasons.len() > 5 { format!(" and {} more", reasons.len() - 5) } else { String::new() };
    let count = rows.len() as i64;
    Ok((format!("{} couldn't be read. Reasons: {listed}{more}.", count_noun(count, "file", "files")), unreadable_hits(rows)))
}

/// "Takeout" names one nested folder here and "Google Takeout Export" holds thousands of files: offer the longer
/// names that contain the word, since the exact match alone can badly undercount what the person means.
pub(crate) fn containing_folders_note(conn: &Connection, folder: &str, root: Option<&RootScope>) -> Result<Option<String>> {
    let Some(name) = folder.strip_prefix("named:").or_else(|| folder.strip_prefix(STARTS_WITH)).map(str::trim).filter(|name| name.chars().count() >= 3) else { return Ok(None) };
    let wanted = name.to_lowercase();
    let within = super::roots::clause("f.path_text", "?2");
    let mut statement = conn.prepare(&format!(
        "SELECT f.path_text FROM files f WHERE f.failed=0 AND lower(f.path_text) LIKE ?1 ESCAPE '\\' AND {within}"
    ))?;
    let mut found: HashMap<String, i64> = HashMap::new();
    let mut rows = statement.query(rusqlite::params![format!("%{}%", like_escape(&wanted)), root.map(RootScope::param)])?;
    while let Some(row) = rows.next()? {
        let path: String = row.get(0)?;
        let parts: Vec<&str> = path.split(['/', '\\']).filter(|part| !part.is_empty()).collect();
        let Some((_, folders)) = parts.split_last() else { continue };
        if let Some(holder) = folders.iter().find(|part| part.to_lowercase().contains(&wanted)) {
            *found.entry((*holder).to_owned()).or_default() += 1;
        }
    }
    found.retain(|holder, _| !holder.eq_ignore_ascii_case(name));
    if found.is_empty() {
        return Ok(None);
    }
    let mut found: Vec<(String, i64)> = found.into_iter().collect();
    found.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let places: Vec<String> = found.iter().take(3).map(|(holder, count)| format!("{holder} ({})", count_noun(*count, "file", "files"))).collect();
    Ok(Some(format!("Folders with “{name}” in their name: {}. Ask about one by its full name.", places.join(", "))))
}

/// Said after an answer when files failed to read (a busy machine timed them out, say): they are in none of the answers.
/// Counts what the apps' Unreadable filter counts, inside the asked-about location.
pub(crate) fn unreadable_files_note(conn: &Connection, root: Option<&RootScope>) -> Result<Option<String>> {
    let mut args = vec![
        Value::Text(crate::scan_session::HIDDEN_ROW_MESSAGE.to_owned()),
        Value::Text(crate::db::zero_byte::ZERO_BYTE_REASON.to_owned()),
    ];
    let within = match root {
        Some(root) => {
            args.push(Value::Text(root.param().to_owned()));
            format!(" AND {}", super::roots::clause("f.path_text", "?3"))
        }
        None => String::new(),
    };
    let count: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM files f WHERE f.failed=1 \
             AND COALESCE(f.error_message,'') NOT IN (?1,?2){within}"
        ),
        rusqlite::params_from_iter(args),
        |row| row.get(0),
    )?;
    Ok((count > 0).then(|| {
        let (noun, pronoun) = if count == 1 { ("file", "it's") } else { ("files", "they're") };
        format!("{} {noun} couldn't be read — {pronoun} not in these results.", count_label(count))
    }))
}

pub(crate) fn reference_answer(reference: &super::chat_intent::Reference, hits: Vec<CatalogHit>) -> (String, Vec<CatalogHit>) {
    use super::chat_intent::Reference;
    if hits.is_empty() {
        return ("There is no earlier list to pick from yet. Ask a question first, then say “the second one”.".into(), vec![]);
    }
    let count = hits.len();
    let describe = |hit: &CatalogHit| if hit.text.is_empty() { file_name(&hit.path) } else { format!("{} ({})", file_name(&hit.path), hit.text) };
    let pick = match reference {
        Reference::Nth(n) => hits.get(n - 1).cloned().ok_or_else(|| format!("The last answer showed {} {}, so there is no number {n}.", count_label(count as i64), if count == 1 { "file" } else { "files" })),
        Reference::Last => Ok(hits[count - 1].clone()),
        Reference::Single if count == 1 => Ok(hits[0].clone()),
        Reference::Single => Err(format!("The last answer showed {} files. Say which one, like “the first one” or “the biggest one”.", count_label(count as i64))),
    };
    match pick {
        Ok(hit) => {
            let which = match reference {
                Reference::Nth(n) => format!("The {} one", ordinal_label(*n)),
                Reference::Last => "The last one".into(),
                Reference::Single => "That".into(),
            };
            (format!("{which} is {}.", describe(&hit)), vec![hit])
        }
        Err(message) => (message, vec![]),
    }
}

fn ordinal_label(n: usize) -> String {
    const NAMES: [&str; 10] = ["first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth", "ninth", "tenth"];
    NAMES.get(n - 1).map_or_else(|| format!("{n}th"), |name| (*name).to_owned())
}

/// The library records positions, not place names, so say how many files carry one.
pub(crate) fn places_answer(conn: &Connection, kinds: &[String], root: Option<&RootScope>) -> Result<String> {
    let filter = Filter { kinds: kinds.to_vec(), root: root.cloned(), ..Filter::default() };
    let (total, _) = totals(conn, &filter)?;
    let mut args = Vec::new();
    let clause = filter.clause(&mut args);
    let located: i64 = conn.query_row(
        &format!("SELECT COUNT(*) FROM files f WHERE {clause} AND f.location_lat IS NOT NULL AND f.location_lon IS NOT NULL"),
        rusqlite::params_from_iter(args),
        |row| row.get(0),
    )?;
    let things = noun(kinds, 2);
    Ok(if total == 0 {
        format!("No {things} in the library yet.")
    } else if located == 0 {
        format!("None of your {} {things} have a location recorded, so I can't say where they were taken.", count_label(total))
    } else {
        format!("{} of your {} {things} have a location recorded, but I can't name the places yet.", count_label(located), count_label(total))
    })
}

/// "Where was DSC02096 taken?": the position stored for the file a question names by its stem, `None` when it names none.
pub(crate) fn file_location_answer(conn: &Connection, text: &str, root: Option<&RootScope>) -> Result<Option<(String, Vec<CatalogHit>)>> {
    let within = super::roots::clause("f.path_text", "?2");
    let mut statement = conn.prepare(&format!(
        "SELECT f.id,f.path_text,f.location_lat,f.location_lon FROM files f WHERE f.failed=0 AND lower(f.path_text) LIKE ?1 ESCAPE '\\' AND {within} ORDER BY f.path_text LIMIT 20"
    ))?;
    for stem in text.split_whitespace().map(|word| word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')) {
        if stem.len() < 5 || !stem.chars().any(|c| c.is_ascii_digit()) || !stem.chars().any(|c| c.is_alphabetic()) {
            continue;
        }
        let pattern = format!("%{}.%", like_escape(&stem.to_lowercase()));
        let rows = statement
            .query_map(rusqlite::params![pattern, root.map(RootScope::param)], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<f64>>(2)?, row.get::<_, Option<f64>>(3)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let found: Vec<_> = rows.into_iter().filter(|(_, path, _, _)| file_name(path).rsplit_once('.').is_some_and(|(file_stem, _)| file_stem.eq_ignore_ascii_case(stem))).collect();
        let Some((_, path, _, _)) = found.first() else { continue };
        let name = file_name(path);
        let position = found.iter().find_map(|(_, _, lat, lon)| lat.zip(*lon));
        let message = match position {
            Some((lat, lon)) => format!("{name} has a location recorded ({lat:.4}, {lon:.4}), but I can't name places yet."),
            None => format!("{name} has no location recorded, so I can't say where it was taken."),
        };
        let ids = found.iter().map(|(id, _, _, _)| *id).collect();
        let filter = Filter { ids: Some(ids), root: root.cloned(), ..Filter::default() };
        let (count, _) = totals(conn, &filter)?;
        return Ok(Some((message, listed(conn, &filter, "newest", SHOWN, count)?)));
    }
    Ok(None)
}

pub(crate) fn answer(conn: &Connection, question: &Question, filter: &Filter) -> Result<(String, Vec<CatalogHit>)> {
    if matches!(question.measure.as_str(), "overview" | "folders" | "folder_count" | "formats") {
        return library_answer(conn, question, filter.root.as_ref());
    }
    let qualifiers = filter.qualifiers();
    if let (Some(unit), Some(_)) = (&question.breakdown, &question.peak)
        && !["year", "month", "camera"].contains(&unit.as_str())
    {
        return Ok((format!("I can't rank {} by {unit} yet. I can count them by year or month, for example “which month has the most {}?”", noun(&filter.kinds, 2), noun(&filter.kinds, 2)), vec![]));
    }
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
        let (group, order) = match unit.as_str() {
            "camera" => ("NULLIF(trim(f.camera_model),'')".to_owned(), "c DESC,g ASC".to_owned()),
            _ => (
                format!("strftime('{}',{TAKEN},'unixepoch','localtime')", if unit == "year" { "%Y" } else { "%Y-%m" }),
                "g DESC".to_owned(),
            ),
        };
        let mut statement = conn.prepare(&format!(
            "SELECT {group} AS g,COUNT(*) AS c,COALESCE(SUM(f.size_bytes),0) AS b FROM files f WHERE {clause} GROUP BY g ORDER BY g IS NULL,{order}"
        ))?;
        let groups = statement
            .query_map(rusqlite::params_from_iter(args), |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, i64>(1)?, row.get::<_, i64>(2)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let parts: Vec<String> = groups
            .iter()
            .take(24)
            .map(|(label, n, b)| {
                let label = label.as_deref().unwrap_or(if unit == "camera" { "no camera recorded" } else { "no date" });
                if question.measure == "size" { format!("{label}: {}", bytes_label(*b)) } else { format!("{label}: {}", count_label(*n)) }
            })
            .collect();
        let more = if groups.len() > 24 { format!(" and {} more", groups.len() - 24) } else { String::new() };
        let lead = question.peak.as_deref().map(|peak| format!("{} ", peak_lead(&groups, question, peak, unit, &noun(&kinds, 2)))).unwrap_or_default();
        return Ok((format!("{lead}{}{qualifiers} by {unit}: {}{more}.", capitalized(&noun(&kinds, 2)), parts.join(", ")), hits));
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

const SUBJECT_FILLERS: [&str; 56] = [
    "them", "they", "it", "its", "those", "these", "ones", "about", "of", "the", "a", "an", "and", "with", "in", "on", "at", "to", "for", "from", "showing", "shows", "show", "containing",
    "contains", "contain", "that", "which", "any", "some", "whats", "what", "is", "are", "there", "where", "who", "me", "my", "i", "do",
    "does", "have", "has", "s", "mentioning", "mention", "mentions", "named", "called", "titled", "labeled", "labelled", "regarding",
    "related", "",
];
pub(crate) const SUBJECT_LIMIT: usize = 200_000;

/// "beach or sunset" → [[beach], [sunset]]. Each word costs several index lookups, so a pasted paragraph is cut to the
/// first few.
pub(crate) const SUBJECT_MAX_GROUPS: usize = 6;
pub(crate) const SUBJECT_MAX_WORDS: usize = 8;

const PEOPLE_SUBJECTS: [&str; 9] = ["people", "person", "persons", "faces", "face", "anyone", "someone", "somebody", "humans"];

/// "photos of people", "pictures with faces": the subject asks for anyone at all, not for a word.
pub(crate) fn asks_for_anyone(query: &str) -> bool {
    matches!(subject_groups(query).as_slice(), [group] if !group.is_empty() && group.iter().all(|word| PEOPLE_SUBJECTS.contains(&word.as_str())))
}

pub(crate) fn subject_groups(query: &str) -> Vec<Vec<String>> {
    let mut groups: Vec<Vec<String>> = vec![vec![]];
    for word in query.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
        if word == "or" {
            if groups.last().is_some_and(|group| !group.is_empty()) {
                if groups.len() == SUBJECT_MAX_GROUPS {
                    break;
                }
                groups.push(vec![]);
            }
        } else if !SUBJECT_FILLERS.contains(&word)
            && let Some(group) = groups.last_mut()
            && group.len() < SUBJECT_MAX_WORDS
            && !group.iter().any(|seen| seen == word)
        {
            group.push(word.to_owned());
        }
    }
    groups.retain(|group| !group.is_empty());
    groups
}

/// Counts, sizes and orders see all matches, not search's best hundred. A word matches name, description, scan tags,
/// document/photo text or current video evidence, written or singular; every word must match, "or" joins alternatives.
/// `None` when no subject word remains.
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
    fn subject_groups_are_bounded() {
        let long = (0..500).map(|n| format!("word{n}")).collect::<Vec<_>>().join(" or ");
        let groups = subject_groups(&long);
        assert_eq!(groups.len(), SUBJECT_MAX_GROUPS);
        let wide = subject_groups(&(0..500).map(|n| format!("w{n}")).collect::<Vec<_>>().join(" "));
        assert_eq!(wide, vec![(0..SUBJECT_MAX_WORDS).map(|n| format!("w{n}")).collect::<Vec<_>>()]);
        assert_eq!(subject_groups("dog dog DOG"), vec![vec!["dog".to_string()]]);
    }

    #[test]
    fn folders_with_the_same_name_are_told_apart_by_their_parent() {
        let conn = crate::db::test_support::memory_db();
        for (id, path) in [(1, "/a/Videos/Downloads/x.mov"), (2, "/a/Videos/Downloads/y.mov"), (3, "/a/Photos/Downloads/z.jpg"), (4, "/a/Docs/n.txt")] {
            conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?1,?2,?1,1000,0,'image','x')", rusqlite::params![id, path]).unwrap();
        }
        let names: Vec<String> = folders(&conn, false, 5, None).unwrap().into_iter().map(|folder| folder.name).collect();
        assert_eq!(names, vec!["Videos/Downloads", "Docs", "Photos/Downloads"]);
    }

    #[test]
    fn folder_totals_walk_the_directory_index() {
        let conn = crate::db::test_support::memory_db();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,'C:\\a\\b.txt',1,5,0,'doc','txt')", []).unwrap();
        conn.execute("INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(2,'C:/a/c.txt',2,5,0,'doc','txt')", []).unwrap();
        let found = folders(&conn, false, 5, None).unwrap();
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
        let names: Vec<String> = folders(&conn, false, 5, None).unwrap().into_iter().map(|folder| folder.name).collect();
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
    fn dates_in_the_future_never_make_a_photo_the_newest() {
        let conn = crate::db::test_support::memory_db();
        for (id, created, modified) in [(1, 2_212_000_000.0, 2_212_000_000.0), (2, 1_700_000_000.0, 1_700_000_000.0), (3, 2_212_000_000.0, 1_650_000_000.0)] {
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,modified_at,scanned_at,kind,extension) VALUES(?1,?2,?1,1,?3,?4,0,'image','jpg')",
                rusqlite::params![id, format!("/p/{id}.jpg"), created, modified],
            )
            .unwrap();
        }
        let newest: Vec<i64> = listed(&conn, &Filter::default(), "newest", SHOWN, 3).unwrap().iter().map(|hit| hit.file_id).collect();
        assert_eq!(newest, vec![2, 3, 1], "the 2040 photo sorts last, and a photo with a bad capture date falls back to its modified date");
        let years: Vec<Option<String>> = conn
            .prepare(&format!("SELECT strftime('%Y',{TAKEN},'unixepoch') FROM files f ORDER BY f.id"))
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        assert_eq!(years, vec![None, Some("2023".into()), Some("2022".into())]);
    }

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
        let question = parse_question("how many winter photos");
        let filter = Filter { kinds: vec!["image".into()], date: question.date.clone(), ..Filter::default() };
        let (message, hits) = answer(&conn, &question, &filter).unwrap();
        assert_eq!(message, "Found 1 photo from winter (any year), using 4.0 MB.");
        assert_eq!(hits.iter().map(|hit| hit.file_id).collect::<Vec<_>>(), vec![5]);
        let question = parse_question("how many photos from 2023");
        let filter = Filter { kinds: vec!["image".into()], date: question.date.clone(), ..Filter::default() };
        assert_eq!(answer(&conn, &question, &filter).unwrap().0, "Found 2 photos from 2023, using 4.0 MB.");
        let question = parse_question("how many videos from 1999");
        let filter = Filter { kinds: vec!["video".into()], date: question.date.clone(), ..Filter::default() };
        assert_eq!(answer(&conn, &question, &filter).unwrap().0, "No videos from 1999.");
    }
}
