//! Deterministic Chat parsing. Mirrors macOS `ChatIntent.swift` line for line;
//! the shared cases live in `shared/test-corpus/chat-intents.json`.
//!
//! Only the user's own words are parsed. Retrieved file text never reaches
//! these functions, so instructions inside documents cannot start actions.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// An imperative request, before it is resolved against the library.
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

/// A date window over `COALESCE(created_at, modified_at)`. `from`/`to` are
/// SQLite date-modifier lists, evaluated in local time and converted to UTC.
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
    pub phrase: String,
}

/// The structured part of a question: what to measure and which filters apply.
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
    /// The message without the words the structure consumed.
    pub reduced: String,
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
            reduced: String::new(),
        }
    }
}

impl Question {
    pub fn structured(&self) -> bool {
        self.measure != "list" || self.explicit_order || self.date.is_some() || self.folder.is_some() || self.duplicates || self.undescribed
    }
}

const QUESTION_WORDS: [&str; 16] = [
    "how", "what", "which", "where", "when", "who", "why", "is", "are", "do", "does", "did", "was", "were", "has", "have",
];
const KNOWN_FOLDERS: [&str; 9] = ["downloads", "desktop", "documents", "pictures", "movies", "videos", "music", "home", "docs"];

/// Lowercased word with surrounding punctuation removed; inner punctuation stays.
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

/// Strips polite lead-ins and trailing courtesy words. Returns cleaned and
/// original-case words, parallel.
fn imperative_words(text: &str) -> (Vec<String>, Vec<String>) {
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

/// A folder reference inside an imperative: a path, a known home folder,
/// `this`, `library` or `named:<folder>`.
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
            if before.is_some_and(|name| !["the", "my", "a", "whole", "entire"].contains(&name)) {
                return Some(format!("named:{}", cased[index - 1]));
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

/// Parses an imperative request. `None` means the message is a question or
/// search; `refuse` means a request Chat will not turn into an action.
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
        return Some(Intent { scope: rest_text(&words, 1), ..Intent::new("findDuplicates") });
    }
    if ["open", "go", "switch", "take", "view", "show"].contains(&verb) {
        if let Some(page) = page_for(rest) {
            return Some(Intent { page: Some(page.into()), ..Intent::new("openPage") });
        }
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
        if let Some(target) = targets.iter().find(|target| starts(rest, target)) {
            if let Some(name) = person_name(&cased[1 + target.len()..], false) {
                return Some(Intent { name: Some(name), ..Intent::new("namePerson") });
            }
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
    if let Some(same) = words.windows(3).position(|window| window == ["the", "same", "person"]) {
        if same >= 2 && ["are", "is"].contains(&words[same - 1].as_str()) {
            let names = &cased[..same - 1];
            let lower = &words[..same - 1];
            if let Some(split) = lower.iter().position(|word| word == "and") {
                let people = vec![names[..split].join(" "), names[split + 1..].join(" ")];
                if people.iter().all(|name| !name.is_empty()) {
                    return Some(Intent { people, ..Intent::new("mergePeople") });
                }
            }
        }
    }
    let library_words = ["the", "my", "library", "files", "scan", "index", "catalog", "everything", "again", "all"];
    if ["rescan", "re-scan", "reindex", "re-index", "refresh", "update"].contains(&verb) && rest.iter().all(|word| library_words.contains(&word.as_str())) {
        return Some(Intent { folder: Some("library".into()), ..Intent::new("rescan") });
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

/// Parses the structured part of a question (counts, sizes, ordering, dates,
/// folders, duplicates, descriptions).
pub fn parse_question(text: &str) -> Question {
    let raw: Vec<&str> = text.split_whitespace().collect();
    let words: Vec<String> = raw.iter().map(|word| clean(word).replace('\'', "")).collect();
    let mut question = Question::default();
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
    if let Some((filter, range)) = date_filter(&words) {
        question.date = Some(filter);
        mark(&mut used, range);
    }
    if let Some((folder, range)) = question_folder(&words, &raw) {
        question.folder = Some(folder);
        mark(&mut used, range);
    }
    if question.structured() {
        let filler = [
            "how", "many", "much", "what", "which", "is", "are", "there", "do", "does", "did", "i", "we", "you", "have", "has", "got",
            "total", "number", "count", "use", "uses", "using", "used", "take", "takes", "taking", "up", "space", "storage", "room", "disk",
            "size", "big", "large", "show", "list", "give", "tell", "me", "top", "been", "yet", "were", "was", "taken", "made", "created",
            "shot", "recorded", "added", "any", "that", "those", "these", "by", "per", "most", "least", "first", "sorted", "files", "file",
        ];
        for (index, word) in words.iter().enumerate() {
            if filler.contains(&word.as_str()) {
                used.insert(index);
            }
        }
        question.reduced = raw.iter().enumerate().filter(|(index, _)| !used.contains(index)).map(|(_, word)| *word).collect::<Vec<_>>().join(" ");
    } else {
        text.clone_into(&mut question.reduced);
    }
    question
}

fn question_folder(words: &[String], raw: &[&str]) -> Option<(String, std::ops::Range<usize>)> {
    for (index, word) in words.iter().enumerate() {
        if !["in", "inside", "under", "from"].contains(&word.as_str()) {
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
        if ["downloads", "desktop", "pictures", "movies", "music"].contains(&target.as_str()) || (folder_after && target != "folder") {
            let end = if folder_after { at + 2 } else { at + 1 };
            let mut chars = cased.chars();
            let label = chars.next().map(|first| first.to_uppercase().collect::<String>() + chars.as_str()).unwrap_or_default();
            return Some((format!("named:{label}"), index..end));
        }
        if target == "folder" || target == "directory" {
            if let Some(name) = raw.get(at + 1) {
                return Some((format!("named:{}", original(name)), index..at + 2));
            }
        }
    }
    None
}

const MONTHS: [(&str, u32); 23] = [
    ("january", 1), ("jan", 1), ("february", 2), ("feb", 2), ("march", 3), ("mar", 3), ("april", 4), ("apr", 4), ("may", 5),
    ("june", 6), ("jun", 6), ("july", 7), ("jul", 7), ("august", 8), ("aug", 8), ("september", 9), ("sept", 9), ("sep", 9),
    ("october", 10), ("oct", 10), ("november", 11), ("nov", 11), ("december", 12),
];
const MONTH_NAMES: [&str; 12] = [
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

/// A single date expression: its window and how many words it spans.
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
    if digits.len() == 3 && digits[0].len() == 4 {
        if let (Some(y), Ok(m), Ok(d)) = (year(digits[0]), digits[1].parse::<u32>(), digits[2].parse::<u32>()) {
            if digits[1].len() == 2 && digits[2].len() == 2 && (1..=12).contains(&m) {
                let date = calendar(y, m, d)?;
                return Some(Span { filter: exact(vec![date.clone()], vec![date, "+1 day".into()], format!("{} {d}, {y}", MONTH_NAMES[m as usize - 1])), length: 1 });
            }
        }
    }
    if digits.len() == 2 && digits[0].len() == 4 && digits[1].len() == 2 {
        if let (Some(y), Ok(m)) = (year(digits[0]), digits[1].parse::<u32>()) {
            if (1..=12).contains(&m) {
                return Some(month_year(y, m, 1));
            }
        }
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

/// The first date expression and the words it consumed, including its
/// preposition. A bare year or month name needs a preposition before it.
fn date_filter(words: &[String]) -> Option<(DateFilter, std::ops::Range<usize>)> {
    let prepositions = ["from", "in", "on", "during", "since", "before", "after", "of", "between", "taken", "around"];
    for index in 0..words.len() {
        let Some(span) = date_at(words, index) else { continue };
        let previous = index.checked_sub(1).map(|at| words[at].as_str()).unwrap_or_default();
        let bare = span.length == 1 && (month(&words[index]).is_some() || year(&words[index]).is_some());
        if bare && !prepositions.contains(&previous) {
            continue;
        }
        let start = if prepositions.contains(&previous) { index - 1 } else { index };
        let end = index + span.length;
        let absolute = span.filter.month_day.is_none() && span.filter.month.is_none();
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
                if let Some(second) = (end + 1 < words.len()).then(|| date_at(words, end + 1)).flatten() {
                    if second.filter.month_day.is_none() && second.filter.month.is_none() {
                        let filter = DateFilter {
                            from: span.filter.from.clone(),
                            to: second.filter.to.clone(),
                            phrase: format!("between {} and {}", span.filter.phrase, second.filter.phrase),
                            ..DateFilter::default()
                        };
                        return Some((filter, start..end + 1 + second.length));
                    }
                }
                filter.phrase = format!("from {}", span.filter.phrase);
            }
            _ => filter.phrase = format!("from {}", span.filter.phrase),
        }
        return Some((filter, start..end));
    }
    None
}

/// "1.2 GB" with decimal units and round-half-up tenths, identical in both engines.
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

/// "15,021".
pub fn count_label(value: i64) -> String {
    let digits = value.abs().to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    if value < 0 {
        format!("-{grouped}")
    } else {
        grouped
    }
}

/// Plural or singular noun for a set of kinds.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Fixtures {
        intents: Vec<IntentCase>,
        questions: Vec<QuestionCase>,
        labels: Vec<(i64, String)>,
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
}
