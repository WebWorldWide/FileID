//! Saved smart-name styles built only from stored catalog evidence.
//! Lockstep with macOS `SmartNameStyle.swift`; both run
//! `shared/test-corpus/naming/style-cases.json`. Text is handled per Unicode
//! scalar (lowercase mapping, Alphabetic, numeric categories) so both engines
//! produce byte-identical names.

use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Short,
    Descriptive,
    DateFirst,
}

impl Style {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "short" => Some(Self::Short),
            "descriptive" => Some(Self::Descriptive),
            "dateFirst" => Some(Self::DateFirst),
            _ => None,
        }
    }

    fn core_limit(self) -> usize {
        if self == Self::Descriptive { 6 } else { 3 }
    }
}

#[derive(Clone, Debug)]
pub struct Options {
    pub style: Style,
    pub max_length: usize,
    pub people: bool,
    pub event: bool,
    pub date: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Evidence {
    pub original_stem: String,
    pub chapter: Option<String>,
    pub stored_name: Option<String>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub person_words: HashSet<String>,
    pub people: Vec<String>,
    pub event: Option<String>,
    pub date: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outcome {
    pub stem: Option<String>,
    pub reason: &'static str,
}

pub const MIN_LENGTH: usize = 4;
pub const MAX_LENGTH: usize = 120;
const MAX_STEM_BYTES: usize = 200;
const MAX_EVENT_WORDS: usize = 4;
const MAX_TAGS: usize = 3;

const STOP: &[&str] = &[
    "a", "an", "the", "of", "in", "on", "at", "to", "for", "with", "and", "or", "but", "from", "by",
    "into", "onto", "over", "under", "near", "while", "during", "after", "before", "about", "above",
    "below", "between", "through", "across", "around", "along", "against", "its", "his", "her",
    "their", "our", "my", "your", "this", "that", "these", "those", "it", "as", "some", "there",
    "here", "who", "which", "what", "whose", "is", "are", "was", "were", "be", "being", "been",
    "has", "have", "had", "does", "do", "did", "can", "will", "very", "also", "just", "photo",
    "photograph", "photos", "picture", "pictures", "image", "images", "video", "videos", "clip",
    "file", "files", "document", "documents", "untitled", "filename", "screenshot", "shows",
    "show", "showing", "shown", "depicts", "depicting", "depicted", "displays", "displaying",
    "displayed", "features", "featuring", "captured", "taken", "scene", "view", "close",
    "closeup", "up", "front", "background", "foreground", "side", "top", "bottom", "center",
    "middle", "person", "people", "someone", "something",
];

const LABEL_GENERIC: &[&str] =
    &["untitled", "chapter", "moment", "marker", "event", "file", "photo", "image", "video", "document"];

const RESERVED: &[&str] = &[
    "con", "prn", "aux", "nul", "com0", "com1", "com2", "com3", "com4", "com5", "com6", "com7",
    "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];

pub fn lower(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

fn is_mark(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

fn is_word_char(c: char) -> bool {
    c.is_alphabetic() || c.is_numeric() || is_mark(c)
}

pub fn raw_words(text: &str) -> Vec<String> {
    let lowered = lower(text).replace('\u{2019}', "'");
    lowered
        .split(|c: char| !(is_word_char(c) || c == '\''))
        .filter_map(|token| {
            let mut token = token.trim_matches('\'').to_string();
            if token.ends_with("'s") {
                token.truncate(token.len() - 2);
            }
            let token: String = token.chars().filter(|c| *c != '\'').collect();
            (!token.is_empty()).then_some(token)
        })
        .collect()
}

pub fn words(text: &str, person_words: &HashSet<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    raw_words(text)
        .into_iter()
        .filter(|word| {
            let mut chars = word.chars();
            let single_ascii = matches!((chars.next(), chars.next()), (Some(c), None) if c.is_ascii());
            !single_ascii
                && !STOP.contains(&word.as_str())
                && !word.chars().all(char::is_numeric)
                && !person_words.contains(word)
        })
        .filter(|word| seen.insert(word.clone()))
        .collect()
}

pub fn add_person_words(target: &mut HashSet<String>, part: &str) {
    for word in raw_words(part) {
        if word.chars().count() >= 2 {
            target.insert(word);
        }
    }
}

pub fn person_display(title: Option<&str>, first: Option<&str>, legacy: Option<&str>) -> String {
    let title = title.unwrap_or("").trim();
    let first = first.unwrap_or("").trim();
    if !title.is_empty() && !first.is_empty() {
        format!("{title} {first}")
    } else if !first.is_empty() {
        first.to_string()
    } else if !title.is_empty() {
        title.to_string()
    } else {
        legacy.unwrap_or("").trim().to_string()
    }
}

pub fn is_user_named(display: &str) -> bool {
    let name = lower(display.trim());
    if name.is_empty() {
        return false;
    }
    let Some(rest) = name.strip_prefix("person").or_else(|| name.strip_prefix("unknown")) else {
        return true;
    };
    if rest.is_empty() {
        return false;
    }
    let digits = rest.trim_start_matches([' ', '_', '-']);
    digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit())
}

pub fn person_token(first: Option<&str>, legacy: Option<&str>) -> Option<String> {
    let source = [first, legacy].into_iter().flatten().map(str::trim).find(|s| !s.is_empty())?;
    let word = source.split(char::is_whitespace).find(|s| !s.is_empty())?;
    let token: String = lower(word).chars().filter(|c| is_word_char(*c)).collect();
    (token.chars().count() >= 2).then_some(token)
}

pub fn single_label(titles: &[String]) -> Option<String> {
    let mut found: Option<(String, String)> = None;
    for title in titles {
        let text = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let tokens = raw_words(&text);
        let usable = !text.is_empty()
            && text.chars().count() <= 60
            && text.len() <= 200
            && tokens.len() <= 8
            && tokens.iter().any(|t| t.chars().any(char::is_alphabetic) && !LABEL_GENERIC.contains(&t.as_str()));
        if !usable {
            continue;
        }
        let key = lower(&text);
        match &found {
            None => found = Some((key, text)),
            Some((existing, _)) if *existing == key => {}
            Some(_) => return None,
        }
    }
    found.map(|(_, text)| text)
}

pub fn split_name(name: &str) -> (String, String) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && dot + 1 < name.len() => (name[..dot].to_string(), name[dot + 1..].to_string()),
        _ => (name.to_string(), String::new()),
    }
}

fn first_sentence(text: &str) -> &str {
    text.split(['.', '!', '?', '\n']).next().unwrap_or("")
}

pub fn propose(evidence: &Evidence, options: &Options) -> Outcome {
    let none = |reason| Outcome { stem: None, reason };
    let pw = &evidence.person_words;
    let chapter = evidence.chapter.as_deref().map(|t| words(t, pw)).unwrap_or_default();
    let stored = evidence.stored_name.as_deref().map(|t| words(t, pw)).unwrap_or_default();
    let description = evidence.description.as_deref().map(|t| words(first_sentence(t), pw)).unwrap_or_default();
    let tags = words(&evidence.tags.iter().take(MAX_TAGS).cloned().collect::<Vec<_>>().join(" "), pw);
    let mut sources = vec![(chapter, "chapter")];
    if options.style == Style::Descriptive {
        sources.extend([(description, "description"), (stored, "proposedName")]);
    } else {
        sources.extend([(stored, "proposedName"), (description, "description")]);
    }
    sources.push((tags, "tags"));
    let mut event: Vec<String> = if options.event {
        evidence.event.as_deref().map(|t| words(t, pw)).unwrap_or_default()
    } else {
        Vec::new()
    };
    event.truncate(MAX_EVENT_WORDS);
    let (mut core, mut reason) = sources
        .into_iter()
        .find(|(w, _)| !w.is_empty())
        .unwrap_or((Vec::new(), "noEvidence"));
    if core.is_empty() {
        if event.is_empty() {
            return none("noEvidence");
        }
        core = std::mem::take(&mut event);
        reason = "event";
    }
    core.truncate(options.style.core_limit());
    let mut people: Vec<String> = if options.people { evidence.people.clone() } else { Vec::new() };
    people.sort();
    people.dedup();
    if people.len() > 2 {
        people.clear();
    }
    let date = if options.style == Style::DateFirst || options.date { evidence.date.clone() } else { None };

    let render = |pick: &Pick| -> String {
        let mut seen = HashSet::new();
        let body = people
            .iter()
            .zip(&pick.people)
            .filter(|(_, on)| **on)
            .map(|(p, _)| p)
            .chain(event.iter().filter(|_| pick.event))
            .chain(core.iter().take(pick.core))
            .filter(|w| seen.insert(w.as_str()))
            .cloned()
            .collect::<Vec<_>>()
            .join("-");
        match (&date, pick.date, options.style) {
            (Some(d), true, Style::DateFirst) => format!("{d} {body}"),
            (Some(d), true, _) => format!("{body}-{d}"),
            _ => body,
        }
    };
    let fits = |pick: &Pick| {
        let text = render(pick);
        text.chars().count() <= options.max_length && text.len() <= MAX_STEM_BYTES
    };
    let mut pick = Pick { people: vec![false; people.len()], core: 1, date: false, event: false };
    if !fits(&pick) {
        return none("budget");
    }
    let try_with = |pick: &mut Pick, change: &dyn Fn(&mut Pick)| {
        let mut next = pick.clone();
        change(&mut next);
        let ok = fits(&next);
        if ok {
            *pick = next;
        }
        ok
    };
    for index in 0..people.len() {
        try_with(&mut pick, &|p| p.people[index] = true);
    }
    let mut core_open = core.len() < 2 || try_with(&mut pick, &|p| p.core = 2);
    if date.is_some() {
        try_with(&mut pick, &|p| p.date = true);
    }
    if !event.is_empty() {
        try_with(&mut pick, &|p| p.event = true);
    }
    while core_open && pick.core < core.len() {
        let next = pick.core + 1;
        core_open = try_with(&mut pick, &|p| p.core = next);
    }
    let stem = render(&pick);
    if RESERVED.contains(&stem.as_str()) {
        return none("reserved");
    }
    if lower(&stem) == lower(&evidence.original_stem) {
        return none("unchanged");
    }
    Outcome { stem: Some(stem), reason }
}

#[derive(Clone)]
struct Pick {
    people: Vec<bool>,
    core: usize,
    date: bool,
    event: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        name: String,
        style: String,
        max_length: usize,
        #[serde(default)]
        include_people: bool,
        #[serde(default)]
        include_event: bool,
        #[serde(default)]
        include_date: bool,
        original: String,
        #[serde(default)]
        chapter: Option<String>,
        #[serde(default)]
        stored_name: Option<String>,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        tags: Vec<String>,
        #[serde(default)]
        person_names: Vec<String>,
        #[serde(default)]
        people: Vec<String>,
        #[serde(default)]
        event: Option<String>,
        #[serde(default)]
        date: Option<String>,
        expected: Option<String>,
        reason: String,
    }

    #[test]
    fn shared_style_cases() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../shared/test-corpus/naming/style-cases.json");
        let cases: Vec<Case> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(cases.len() >= 10);
        for case in cases {
            let mut person_words = HashSet::new();
            for name in &case.person_names {
                add_person_words(&mut person_words, name);
            }
            let evidence = Evidence {
                original_stem: case.original.clone(),
                chapter: case.chapter.clone(),
                stored_name: case.stored_name.clone(),
                description: case.description.clone(),
                tags: case.tags.clone(),
                person_words,
                people: case.people.clone(),
                event: case.event.clone(),
                date: case.date.clone(),
            };
            let options = Options {
                style: Style::parse(&case.style).unwrap(),
                max_length: case.max_length,
                people: case.include_people,
                event: case.include_event,
                date: case.include_date,
            };
            let outcome = propose(&evidence, &options);
            assert_eq!(outcome.stem, case.expected, "{}", case.name);
            assert_eq!(outcome.reason, case.reason, "{}", case.name);
        }
    }

    #[test]
    fn person_rules() {
        assert!(is_user_named("Sam"));
        assert!(is_user_named("Personality"));
        assert!(is_user_named("Person-"));
        assert!(!is_user_named("Person 12"));
        assert!(!is_user_named("unknown_3"));
        assert!(!is_user_named("Person"));
        assert!(!is_user_named("  "));
        assert_eq!(person_token(Some(" Sam "), Some("Samuel")).as_deref(), Some("sam"));
        assert_eq!(person_token(None, Some("Mira Smith")).as_deref(), Some("mira"));
        assert_eq!(person_token(Some("J"), None), None);
        assert_eq!(person_display(Some("Dr"), Some("Ana"), None), "Dr Ana");
    }

    #[test]
    fn labels_and_names() {
        assert_eq!(single_label(&["Birthday Gift".into(), "birthday  gift".into()]).as_deref(), Some("Birthday Gift"));
        assert_eq!(single_label(&["Home run".into(), "Birthday gift".into()]), None);
        assert_eq!(single_label(&["Chapter 1".into()]), None);
        assert_eq!(split_name("IMG_1.JPG"), ("IMG_1".into(), "JPG".into()));
        assert_eq!(split_name(".bashrc"), (".bashrc".into(), String::new()));
        assert_eq!(split_name("name."), ("name.".into(), String::new()));
        assert_eq!(raw_words("Sam’s Café-day ДЕНЬ"), ["sam", "café", "day", "день"]);
    }
}
