//! Deterministic Chat parsing, line for line with macOS `ChatIntent.swift` (shared cases:
//! `shared/test-corpus/chat-intents.json`). Only the user's own words are parsed, so instructions inside retrieved
//! documents cannot start actions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Intent {
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub people: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setting: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

impl Intent {
    fn new(kind: &str) -> Self {
        Self { kind: kind.to_owned(), ..Self::default() }
    }
}

/// `from`/`to` are SQLite date-modifier lists, evaluated in local time and converted to UTC.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DateFilter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month_day: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub month: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub months: Option<Vec<String>>,
    pub phrase: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Question {
    pub measure: String,
    pub order: String,
    pub explicit_order: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<DateFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    pub duplicates: bool,
    pub undescribed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub faces: bool,
    pub reduced: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formats: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_bytes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peak: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub people: Option<String>,
}

impl Default for Question {
    fn default() -> Self {
        Self {
            measure: "list".into(),
            order: "newest".into(),
            explicit_order: false,
            date: None,
            folder: None,
            duplicates: false,
            undescribed: false,
            faces: false,
            reduced: String::new(),
            formats: None,
            limit: None,
            min_bytes: None,
            max_bytes: None,
            breakdown: None,
            peak: None,
            people: None,
        }
    }
}

impl Question {
    pub fn structured(&self) -> bool {
        self.measure != "list" || self.explicit_order || self.date.is_some() || self.folder.is_some() || self.duplicates || self.undescribed || self.faces || self.formats.is_some()
            || self.limit.is_some() || self.min_bytes.is_some() || self.max_bytes.is_some() || self.breakdown.is_some() || self.people.is_some()
    }
}

const QUESTION_WORDS: [&str; 16] = [
    "how", "what", "which", "where", "when", "who", "why", "is", "are", "do", "does", "did", "was", "were", "has", "have",
];
const KNOWN_FOLDERS: [&str; 9] = ["downloads", "desktop", "documents", "pictures", "movies", "videos", "music", "home", "docs"];

pub fn clean(word: &str) -> String {
    word.replace('’', "'")
        .trim_start_matches(|c: char| "\"'“”‘(".contains(c))
        .trim_end_matches(|c: char| "\"'“”’).,!?;:".contains(c))
        .to_lowercase()
}

fn original(word: &str) -> String {
    word.replace('’', "'")
        .trim_start_matches(|c: char| "\"'“”‘(".contains(c))
        .trim_end_matches(|c: char| "\"'“”’).,!?;:".contains(c))
        .to_owned()
}

fn starts(words: &[String], prefix: &[&str]) -> bool {
    words.len() >= prefix.len() && words.iter().zip(prefix).all(|(word, part)| word == part)
}

fn has(words: &[String], any: &[&str]) -> bool {
    words.iter().any(|word| any.contains(&word.as_str()))
}

fn has_prefix(words: &[String], any: &[&str]) -> bool {
    words.iter().any(|word| any.iter().any(|stem| word.starts_with(stem)))
}

fn is_path(word: &str) -> bool {
    word.starts_with('/')
        || word.starts_with("~/")
        || word == "~"
        || (word.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) && word.get(1..3) == Some(":\\"))
}

pub(crate) fn imperative_words(text: &str) -> (Vec<String>, Vec<String>) {
    let raw: Vec<&str> = text.split_whitespace().collect();
    let mut lower: Vec<String> = raw.iter().map(|word| clean(word)).collect();
    let mut cased: Vec<String> = raw.iter().map(|word| original(word)).collect();
    let keep: Vec<usize> = (0..lower.len()).filter(|&index| !lower[index].is_empty()).collect();
    lower = keep.iter().map(|&index| lower[index].clone()).collect();
    cased = keep.iter().map(|&index| cased[index].clone()).collect();
    const LEADS: [&[&str]; 18] = [
        &["please"], &["can", "you"], &["could", "you"], &["would", "you"], &["will", "you"], &["i", "want", "to"],
        &["i'd", "like", "to"], &["i", "would", "like", "to"], &["help", "me"], &["let's"], &["lets"], &["go", "ahead", "and"],
        &["now"], &["ok"], &["okay"], &["hey"], &["fileid"], &["just"],
    ];
    while let Some(lead) = LEADS.iter().find(|lead| starts(&lower, lead)) {
        lower.drain(..lead.len());
        cased.drain(..lead.len());
    }
    while lower.last().is_some_and(|word| ["please", "now", "thanks"].contains(&word.as_str()))
        || (lower.len() >= 2 && lower[lower.len() - 2..] == ["for", "me"])
    {
        let count = if lower.last().is_some_and(|word| word == "me") { 2 } else { 1 };
        lower.truncate(lower.len() - count);
        cased.truncate(cased.len() - count);
    }
    (lower, cased)
}

const FOLDER_NAME_BOUNDARIES: [&str; 20] = [
    "the", "my", "a", "whole", "entire", "our", "this", "that", "current", "in", "to", "for", "and", "of", "on", "into", "from", "at", "called",
    "named",
];

pub fn folder_reference(words: &[String], cased: &[String], text: &str) -> Option<String> {
    if let Some(quoted) = quoted_path(text) {
        return Some(quoted);
    }
    if let Some(path) = cased.iter().find(|word| is_path(word)) {
        return Some(path.clone());
    }
    for (index, word) in words.iter().enumerate() {
        if KNOWN_FOLDERS.contains(&word.as_str()) {
            return Some(if word == "docs" { "documents".into() } else { word.clone() });
        }
        if (word == "folder" || word == "directory")
            && !words.get(index + 1).is_some_and(|next| ["layout", "structure", "plan", "organization"].contains(&next.as_str()))
        {
            let before = index.checked_sub(1).map(|at| words[at].as_str());
            if matches!(before, Some("this" | "current" | "that")) {
                return Some("this".into());
            }
            if before.is_some_and(|name| !FOLDER_NAME_BOUNDARIES.contains(&name)) {
                let mut start = index - 1;
                while start > 0 && !FOLDER_NAME_BOUNDARIES.contains(&words[start - 1].as_str()) {
                    start -= 1;
                }
                return Some(format!("named:{}", cased[start..index].join(" ")));
            }
            if words.get(index + 1).is_some_and(|next| !["in", "to", "for", "and"].contains(&next.as_str())) {
                return Some(format!("named:{}", cased[index + 1]));
            }
        }
    }
    if has(words, &["this", "here"]) {
        return Some("this".into());
    }
    if words.is_empty() || has(words, &["library", "everything", "files", "all"]) {
        return Some("library".into());
    }
    None
}

fn quoted_path(text: &str) -> Option<String> {
    let text = text.replace(['“', '”'], "\"");
    let mut parts = text.split('"');
    parts.next()?;
    let inside = parts.next()?;
    parts.next()?;
    is_path(inside.trim()).then(|| inside.trim().to_owned())
}

fn person_name(cased: &[String], require_capital: bool) -> Option<String> {
    let mut tokens: Vec<String> = cased.to_vec();
    if tokens.first().is_some_and(|word| word.eq_ignore_ascii_case("as")) {
        tokens.remove(0);
    }
    if tokens.is_empty() || tokens.len() > 4 {
        return None;
    }
    let valid = tokens.iter().all(|token| {
        token.chars().next().is_some_and(char::is_alphabetic)
            && token.chars().all(|c| c.is_alphabetic() || "'-.".contains(c))
    });
    if !valid || (require_capital && !tokens.iter().all(|token| token.chars().next().is_some_and(char::is_uppercase))) {
        return None;
    }
    Some(
        tokens
            .iter()
            .map(|token| {
                if token.chars().all(|c| !c.is_uppercase()) {
                    let mut chars = token.chars();
                    chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default()
                } else {
                    token.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn page_for(words: &[String]) -> Option<&'static str> {
    let filler = ["to", "me", "the", "my", "page", "tab", "view", "screen", "section", "window", "up"];
    let rest: Vec<&str> = words.iter().map(String::as_str).filter(|word| !filler.contains(word)).collect();
    Some(match rest.as_slice() {
        ["library" | "files"] | ["all", "files"] => "library",
        ["people" | "faces" | "persons"] => "people",
        ["duplicates" | "cleanup" | "clean" | "dupes"] => "cleanup",
        ["deep", "analyze"] | ["describe", "files"] | ["descriptions" | "describe"] => "deepAnalyze",
        ["restructure" | "organize" | "organizer" | "folders"] | ["organize", "folders"] => "restructure",
        ["settings" | "preferences" | "options"] => "settings",
        ["search" | "moments"] | ["search", "and" | "&", "moments"] => "search",
        ["tools" | "export"] | ["file", "tools"] => "tools",
        ["chat"] => "chat",
        _ => return None,
    })
}

fn rest_text(words: &[String], from: usize) -> Option<String> {
    let rest = words.get(from..).unwrap_or_default().join(" ");
    (!rest.is_empty()).then_some(rest)
}

const DUPLICATE_WORDS: [&str; 6] = ["duplicate", "duplicates", "dupes", "dupe", "duplicated", "copies"];

/// `None` means the message is a question or search; `refuse` means a request Chat will not turn into an action.
pub fn parse_intent(text: &str) -> Option<Intent> {
    let (words, cased) = imperative_words(text);
    let verb = words.first()?.as_str();
    if QUESTION_WORDS.contains(&verb) {
        return None;
    }
    let rest = &words[1..];
    let duplicates = has(rest, &DUPLICATE_WORDS) || has(rest, &["identical"]);

    if ["stop", "cancel", "pause", "halt", "abort", "end"].contains(&verb) {
        for word in rest {
            let job = if ["scan", "scanning", "scans", "import", "indexing"].contains(&word.as_str()) {
                "scan"
            } else if ["describ", "analy", "caption", "deep"].iter().any(|stem| word.starts_with(stem)) {
                "describe"
            } else if ["organi", "restructur", "reorgani", "move", "moving", "layout"].iter().any(|stem| word.starts_with(stem)) {
                "organize"
            } else if ["moment", "moments", "timeline"].contains(&word.as_str()) {
                "moments"
            } else if ["everything", "all", "jobs", "job", "work", "it", "that", "tasks"].contains(&word.as_str()) {
                "all"
            } else {
                continue;
            };
            return Some(Intent { job: Some(job.into()), ..Intent::new("stopJob") });
        }
        return None;
    }
    if ["undo", "revert", "reverse"].contains(&verb) {
        let movement = rest.is_empty()
            || has_prefix(rest, &["move", "organi", "restructur", "reorgani", "layout"])
            || has(rest, &["last", "that", "it", "folder", "folders"]);
        return movement.then(|| Intent { folder: Some("library".into()), ..Intent::new("undoLastMove") });
    }
    let removal = ["delete", "remove", "trash", "erase", "discard"].contains(&verb)
        || (["move", "put", "send", "throw"].contains(&verb) && has(rest, &["trash", "bin", "recycle", "garbage"]))
        || starts(&words, &["get", "rid"])
        || starts(&words, &["clear", "out"])
        || (starts(&words, &["clean", "up"]) && duplicates);
    if removal {
        return Some(if duplicates || has(rest, &["extra"]) {
            Intent { scope: rest_text(&words, 1), ..Intent::new("moveDuplicatesToTrash") }
        } else {
            Intent::new("refuse")
        });
    }
    if ["find", "show", "list", "check", "look", "any", "search", "detect"].contains(&verb) && duplicates {
        let removes = (0..rest.len()).find(|&at| {
            let after = rest[at + 1..].iter().find(|word| word.as_str() != "then");
            ["and", "then"].contains(&rest[at].as_str())
                && after.is_some_and(|word| ["move", "delete", "remove", "trash", "put", "send", "throw", "erase", "discard"].contains(&word.as_str()))
        });
        return Some(match removes {
            Some(at) => Intent { scope: rest_text(&words[..=at], 1), ..Intent::new("moveDuplicatesToTrash") },
            None => Intent { scope: rest_text(&words, 1), ..Intent::new("findDuplicates") },
        });
    }
    if ["open", "go", "switch", "take", "view", "show"].contains(&verb)
        && let Some(page) = page_for(rest) {
            return Some(Intent { page: Some(page.into()), ..Intent::new("openPage") });
        }
    let suggestion = ["suggested", "suggestion", "suggestions", "smart", "proposed", "ai", "better", "descriptive", "recommended", "new"];
    if (verb == "rename" && has(rest, &suggestion))
        || (["apply", "use", "accept"].contains(&verb) && has(rest, &["names", "name", "filenames"]) && has(rest, &suggestion))
    {
        let scope: Vec<String> = rest
            .iter()
            .filter(|word| !suggestion.contains(&word.as_str()) && !["names", "name", "filenames", "to", "the", "using", "with"].contains(&word.as_str()))
            .cloned()
            .collect();
        return Some(Intent { scope: rest_text(&scope, 0), ..Intent::new("renameToSuggested") });
    }
    let layout = ["layout", "plan", "organization", "structure", "suggestion", "suggestions", "restructure", "reorganization", "folders"];
    if (["apply", "accept"].contains(&verb) || starts(&words, &["go", "with"]) || verb == "use") && has(rest, &layout) {
        let folder = folder_reference(rest, &cased[1..], text).filter(|folder| folder != "library");
        return Some(Intent { folder: Some(folder.unwrap_or_else(|| "library".into())), ..Intent::new("applyFolderLayout") });
    }
    let organize = ["organize", "organise", "reorganize", "reorganise", "restructure", "tidy", "arrange"].contains(&verb)
        || starts(&words, &["sort", "out"])
        || (verb == "clean" && rest.first().is_some_and(|word| word == "up"))
        || (["suggest", "plan", "propose"].contains(&verb) && has(rest, &layout));
    if organize {
        let skip = usize::from(rest.first().is_some_and(|word| word == "up" || word == "out"));
        let folder = folder_reference(&rest[skip..], &cased[1 + skip..], text);
        return Some(Intent { folder: Some(folder.unwrap_or_else(|| "library".into())), ..Intent::new("suggestFolderLayout") });
    }
    if ["name", "call", "label"].contains(&verb) {
        let targets: [&[&str]; 9] = [
            &["this", "person"], &["that", "person"], &["this", "face"], &["this", "one"], &["him"], &["her"], &["them"], &["this"], &["it"],
        ];
        if let Some(target) = targets.iter().find(|target| starts(rest, target))
            && let Some(name) = person_name(&cased[1 + target.len()..], false) {
                return Some(Intent { name: Some(name), ..Intent::new("namePerson") });
            }
        return None;
    }
    for lead in [&["this", "person", "is"][..], &["this", "is"], &["that", "is"], &["that's"], &["this's"], &["he", "is"], &["she", "is"], &["he's"], &["she's"]] {
        if starts(&words, lead) {
            return person_name(&cased[lead.len()..], true).map(|name| Intent { name: Some(name), ..Intent::new("namePerson") });
        }
    }
    if ["merge", "combine", "join"].contains(&verb) {
        let noise = ["people", "persons", "faces", "the", "these", "those", "two", "together", "person", "groups", "clusters", "as", "one"];
        let into = rest.iter().position(|word| word == "into");
        let mut groups: Vec<Vec<String>> = vec![vec![]];
        for (index, word) in rest.iter().enumerate() {
            if ["and", "with", "into", "&"].contains(&word.as_str()) {
                groups.push(vec![]);
                continue;
            }
            if !noise.contains(&word.as_str()) {
                groups.last_mut()?.push(cased[1 + index].trim_end_matches(',').to_owned());
            }
        }
        let mut people: Vec<String> = groups.into_iter().filter(|group| !group.is_empty()).map(|group| group.join(" ")).collect();
        if into.is_some() && people.len() >= 2 {
            let destination = people.pop()?;
            people.insert(0, destination);
        }
        return (people.is_empty() && has(rest, &["these", "those", "them"]) || people.len() >= 2)
            .then(|| Intent { people, ..Intent::new("mergePeople") });
    }
    if let Some(same) = words.windows(3).position(|window| window == ["the", "same", "person"])
        && same >= 2 && ["are", "is"].contains(&words[same - 1].as_str()) {
            let names = &cased[..same - 1];
            let lower = &words[..same - 1];
            if let Some(split) = lower.iter().position(|word| word == "and") {
                let people = vec![names[..split].join(" "), names[split + 1..].join(" ")];
                if people.iter().all(|name| !name.is_empty()) {
                    return Some(Intent { people, ..Intent::new("mergePeople") });
                }
            }
        }
    let library_words = ["the", "my", "library", "files", "scan", "index", "catalog", "everything", "again", "all"];
    if ["rescan", "re-scan", "reindex", "re-index", "refresh", "update"].contains(&verb) {
        if rest.iter().all(|word| library_words.contains(&word.as_str())) {
            return Some(Intent { folder: Some("library".into()), ..Intent::new("rescan") });
        }
        if ["rescan", "re-scan", "reindex", "re-index"].contains(&verb)
            && let Some(folder) = folder_reference(rest, &cased[1..], text).filter(|folder| folder != "library") {
                return Some(Intent { folder: Some(folder), ..Intent::new("rescan") });
            }
    }
    if starts(&words, &["check", "for", "new", "files"]) {
        return Some(Intent { folder: Some("library".into()), ..Intent::new("rescan") });
    }
    if ["scan", "index", "add", "import"].contains(&verb) {
        if rest.iter().all(|word| library_words.contains(&word.as_str())) {
            return (verb == "scan" || verb == "index").then(|| Intent { folder: Some("library".into()), ..Intent::new("rescan") });
        }
        let folder = folder_reference(rest, &cased[1..], text).filter(|folder| folder != "library")?;
        return Some(Intent { folder: Some(folder), ..Intent::new("scanFolder") });
    }
    if ["describe", "caption", "analyze", "analyse"].contains(&verb) || (verb == "write" && has(rest, &["descriptions", "captions"])) {
        let skip = rest.iter().take_while(|word| ["descriptions", "captions", "for", "of"].contains(&word.as_str())).count();
        return Some(Intent { scope: rest_text(&words, 1 + skip), ..Intent::new("describeFiles") });
    }
    if has(&words, &["moment", "moments"]) && ["find", "show", "search", "look", "get"].contains(&verb) {
        let noise = ["the", "moment", "moments", "when", "where", "of", "with", "in", "my", "for", "that", "a", "an", "videos", "video", "me"];
        let query: Vec<String> = cased[1..].iter().zip(rest).filter(|(_, word)| !noise.contains(&word.as_str())).map(|(word, _)| word.clone()).collect();
        return Some(Intent { query: Some(query.join(" ")), ..Intent::new("findMoments") });
    }
    if ["export", "convert", "resize"].contains(&verb) || starts(&words, &["make", "copies"]) || starts(&words, &["save", "copies"]) {
        let skip = if verb == "make" || verb == "save" { 2 + usize::from(words.get(2).is_some_and(|word| word == "of")) } else { 1 };
        return Some(Intent { scope: rest_text(&words, skip), ..Intent::new("exportFiles") });
    }
    let switch = ["turn", "switch"].contains(&verb) && has(rest, &["on", "off"]);
    if switch || ["enable", "disable"].contains(&verb) || (verb == "set" && has(rest, &["on", "off", "true", "false"])) {
        let value = match verb {
            "enable" => true,
            "disable" => false,
            _ => rest.iter().rev().find_map(|word| match word.as_str() {
                "on" | "true" => Some(true),
                "off" | "false" => Some(false),
                _ => None,
            })?,
        };
        let joined = rest.join(" ");
        let setting = if joined.contains("tag") && has_prefix(rest, &["kept", "keeper", "cleanup", "duplicate", "copies", "copy"]) {
            "cleanupAutoTag"
        } else if has_prefix(rest, &["background", "lava", "animation", "motion"]) {
            "backgroundMotion"
        } else if has_prefix(rest, &["summar"]) || joined.contains("local ai") || joined.contains("written answers") {
            "chatSummaries"
        } else {
            return Some(Intent::new("refuse"));
        };
        return Some(Intent { setting: Some(setting.into()), value: Some(value), ..Intent::new("changeSetting") });
    }
    if verb == "search" {
        let skip = rest
            .iter()
            .take_while(|word| ["for", "the", "my", "library", "files", "in", "through"].contains(&word.as_str()))
            .count();
        let query = cased.get(1 + skip..).unwrap_or_default().join(" ");
        return (!query.is_empty()).then(|| Intent { query: Some(query), ..Intent::new("search") });
    }
    None
}

pub fn parse_question(text: &str) -> Question {
    let raw: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<String> = raw.iter().map(|word| clean(word).replace('\'', "")).collect();
    let mut question = Question::default();
    if let Some(ask) = people_ask(&words) {
        question.measure = if ask == "count" { "count" } else { "list" }.into();
        question.people = Some(ask.into());
        return question;
    }
    if let Some((measure, order)) = library_measure(&words) {
        question.measure = measure.into();
        question.order = order.into();
        question.explicit_order = order == "largest";
        return question;
    }
    let mut used = BTreeSet::new();
    let mark = |used: &mut BTreeSet<usize>, range: std::ops::Range<usize>| used.extend(range);
    for (index, pair) in words.windows(2).enumerate() {
        if pair == ["how", "many"] || pair == ["number", "of"] {
            question.measure = "count".into();
            mark(&mut used, index..index + 2);
        }
        if (pair[0] == "how" && ["much", "big", "large"].contains(&pair[1].as_str())) || pair == ["total", "size"] {
            question.measure = "size".into();
            mark(&mut used, index..index + 2);
        }
    }
    if words.first().is_some_and(|word| word == "count") {
        question.measure = "count".into();
        used.insert(0);
    }
    if question.measure == "list" && has(&words, &["space", "storage", "room"]) && has(&words, &["use", "uses", "using", "used", "take", "takes", "taking"]) {
        question.measure = "size".into();
    }
    for (index, word) in words.iter().enumerate() {
        let order = match word.as_str() {
            "largest" | "biggest" | "heaviest" => "largest",
            "smallest" | "tiniest" => "smallest",
            "newest" | "latest" | "recent" => "newest",
            "oldest" | "earliest" => "oldest",
            _ => continue,
        };
        question.order = order.into();
        question.explicit_order = true;
        used.insert(index);
    }
    if has(&words, &DUPLICATE_WORDS) || has(&words, &["identical"]) {
        question.duplicates = true;
        for (index, word) in words.iter().enumerate() {
            if DUPLICATE_WORDS.contains(&word.as_str()) || ["identical", "extra"].contains(&word.as_str()) {
                used.insert(index);
            }
        }
    }
    let described = ["described", "description", "descriptions", "analyzed", "captioned", "caption", "captions"];
    if has(&words, &["undescribed", "unanalyzed"])
        || (has(&words, &["not", "no", "without", "missing", "havent", "hasnt", "need", "needs", "lacking"]) && has(&words, &described))
    {
        question.undescribed = true;
        for (index, word) in words.iter().enumerate() {
            if ["undescribed", "unanalyzed", "not", "no", "without", "missing", "havent", "hasnt", "need", "needs", "lacking", "yet", "been"]
                .contains(&word.as_str())
                || described.contains(&word.as_str())
            {
                used.insert(index);
            }
        }
    }
    if has(&words, &["face", "faces"]) && !has(&words, &["people", "persons", "person", "named", "unnamed"]) {
        question.faces = true;
        for (index, word) in words.iter().enumerate() {
            if ["face", "faces", "with", "contain", "contains", "containing", "show", "showing", "visible", "any"].contains(&word.as_str()) {
                used.insert(index);
            }
        }
    }
    if has(&words, &["camera", "cameras"]) && has(&words, &["which", "what", "whats", "most", "top", "fewest", "least"]) {
        question.breakdown = Some("camera".into());
        if question.measure == "list" {
            question.measure = "count".into();
        }
        if has(&words, &["most", "top", "mostly", "often"]) {
            question.peak = Some("most".into());
        } else if has(&words, &["fewest", "least"]) {
            question.peak = Some("least".into());
        }
        for (index, word) in words.iter().enumerate() {
            if ["camera", "cameras", "fewest", "least", "took", "take", "takes", "taken", "shot", "shoot", "use", "used", "uses", "using", "device", "devices", "do", "does", "did"].contains(&word.as_str()) {
                used.insert(index);
            }
        }
    }
    let mut formats: Vec<String> = Vec::new();
    for (index, word) in words.iter().enumerate() {
        if let Some(extensions) = format_extensions(word) {
            for extension in extensions {
                if !formats.iter().any(|known| known == extension) {
                    formats.push((*extension).to_owned());
                }
            }
            used.insert(index);
        }
    }
    if !formats.is_empty() {
        question.formats = Some(formats);
    }
    let order_words = ["largest", "biggest", "heaviest", "smallest", "tiniest", "newest", "latest", "recent", "oldest", "earliest"];
    for (index, word) in words.iter().enumerate() {
        let Ok(n) = word.parse::<i64>() else { continue };
        if !(1..=500).contains(&n) || n.to_string() != *word {
            continue;
        }
        let before = index.checked_sub(1).map(|at| words[at].as_str()).unwrap_or_default();
        let after = words.get(index + 1).map(String::as_str).unwrap_or_default();
        if before == "top" || before == "first" || order_words.contains(&after) {
            question.limit = Some(n);
            used.insert(index);
            if before == "top" || before == "first" {
                used.insert(index - 1);
            }
            if before == "top" && !question.explicit_order {
                question.order = "largest".into();
                question.explicit_order = true;
            }
            break;
        }
    }
    if question.limit.is_none()
        && let Some(index) = words.iter().position(|word| order_words.contains(&word.as_str()))
        && words.get(index + 1).is_some_and(|next| SINGULAR_FILE_WORDS.contains(&next.as_str()))
    {
        question.limit = Some(1);
    }
    for index in 0..words.len().saturating_sub(1) {
        if !["per", "by", "each", "every"].contains(&words[index].as_str()) {
            continue;
        }
        let unit = words[index + 1].as_str();
        if ["size", "sizes"].contains(&unit) && !question.explicit_order {
            question.order = "largest".into();
            question.explicit_order = true;
            mark(&mut used, index..index + 2);
        } else if ["date", "time"].contains(&unit) && !question.explicit_order {
            question.order = "newest".into();
            question.explicit_order = true;
            mark(&mut used, index..index + 2);
        } else if ["year", "years", "month", "months"].contains(&unit) {
            question.breakdown = Some(if unit.starts_with("year") { "year" } else { "month" }.into());
            if question.measure == "list" {
                question.measure = "count".into();
            }
            mark(&mut used, index..index + 2);
        }
    }
    if let Some((peak, unit, range)) = peak_unit(&words) {
        question.peak = Some(peak.into());
        question.breakdown = Some(unit.into());
        if question.measure == "list" {
            question.measure = "count".into();
        }
        used.extend(range);
    }
    if has(&words, &["yearly", "annually"]) {
        question.breakdown = Some("year".into());
        if question.measure == "list" {
            question.measure = "count".into();
        }
        for (index, word) in words.iter().enumerate() {
            if word == "yearly" || word == "annually" {
                used.insert(index);
            }
        }
    }
    if question.measure == "size" && words.iter().any(|word| word == "most") && !question.explicit_order {
        question.order = "largest".into();
        question.explicit_order = true;
    }
    if let Some((minimum, maximum, range)) = size_bounds(&words) {
        question.min_bytes = minimum;
        question.max_bytes = maximum;
        used.extend(range);
    }
    let named_folder = question_folder(&words, &raw);
    let outside_folder: Vec<String> = words
        .iter()
        .enumerate()
        .map(|(index, word)| if named_folder.as_ref().is_some_and(|(_, range)| range.contains(&index)) { String::new() } else { word.clone() })
        .collect();
    if let Some((filter, range)) = date_filter(&outside_folder) {
        question.date = Some(filter);
        mark(&mut used, range);
    }
    if let Some((folder, range)) = named_folder {
        question.folder = Some(folder);
        mark(&mut used, range);
    }
    if question.structured() {
        let filler = [
            "how", "many", "much", "what", "whats", "which", "is", "are", "there", "do", "does", "did", "i", "we", "you", "have", "has", "got",
            "total", "number", "count", "use", "uses", "using", "used", "take", "takes", "taking", "up", "space", "storage", "room", "disk",
            "size", "big", "large", "show", "list", "give", "tell", "me", "top", "been", "yet", "were", "was", "taken", "made", "created",
            "shot", "recorded", "added", "any", "that", "those", "these", "by", "per", "most", "least", "first", "sorted", "files", "file",
            "my", "library", "collection", "whole", "entire", "altogether", "overall", "everything", "all",
        ];
        for (index, word) in words.iter().enumerate() {
            let names_the_library = (matches!(word.as_str(), "this" | "the" | "our")
                && words.get(index + 1).is_some_and(|next| next == "library" || next == "collection"))
                || (word == "this" && index + 1 == words.len());
            if filler.contains(&word.as_str()) || names_the_library {
                used.insert(index);
            }
        }
        for (index, pair) in words.windows(2).enumerate() {
            if pair[1] == "one" && points_back(pair) {
                used.insert(index + 1);
            }
        }
        question.reduced = raw.iter().enumerate().filter(|(index, _)| !used.contains(index)).map(|(_, word)| *word).collect::<Vec<_>>().join(" ");
    } else {
        text.clone_into(&mut question.reduced);
    }
    question
}

const SINGULAR_FILE_WORDS: &[&str] = &["video", "photo", "picture", "image", "file", "document", "clip", "pdf", "pic", "recording", "one"];

const PLACE_WORDS: &[&str] = &["photos", "pictures", "images", "library", "collection", "files"];
const COUNT_PEOPLE_TAIL: &[&str] = &[
    "in", "on", "of", "my", "our", "the", "this", "that", "whole", "entire", "library", "collection", "photos", "pictures", "images", "photo", "files",
    "there", "are", "is", "do", "i", "we", "have", "has", "got", "altogether", "overall", "total", "all", "so", "far",
    "named", "unnamed", "grouped", "identified", "recognized", "recognised", "yet", "group", "groups", "cluster", "clusters",
];
const WHO_WORDS: &[&str] = &[
    "who", "whos", "is", "are", "in", "on", "my", "our", "the", "this", "all", "photos", "pictures", "images", "library", "collection", "files",
    "there", "here", "appear", "appears", "appearing", "featured", "shown", "do", "i", "we", "have", "got", "people", "persons", "everyone", "anyone",
    "named", "unnamed",
];
const LIST_LEADS: &[&str] = &["which", "what", "whats", "list", "show", "give", "tell"];

/// "how many people are in my library?" is "count"; "who is in my photos?" / "show me the people" are "who". Anything
/// with another filter or subject keeps its normal parse.
fn people_ask(words: &[String]) -> Option<&'static str> {
    let words: Vec<&str> = words.iter().map(String::as_str).filter(|word| !word.is_empty()).collect();
    let is_people = |word: &&str| ["people", "persons"].contains(word);
    let start = words
        .windows(2)
        .position(|pair| pair == ["how", "many"] || pair == ["number", "of"])
        .map(|at| at + 2)
        .or_else(|| (words.first() == Some(&"count")).then_some(1));
    if let Some(start) = start {
        let mut at = start;
        while words.get(at).is_some_and(|word| ["different", "unique", "distinct", "named", "unnamed", "separate", "total", "the", "my", "all", "of", "group", "groups", "cluster", "clusters"].contains(word)) {
            at += 1;
        }
        return (words.get(at).is_some_and(|word| is_people(word)) && words[at + 1..].iter().all(|word| COUNT_PEOPLE_TAIL.contains(word))).then_some("count");
    }
    let place = words.iter().any(|word| PLACE_WORDS.contains(word)) || words.iter().any(|word| ["everyone", "anyone"].contains(word));
    if words.iter().any(|word| ["who", "whos", "everyone", "anyone"].contains(word)) && place && words.iter().all(|word| WHO_WORDS.contains(word)) {
        return Some("who");
    }
    let noun = words.iter().position(is_people)?;
    let leads = words.first().is_some_and(|word| LIST_LEADS.contains(word));
    let about_photos = words.get(noun + 1).is_some_and(|word| ["photos", "pictures", "images", "files", "videos"].contains(word));
    (leads && !about_photos && words.iter().all(|word| WHO_WORDS.contains(word) || LIST_LEADS.contains(word) || ["me", "a", "any"].contains(word))).then_some("who")
}

fn peak_unit(words: &[String]) -> Option<(&'static str, &'static str, Vec<usize>)> {
    let peak = words.iter().position(|word| ["most", "busiest", "highest", "fewest", "least", "lowest"].contains(&word.as_str()))?;
    if words.get(peak + 1).is_some_and(|next| ["recent", "recently", "common", "commonly"].contains(&next.as_str())) {
        return None;
    }
    let unit_at = words.iter().enumerate().position(|(index, word)| {
        let unit = ["year", "years", "month", "months", "day", "days", "week", "weeks", "hour", "hours"].contains(&word.as_str());
        let before = index.checked_sub(1).map(|at| words[at].as_str());
        unit && before.is_some_and(|before| ["which", "what", "whats", "busiest", "highest", "fewest", "lowest"].contains(&before))
    })?;
    let unit = match words[unit_at].trim_end_matches('s') {
        "year" => "year",
        "month" => "month",
        "day" => "day",
        "week" => "week",
        _ => "hour",
    };
    let extreme = if ["fewest", "least", "lowest"].contains(&words[peak].as_str()) { "least" } else { "most" };
    let mut range = vec![peak, unit_at];
    if ["busiest", "highest", "fewest", "lowest"].contains(&words[unit_at - 1].as_str()) {
        range.push(unit_at - 1);
    }
    Some((extreme, unit, range))
}

/// "bigger than 1 GB", "under 2.5 gb": byte bounds and the words used. Decimal units, as sizes are shown
/// (`ChatIntent.sizeBounds`).
pub fn size_bounds(words: &[String]) -> Option<(Option<i64>, Option<i64>, BTreeSet<usize>)> {
    let unit = |word: &str| -> Option<f64> {
        Some(match word {
            "b" | "byte" | "bytes" => 1.0,
            "kb" => 1e3,
            "mb" => 1e6,
            "gb" => 1e9,
            "tb" => 1e12,
            _ => return None,
        })
    };
    let above = ["bigger", "larger", "over", "above", "more", "exceeding", "heavier"];
    let below = ["smaller", "under", "below", "less", "lighter"];
    let amount = |index: usize| -> Option<(f64, usize)> {
        let word = words.get(index)?;
        if let Ok(number) = word.parse::<f64>()
            && number >= 0.0
            && let Some(unit) = words.get(index + 1).and_then(|next| unit(next))
        {
            return Some((number * unit, 2));
        }
        let digits = word.len() - word.trim_start_matches(|c: char| c.is_numeric() || c == '.').len();
        if digits > 0
            && let Ok(number) = word[..digits].parse::<f64>()
            && let Some(unit) = unit(&word[digits..])
        {
            return Some((number * unit, 1));
        }
        None
    };
    let (mut minimum, mut maximum) = (None, None);
    let mut used = BTreeSet::new();
    let mut index = 0;
    while index < words.len() {
        let word = words[index].as_str();
        let mut start = index + 1;
        let mut direction = if above.contains(&word) {
            Some(true)
        } else if below.contains(&word) {
            Some(false)
        } else {
            None
        };
        if word == "at" && let Some(next) = words.get(index + 1).filter(|next| *next == "least" || *next == "most") {
            direction = Some(next == "least");
            start = index + 2;
        }
        let Some(direction) = direction else {
            index += 1;
            continue;
        };
        if words.get(start).is_some_and(|word| word == "than") {
            start += 1;
        }
        let Some((bytes, length)) = amount(start).filter(|(bytes, _)| bytes.is_finite() && *bytes < 9e18) else {
            index += 1;
            continue;
        };
        let value = bytes.round() as i64;
        if direction {
            minimum = Some(value);
        } else {
            maximum = Some(value);
        }
        used.extend(index..start + length);
        index = start + length;
    }
    (!used.is_empty()).then_some((minimum, maximum, used))
}

const RAW_EXTENSIONS: [&str; 20] = [
    "raw", "arw", "cr2", "cr3", "nef", "dng", "orf", "rw2", "raf", "srw", "pef", "3fr", "erf", "kdc", "mef", "mos", "mrw", "rwl", "srf", "x3f",
];

fn format_extensions(word: &str) -> Option<&'static [&'static str]> {
    known_format(word).or_else(|| word.strip_suffix('s').and_then(known_format))
}

fn known_format(word: &str) -> Option<&'static [&'static str]> {
    Some(match word {
        "raw" => &RAW_EXTENSIONS,
        "jpg" | "jpeg" => &["jpg", "jpeg"],
        "arw" => &["arw"],
        "cr2" => &["cr2"],
        "cr3" => &["cr3"],
        "nef" => &["nef"],
        "dng" => &["dng"],
        "orf" => &["orf"],
        "raf" => &["raf"],
        "rw2" => &["rw2"],
        "png" => &["png"],
        "heic" => &["heic", "heif"],
        "gif" => &["gif"],
        "tiff" => &["tif", "tiff"],
        "webp" => &["webp"],
        "mov" => &["mov"],
        "mp4" => &["mp4"],
        "mkv" => &["mkv"],
        "avi" => &["avi"],
        "mpg" | "mpeg" => &["mpg", "mpeg"],
        "m4v" => &["m4v"],
        "wmv" => &["wmv"],
        "3gp" => &["3gp", "3g2"],
        "webm" => &["webm"],
        "mts" | "m2ts" => &["mts", "m2ts"],
        "flv" => &["flv"],
        "ogv" => &["ogv"],
        "vob" => &["vob"],
        "mxf" => &["mxf"],
        "bmp" => &["bmp"],
        "avif" => &["avif"],
        "tif" => &["tif", "tiff"],
        "heif" => &["heic", "heif"],
        "srw" => &["srw"],
        "pef" => &["pef"],
        "mp3" => &["mp3"],
        "flac" => &["flac"],
        "wav" => &["wav"],
        "m4a" => &["m4a"],
        "wma" => &["wma"],
        "docx" => &["docx"],
        "xlsx" => &["xlsx"],
        "pptx" => &["pptx"],
        "txt" => &["txt"],
        "csv" => &["csv"],
        "json" => &["json"],
        _ => return None,
    })
}

const KIND_WORDS: [&str; 22] = [
    "photo", "photos", "picture", "pictures", "image", "images", "video", "videos", "clip", "clips", "document", "documents", "pdf", "pdfs", "audio",
    "music", "song", "songs", "people", "person", "face", "faces",
];

const OVERVIEW_WORDS: [&str; 26] = [
    "what", "whats", "is", "in", "my", "the", "library", "files", "collection", "tell", "me", "about", "give", "an", "overview", "summary", "of",
    "summarize", "summarise", "describe", "everything", "all", "do", "i", "have", "got",
];

fn library_measure(words: &[String]) -> Option<(&'static str, &'static str)> {
    let words: Vec<String> = words.iter().filter(|word| !word.is_empty()).cloned().collect();
    if words.is_empty() {
        return None;
    }
    if words.iter().all(|word| OVERVIEW_WORDS.contains(&word.as_str()))
        && has(&words, &["library", "collection", "overview", "summary", "summarize", "summarise"])
    {
        return Some(("overview", "newest"));
    }
    let unknown: Vec<(usize, &String)> = words.iter().enumerate().filter(|(_, word)| !OVERVIEW_WORDS.contains(&word.as_str())).collect();
    if let [(at, word)] = unknown.as_slice()
        && let Some(library) = words.iter().position(|word| word == "library" || word == "collection")
            && *at < library
                && ["tell", "describe", "summarize", "summarise", "give", "show", "what", "whats"].contains(&words[0].as_str())
                && !KIND_WORDS.contains(&word.as_str())
                && format_extensions(word).is_none()
            {
                return Some(("overview", "newest"));
            }
    if has(&words, &["format", "formats", "extension", "extensions"])
        && has(&words, &["what", "which", "each", "every", "per", "by", "breakdown", "different", "list"])
        && !has(&words, &["folder", "folders", "directory"])
        && (!has(&words, &["in", "inside", "under", "from"]) || has(&words, &["library", "collection"]))
    {
        return Some(("formats", "newest"));
    }
    if (has(&words, &["type", "types", "kind", "kinds"]) && has(&words, &["each", "every", "per", "by", "breakdown", "different"])
        || has(&words, &["type", "types", "kinds"]) && has(&words, &["what", "which"]) && has(&words, &["files", "file"]))
        && !has(&words, &["folder", "folders", "in", "inside", "under", "from"])
    {
        return Some(("overview", "newest"));
    }
    if has(&words, &["folder", "folders", "directory", "directories"])
        && has(&words, &["most", "biggest", "largest", "top", "fullest", "heaviest"])
        && !has(&words, &["in", "inside", "under", "from"])
    {
        return Some(("folders", if has(&words, &["biggest", "largest", "heaviest"]) { "largest" } else { "newest" }));
    }
    None
}

fn question_folder(words: &[String], raw: &[&str]) -> Option<(String, std::ops::Range<usize>)> {
    for (index, word) in words.iter().enumerate() {
        if !["in", "inside", "under", "from", "of"].contains(&word.as_str()) {
            continue;
        }
        let mut at = index + 1;
        while words.get(at).is_some_and(|word| ["my", "the"].contains(&word.as_str())) {
            at += 1;
        }
        let Some(target) = words.get(at) else { continue };
        let cased = original(raw[at]);
        if is_path(&cased) {
            return Some((cased, index..at + 1));
        }
        let folder_after = words.get(at + 1).is_some_and(|word| word == "folder" || word == "directory");
        if word == "of" && !folder_after && target != "folder" && target != "directory" {
            continue;
        }
        if ["downloads", "desktop", "pictures", "movies", "music"].contains(&target.as_str()) || (folder_after && target != "folder") {
            let end = if folder_after { at + 2 } else { at + 1 };
            let mut chars = cased.chars();
            let label = chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
            return Some((format!("named:{label}"), index..end));
        }
        if (target == "folder" || target == "directory")
            && let Some(name) = raw.get(at + 1) {
                return Some((format!("named:{}", original(name)), index..at + 2));
            }
    }
    None
}

const MONTHS: [(&str, u32); 23] = [
    ("january", 1), ("jan", 1), ("february", 2), ("feb", 2), ("march", 3), ("mar", 3), ("april", 4), ("apr", 4), ("may", 5),
    ("june", 6), ("jun", 6), ("july", 7), ("jul", 7), ("august", 8), ("aug", 8), ("september", 9), ("sept", 9), ("sep", 9),
    ("october", 10), ("oct", 10), ("november", 11), ("nov", 11), ("december", 12),
];
pub(crate) const MONTH_NAMES: [&str; 12] = [
    "January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December",
];

fn month(word: &str) -> Option<u32> {
    if word == "dec" {
        return Some(12);
    }
    MONTHS.iter().find(|(name, _)| *name == word).map(|(_, number)| *number)
}

fn day(word: &str) -> Option<u32> {
    let digits = word.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let suffix = &word[digits.len()..];
    (!digits.is_empty() && digits.len() <= 2 && digits.chars().all(|c| c.is_ascii_digit()) && ["", "st", "nd", "rd", "th"].contains(&suffix))
        .then(|| digits.parse().ok())
        .flatten()
        .filter(|value| (1..=31).contains(value))
}

fn year(word: &str) -> Option<u32> {
    (word.len() == 4 && word.chars().all(|c| c.is_ascii_digit())).then(|| word.parse().ok()).flatten().filter(|value| (1900..=2100).contains(value))
}

#[derive(Clone)]
struct Span {
    filter: DateFilter,
    length: usize,
}

fn exact(from: Vec<String>, to: Vec<String>, label: String) -> DateFilter {
    DateFilter { from: Some(from), to: Some(to), phrase: label, ..DateFilter::default() }
}

fn calendar(y: u32, m: u32, d: u32) -> Option<String> {
    let days = [31, if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    (d <= days[m as usize - 1]).then(|| format!("{y:04}-{m:02}-{d:02}"))
}

fn date_at(words: &[String], index: usize) -> Option<Span> {
    let word = words[index].as_str();
    let next = words.get(index + 1).map(String::as_str).unwrap_or_default();
    let modifiers = |parts: &[&str]| parts.iter().map(|part| (*part).to_owned()).collect::<Vec<_>>();
    match word {
        "today" => return Some(Span { filter: exact(modifiers(&["now", "localtime", "start of day"]), modifiers(&["now", "localtime", "start of day", "+1 day"]), "today".into()), length: 1 }),
        "yesterday" => return Some(Span { filter: exact(modifiers(&["now", "localtime", "start of day", "-1 day"]), modifiers(&["now", "localtime", "start of day"]), "yesterday".into()), length: 1 }),
        "this" | "last" | "past" if ["week", "month", "year"].contains(&next) => {
            let start: &[&str] = match next {
                "week" => &["now", "localtime", "start of day", "-6 days", "weekday 1"],
                "month" => &["now", "localtime", "start of month"],
                _ => &["now", "localtime", "start of year"],
            };
            let step = format!("+1 {next}");
            let back = format!("-1 {next}");
            let step = if next == "week" { "+7 days".to_owned() } else { step };
            let back = if next == "week" { "-7 days".to_owned() } else { back };
            let label = format!("{word} {next}");
            let filter = match word {
                "this" => exact([modifiers(start), vec![]].concat(), [modifiers(start), vec![step]].concat(), label),
                "last" => exact([modifiers(start), vec![back]].concat(), modifiers(start), label),
                _ => DateFilter { from: Some(vec!["now".into(), back]), phrase: format!("the past {next}"), ..DateFilter::default() },
            };
            return Some(Span { filter, length: 2 });
        }
        _ => {}
    }
    let digits: Vec<&str> = word.split('-').collect();
    if digits.len() == 3 && digits[0].len() == 4
        && let (Some(y), Ok(m), Ok(d)) = (year(digits[0]), digits[1].parse::<u32>(), digits[2].parse::<u32>())
            && digits[1].len() == 2 && digits[2].len() == 2 && (1..=12).contains(&m) {
                let date = calendar(y, m, d)?;
                return Some(Span { filter: exact(vec![date.clone()], vec![date, "+1 day".into()], format!("{} {d}, {y}", MONTH_NAMES[m as usize - 1])), length: 1 });
            }
    if digits.len() == 2 && digits[0].len() == 4 && digits[1].len() == 2
        && let (Some(y), Ok(m)) = (year(digits[0]), digits[1].parse::<u32>())
            && (1..=12).contains(&m) {
                return Some(month_year(y, m, 1));
            }
    let slashes: Vec<&str> = word.split('/').collect();
    if (2..=3).contains(&slashes.len()) && slashes.iter().all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit())) {
        let m: u32 = slashes[0].parse().ok()?;
        let d: u32 = slashes[1].parse().ok()?;
        if (1..=12).contains(&m) && (1..=31).contains(&d) && slashes[0].len() <= 2 && slashes[1].len() <= 2 {
            if let Some(y) = slashes.get(2) {
                let y: u32 = if y.len() == 2 { 2000 + y.parse::<u32>().ok()? } else { year(y)? };
                let date = calendar(y, m, d)?;
                return Some(Span { filter: exact(vec![date.clone()], vec![date, "+1 day".into()], format!("{} {d}, {y}", MONTH_NAMES[m as usize - 1])), length: 1 });
            }
            return Some(month_day(m, d, 1));
        }
    }
    let season = match word {
        "spring" => Some(3),
        "summer" => Some(6),
        "fall" | "autumn" => Some(9),
        "winter" => Some(12),
        _ => None,
    };
    if let Some(first) = season {
        if let Some(y) = year(next) {
            let start = format!("{y:04}-{first:02}-01");
            return Some(Span { filter: exact(vec![start.clone()], vec![start, "+3 months".into()], format!("{word} {y}")), length: 2 });
        }
        let months = (0..3).map(|offset| format!("{:02}", (first - 1 + offset) % 12 + 1)).collect();
        return Some(Span {
            filter: DateFilter { months: Some(months), phrase: format!("{word} (any year)"), ..DateFilter::default() },
            length: 1,
        });
    }
    if let Some(m) = month(word) {
        if let Some(d) = day(next) {
            if let Some(y) = words.get(index + 2).and_then(|word| year(word)) {
                let date = calendar(y, m, d)?;
                return Some(Span { filter: exact(vec![date.clone()], vec![date, "+1 day".into()], format!("{} {d}, {y}", MONTH_NAMES[m as usize - 1])), length: 3 });
            }
            return Some(month_day(m, d, 2));
        }
        if let Some(y) = year(next) {
            return Some(month_year(y, m, 2));
        }
        return Some(Span { filter: DateFilter { month: Some(format!("{m:02}")), phrase: format!("{} (any year)", MONTH_NAMES[m as usize - 1]), ..DateFilter::default() }, length: 1 });
    }
    if let Some(d) = day(word) {
        let of = usize::from(next == "of");
        if let Some(m) = words.get(index + 1 + of).and_then(|word| month(word)) {
            if let Some(y) = words.get(index + 2 + of).and_then(|word| year(word)) {
                let date = calendar(y, m, d)?;
                return Some(Span { filter: exact(vec![date.clone()], vec![date, "+1 day".into()], format!("{} {d}, {y}", MONTH_NAMES[m as usize - 1])), length: 3 + of });
            }
            return Some(month_day(m, d, 2 + of));
        }
    }
    if let Some(y) = year(word) {
        let start = format!("{y:04}-01-01");
        return Some(Span { filter: exact(vec![start.clone()], vec![start, "+1 year".into()], y.to_string()), length: 1 });
    }
    None
}

fn month_year(y: u32, m: u32, length: usize) -> Span {
    let start = format!("{y:04}-{m:02}-01");
    Span { filter: exact(vec![start.clone()], vec![start, "+1 month".into()], format!("{} {y}", MONTH_NAMES[m as usize - 1])), length }
}

fn month_day(m: u32, d: u32, length: usize) -> Span {
    Span {
        filter: DateFilter { month_day: Some(format!("{m:02}-{d:02}")), phrase: format!("{} {d} (any year)", MONTH_NAMES[m as usize - 1]), ..DateFilter::default() },
        length,
    }
}

/// A bare year or month name needs a preposition before it.
const MEDIA_NOUNS: [&str; 10] = ["photos", "photo", "pictures", "picture", "pics", "images", "videos", "video", "clips", "shots"];

fn date_filter(words: &[String]) -> Option<(DateFilter, std::ops::Range<usize>)> {
    let prepositions = ["from", "in", "on", "during", "since", "before", "after", "of", "between", "taken", "around"];
    for index in 0..words.len() {
        let Some(span) = date_at(words, index) else { continue };
        let previous = index.checked_sub(1).map(|at| words[at].as_str()).unwrap_or_default();
        let bare = span.length == 1 && (month(&words[index]).is_some() || year(&words[index]).is_some());
        if bare && !prepositions.contains(&previous) {
            continue;
        }
        // A season alone needs a media word after it or a preposition before it, so "spring cleaning" stays a subject.
        let seasonal = span.filter.months.is_some();
        let after_the = previous == "the" && index >= 2 && prepositions.contains(&words[index - 2].as_str());
        let media_next = words.get(index + 1).is_some_and(|word| MEDIA_NOUNS.contains(&word.as_str()));
        if seasonal && !prepositions.contains(&previous) && !after_the && !media_next {
            continue;
        }
        let start = if seasonal && after_the {
            index - 2
        } else if prepositions.contains(&previous) {
            index - 1
        } else {
            index
        };
        let end = index + span.length;
        let absolute = span.filter.month_day.is_none() && span.filter.month.is_none() && !seasonal;
        let mut filter = span.filter.clone();
        match previous {
            "since" if absolute => {
                filter = DateFilter { from: span.filter.from.clone(), phrase: format!("since {}", span.filter.phrase), ..DateFilter::default() };
            }
            "after" if absolute => {
                filter = DateFilter { from: span.filter.to.clone(), phrase: format!("after {}", span.filter.phrase), ..DateFilter::default() };
            }
            "before" if absolute => {
                filter = DateFilter { to: span.filter.from.clone(), phrase: format!("before {}", span.filter.phrase), ..DateFilter::default() };
            }
            "between" if absolute && words.get(end).is_some_and(|word| word == "and") => {
                if let Some(second) = (end + 1 < words.len()).then(|| date_at(words, end + 1)).flatten()
                    && second.filter.month_day.is_none() && second.filter.month.is_none() {
                        let filter = DateFilter {
                            from: span.filter.from.clone(),
                            to: second.filter.to.clone(),
                            phrase: format!("between {} and {}", span.filter.phrase, second.filter.phrase),
                            ..DateFilter::default()
                        };
                        return Some((filter, start..end + 1 + second.length));
                    }
                filter.phrase = format!("from {}", span.filter.phrase);
            }
            _ => filter.phrase = format!("from {}", span.filter.phrase),
        }
        return Some((filter, start..end));
    }
    None
}

/// Decimal units, round-half-up tenths, identical in both engines.
pub fn bytes_label(bytes: i64) -> String {
    let bytes = bytes.max(0);
    if bytes < 1000 {
        return if bytes == 1 { "1 byte".into() } else { format!("{bytes} bytes") };
    }
    let units = ["KB", "MB", "GB", "TB", "PB"];
    let mut unit = 1000i64;
    let mut index = 0;
    while index + 1 < units.len() && bytes >= unit * 1000 {
        unit *= 1000;
        index += 1;
    }
    let step = unit / 10;
    let mut tenths = (bytes + step / 2) / step;
    if tenths >= 10_000 && index + 1 < units.len() {
        index += 1;
        let step = step * 1000;
        tenths = (bytes + step / 2) / step;
    }
    format!("{}.{} {}", tenths / 10, tenths % 10, units[index])
}

pub use crate::util::display::{count_label, count_noun};

pub fn noun(kinds: &[String], count: i64) -> String {
    let one = count == 1;
    let label = |kind: &str| -> &str {
        match (kind, one) {
            ("image", true) => "photo",
            ("image", false) => "photos",
            ("video", true) => "video",
            ("video", false) => "videos",
            ("doc", true) => "document",
            ("doc", false) => "documents",
            ("pdf", true) => "PDF",
            ("pdf", false) => "PDFs",
            ("audio", true) => "audio file",
            ("audio", false) => "audio files",
            ("model", true) => "3D model",
            ("model", false) => "3D models",
            ("other", true) => "other file",
            ("other", false) => "other files",
            (_, true) => "file",
            _ => "files",
        }
    };
    let mut kinds: Vec<&str> = kinds.iter().map(String::as_str).collect();
    kinds.sort_unstable();
    kinds.dedup();
    if kinds == ["doc", "pdf"] {
        return if one { "document".into() } else { "documents".into() };
    }
    let labels: Vec<&str> = kinds.iter().map(|kind| label(kind)).collect();
    match labels.len() {
        0 => label("").into(),
        1 => labels[0].into(),
        count => format!("{} and {}", labels[..count - 1].join(", "), labels[count - 1]),
    }
}

const NAME_LEADS: [&str; 5] = ["of", "with", "featuring", "including", "showing"];
const NOT_NAMES: &[&str] = &[
    "january", "february", "march", "april", "may", "june", "july", "august", "september", "october", "november", "december",
    "monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday",
    "christmas", "halloween", "thanksgiving", "easter", "hanukkah", "diwali", "valentines", "new", "summer", "winter",
    "spring", "fall", "autumn", "iphone", "ipad", "mac", "macbook", "google", "apple", "instagram", "facebook", "whatsapp",
];

fn capitalised_word(word: &str) -> bool {
    let mut letters = word.chars();
    letters.next().is_some_and(char::is_uppercase)
        && word.chars().all(|c| c.is_alphabetic() || matches!(c, '\'' | '’' | '-'))
        && word.chars().any(char::is_lowercase)
}

/// One or two capitalised words after of/with/featuring/including/showing; longer runs are folder or event titles, and
/// months, weekdays, holidays and brands are not people (`ChatIntent.spokenName`, corpus `spokenNames`).
pub fn spoken_name(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().map(|word| word.trim_matches(|c: char| !c.is_alphanumeric())).collect();
    let lead = words.iter().position(|word| NAME_LEADS.contains(&word.to_lowercase().as_str()))?;
    let run: Vec<&str> = words[lead + 1..].iter().copied().take_while(|word| capitalised_word(word)).collect();
    let first = run.first()?.to_lowercase();
    ((1..=2).contains(&run.len()) && !NOT_NAMES.contains(&first.as_str())).then(|| run.join(" "))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reference {
    Nth(usize),
    Last,
    Single,
}

const ORDINALS: [(&str, usize); 10] = [
    ("first", 1), ("second", 2), ("third", 3), ("fourth", 4), ("fifth", 5), ("sixth", 6), ("seventh", 7), ("eighth", 8), ("ninth", 9), ("tenth", 10),
];
const REFERENCE_FILLER: [&str; 46] = [
    "show", "me", "tell", "about", "what", "whats", "is", "are", "was", "how", "the", "a", "an", "that", "this", "it", "one", "of", "details", "detail",
    "info", "more", "please", "give", "which", "and", "then", "just", "result", "file", "item", "photo", "video", "picture", "image", "clip", "open",
    "play", "view", "display", "watch", "preview", "launch", "see", "select", "pick",
];

fn ordinal(word: &str) -> Option<usize> {
    if let Some((_, n)) = ORDINALS.iter().find(|(name, _)| *name == word) {
        return Some(*n);
    }
    let digits = word.trim_end_matches(|c: char| c.is_ascii_alphabetic());
    let suffix = &word[digits.len()..];
    (["st", "nd", "rd", "th"].contains(&suffix))
        .then(|| digits.parse::<usize>().ok())
        .flatten()
        .filter(|n| (1..=50).contains(n))
}

/// "the second one", "tell me about it", "open the third one", "play it": a message that only points at something the previous answer listed.
pub fn reference_ask(text: &str) -> Option<Reference> {
    let words: Vec<String> = text.split_whitespace().map(|word| clean(word).replace('\'', "")).filter(|word| !word.is_empty()).collect();
    if words.is_empty() {
        return None;
    }
    let mut found = None;
    for word in &words {
        if let Some(n) = ordinal(word) {
            found = Some(Reference::Nth(n));
        } else if word == "last" || word == "final" {
            found = Some(Reference::Last);
        } else if !REFERENCE_FILLER.contains(&word.as_str()) {
            return None;
        }
    }
    if found.is_some() {
        return found;
    }
    let single = has(&words, &["it"]) || words.windows(2).any(|pair| matches!(pair, [a, b] if ["that", "this", "the"].contains(&a.as_str()) && b == "one"));
    single.then_some(Reference::Single)
}

const ORDER_WORDS: [&str; 12] = [
    "biggest", "largest", "heaviest", "smallest", "tiniest", "newest", "latest", "oldest", "earliest", "recent", "first", "last",
];

/// The message refers back to the previous answer ("those", "the biggest one") instead of naming a new subject.
pub fn points_back(words: &[String]) -> bool {
    has(words, &["those", "these", "them", "they", "ones"])
        || words.windows(2).any(|pair| {
            (pair[1] == "one"
                && (["the", "this", "that", "which"].contains(&pair[0].as_str()) || ORDER_WORDS.contains(&pair[0].as_str()) || ordinal(&pair[0]).is_some()))
                || (["in", "inside", "within"].contains(&pair[0].as_str()) && ["it", "there"].contains(&pair[1].as_str()))
        })
}

/// "And photos?", "What about videos?": what follows a short follow-up that swaps one part of the previous question and
/// keeps its folder and dates.
pub fn follow_up_rest(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let lead = |at: usize| words.get(at).map(|word| clean(word));
    let skip = match lead(0).as_deref() {
        Some("and") => 1,
        Some("what" | "how") if lead(1).as_deref() == Some("about") => 2,
        _ => return None,
    };
    let rest = &words[skip..];
    (!rest.is_empty() && rest.len() <= 4).then(|| rest.join(" ").trim_end_matches(['?', '!', '.']).to_owned())
}

const OFF_TOPIC_WORDS: [&str; 12] = ["weather", "forecast", "joke", "jokes", "news", "horoscope", "recipe", "recipes", "lyrics", "stocks", "translate", "translation"];
const OFF_TOPIC_PHRASES: [&str; 4] = ["who won", "what time is it", "tell me a story", "capital of"];
const FILE_WORDS: [&str; 20] = [
    "file", "files", "photo", "photos", "picture", "pictures", "image", "images", "video", "videos", "clip", "clips", "document", "documents", "pdf", "pdfs",
    "folder", "folders", "library", "screenshots",
];

/// A general-assistant question ("what's the weather", "tell me a joke") that names nothing in the library.
pub fn off_topic(text: &str) -> bool {
    let lower = text.to_lowercase();
    let words: Vec<String> = lower.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_owned).collect();
    let asks = words.iter().any(|word| OFF_TOPIC_WORDS.contains(&word.as_str())) || OFF_TOPIC_PHRASES.iter().any(|phrase| lower.contains(phrase));
    asks && !words.iter().any(|word| FILE_WORDS.contains(&word.as_str()))
}

const TAKEN_WORDS: [&str; 6] = ["taken", "shot", "photographed", "filmed", "recorded", "captured"];
const GEOTAG_WORDS: [&str; 5] = ["gps", "geotagged", "geotag", "geotags", "geolocated"];

/// Returns the kinds named (empty for every file) (`ChatIntent.placeAsk`, corpus `placeAsks`).
pub fn place_ask(text: &str) -> Option<Vec<String>> {
    let words: Vec<String> = text.split_whitespace().map(|word| clean(word).replace('\'', "")).collect();
    let asks = (has(&words, &["where"]) && has(&words, &TAKEN_WORDS))
        || has(&words, &GEOTAG_WORDS)
        || (has(&words, &["location", "locations"]) && has(&words, &["have", "has", "recorded", "saved", "stored", "with", "many"]));
    if !asks {
        return None;
    }
    let mut kinds = Vec::new();
    for word in &words {
        let kind = match word.as_str() {
            "photo" | "photos" | "picture" | "pictures" | "image" | "images" | "pics" => "image",
            "video" | "videos" | "clip" | "clips" => "video",
            _ => continue,
        };
        if !kinds.iter().any(|known| known == kind) {
            kinds.push(kind.to_owned());
        }
    }
    kinds.sort();
    Some(kinds)
}

/// Left for the catalog to say whether it is a folder (`ChatIntent.askedInside`, corpus `insideAsks`).
pub fn asked_inside(text: &str) -> Option<String> {
    let raw: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<String> = raw.iter().map(|word| clean(word).replace('\'', "")).collect();
    let mut at = match words.first().map(String::as_str)? {
        "whats" => 1,
        "what" if words.get(1).is_some_and(|word| word == "is" || word == "are") => 2,
        _ => return None,
    };
    while words.get(at).is_some_and(|word| word == "all" || word == "everything") {
        at += 1;
    }
    if !words.get(at).is_some_and(|word| word == "in" || word == "inside") {
        return None;
    }
    at += 1;
    if words.get(at).is_some_and(|word| word == "the") {
        at += 1;
    }
    let mut name: Vec<String> = raw[at.min(raw.len())..].iter().map(|word| original(word)).filter(|word| !word.is_empty()).collect();
    if name.last().is_some_and(|word| ["folder", "directory"].contains(&word.to_lowercase().as_str())) {
        name.pop();
    }
    let first = name.first()?.to_lowercase();
    (!["this", "that", "these", "those", "my", "our", "it", "here", "there", "library", "collection"].contains(&first.as_str())).then(|| name.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Fixtures {
        intents: Vec<IntentCase>,
        questions: Vec<QuestionCase>,
        labels: Vec<(i64, String)>,
        #[serde(rename = "spokenNames")]
        spoken_names: Vec<SpokenNameCase>,
        #[serde(rename = "placeAsks")]
        place_asks: Vec<PlaceAskCase>,
        #[serde(rename = "insideAsks")]
        inside_asks: Vec<SpokenNameCase>,
    }

    #[derive(Deserialize)]
    struct PlaceAskCase {
        text: String,
        kinds: Option<Vec<String>>,
    }

    #[derive(Deserialize)]
    struct SpokenNameCase {
        text: String,
        name: Option<String>,
    }
    #[derive(Deserialize)]
    struct IntentCase {
        text: String,
        intent: Option<Intent>,
    }
    #[derive(Deserialize)]
    struct QuestionCase {
        text: String,
        question: Question,
    }

    fn fixtures() -> Fixtures {
        serde_json::from_str(include_str!("../../../../../../shared/test-corpus/chat-intents.json")).unwrap()
    }

    #[test]
    fn shared_intent_cases() {
        let failures: Vec<String> = fixtures()
            .intents
            .into_iter()
            .filter_map(|case| {
                let actual = parse_intent(&case.text);
                (actual != case.intent).then(|| format!("{} → {}", case.text, serde_json::to_string(&actual).unwrap()))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn shared_question_cases() {
        let failures: Vec<String> = fixtures()
            .questions
            .into_iter()
            .filter_map(|case| {
                let actual = parse_question(&case.text);
                (actual != case.question).then(|| format!("{} → {}", case.text, serde_json::to_string(&actual).unwrap()))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn shared_spoken_names() {
        let failures: Vec<String> = fixtures()
            .spoken_names
            .into_iter()
            .filter_map(|case| {
                let actual = spoken_name(&case.text);
                (actual != case.name).then(|| format!("{} → {actual:?}", case.text))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn shared_place_asks() {
        let failures: Vec<String> = fixtures()
            .place_asks
            .into_iter()
            .filter_map(|case| {
                let actual = place_ask(&case.text);
                (actual != case.kinds).then(|| format!("{} → {actual:?}", case.text))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn shared_inside_asks() {
        let failures: Vec<String> = fixtures()
            .inside_asks
            .into_iter()
            .filter_map(|case| {
                let actual = asked_inside(&case.text);
                (actual != case.name).then(|| format!("{} → {actual:?}", case.text))
            })
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn shared_labels() {
        for (bytes, label) in fixtures().labels {
            assert_eq!(bytes_label(bytes), label, "{bytes}");
        }
        assert_eq!(count_label(1_234_567), "1,234,567");
        assert_eq!(count_label(999), "999");
        assert_eq!(noun(&["doc".into(), "pdf".into()], 2), "documents");
        assert_eq!(noun(&["image".into(), "video".into()], 2), "photos and videos");
        assert_eq!(noun(&[], 1), "file");
    }

    #[test]
    fn verbs_before_an_ordinal_or_pronoun_still_point_at_the_previous_answer() {
        for (text, want) in [
            ("Open the third one", Some(Reference::Nth(3))),
            ("Open third one", Some(Reference::Nth(3))),
            ("Play the second one", Some(Reference::Nth(2))),
            ("please show me the 4th one", Some(Reference::Nth(4))),
            ("open the last one", Some(Reference::Last)),
            ("open it", Some(Reference::Single)),
            ("play that one", Some(Reference::Single)),
            ("open my taxes", None),
            ("Open third folder", None),
            ("open settings", None),
        ] {
            assert_eq!(reference_ask(text), want, "{text}");
        }
    }

    #[test]
    fn short_follow_ups_and_inside_it_keep_the_previous_scope() {
        assert_eq!(follow_up_rest("And photos?").as_deref(), Some("photos"));
        assert_eq!(follow_up_rest("What about 2012?").as_deref(), Some("2012"));
        assert_eq!(follow_up_rest("how about November 2012").as_deref(), Some("November 2012"));
        assert_eq!(follow_up_rest("And show me the photos from the trip to Spain last summer"), None);
        assert_eq!(follow_up_rest("How many photos?"), None);
        let words = |text: &str| -> Vec<String> { text.to_lowercase().split(|c: char| !c.is_alphabetic()).map(str::to_owned).collect() };
        assert!(points_back(&words("How many videos in it?")));
        assert!(points_back(&words("and what is inside there")));
        assert!(!points_back(&words("How many videos in Italy?")));
    }

    #[test]
    fn general_assistant_questions_are_off_topic_unless_they_name_files() {
        for text in ["What's the weather?", "What's the weather today?", "Who won the game last night?", "Tell me a joke", "give me a recipe for soup"] {
            assert!(off_topic(text), "{text}");
        }
        for text in ["photos of the weather vane", "Find the weather report PDF", "How many photos do I have?", "videos from the news segment", "scooter"] {
            assert!(!off_topic(text), "{text}");
        }
    }

    #[test]
    fn this_library_is_not_a_search_subject() {
        for text in ["How many videos are in this library?", "how many photos are in the collection", "count the videos in our library"] {
            let question = parse_question(text);
            assert_eq!(question.measure, "count", "{text}");
            assert!(!question.reduced.split_whitespace().any(|word| ["this", "our", "collection"].contains(&word)), "{text} → {}", question.reduced);
        }
        assert_eq!(parse_question("how many photos from this trip").reduced, "photos from this trip");
    }
}
