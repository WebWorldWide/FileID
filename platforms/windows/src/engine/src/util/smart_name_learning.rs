//! Local learning of smart-name style corrections. Each sample pairs the name
//! the engine would propose without learning (`base`) with the name the user
//! applied; preferences are what a clear majority of the newest samples show.
//! Lockstep with macOS `SmartNameLearning.swift`; both run
//! `shared/test-corpus/naming/learning-cases.json`.

use std::collections::{BTreeMap, HashSet};

use super::smart_name_style::{lower, raw_words, split_name, upper};

pub const WINDOW: usize = 50;
pub const KEEP: usize = 200;
pub const KEYS: [&str; 8] =
    ["dropWords", "people", "words", "leadingDate", "trailingDate", "dateFormat", "separator", "case"];
const MIN_VOTES: usize = 3;
const MIN_DROPS: usize = 2;
const MAX_WORD_CAP: usize = 5;
const SEPARATORS: [char; 3] = [' ', '-', '_'];
const TRIM: [char; 4] = [' ', '-', '_', '.'];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Learned {
    pub separator: Option<String>,
    pub letter_case: Option<String>,
    pub leading_date: Option<String>,
    pub trailing_date: Option<String>,
    pub date_format: Option<String>,
    pub omit_people: bool,
    pub max_words: Option<usize>,
    pub drop_words: Vec<String>,
}

impl Learned {
    pub fn active_keys(&self) -> Vec<&'static str> {
        KEYS.into_iter().filter(|key| !self.values(key).is_empty()).collect()
    }

    pub fn without(&self, key: &str) -> Learned {
        let mut copy = self.clone();
        match key {
            "dropWords" => copy.drop_words.clear(),
            "people" => copy.omit_people = false,
            "words" => copy.max_words = None,
            "leadingDate" => copy.leading_date = None,
            "trailingDate" => copy.trailing_date = None,
            "dateFormat" => copy.date_format = None,
            "separator" => copy.separator = None,
            "case" => copy.letter_case = None,
            _ => {}
        }
        copy
    }

    pub fn values(&self, key: &str) -> Vec<String> {
        match key {
            "dropWords" => self.drop_words.clone(),
            "people" => if self.omit_people { vec!["omit".into()] } else { Vec::new() },
            "words" => self.max_words.map(|cap| cap.to_string()).into_iter().collect(),
            "leadingDate" => self.leading_date.clone().into_iter().collect(),
            "trailingDate" => self.trailing_date.clone().into_iter().collect(),
            "dateFormat" => self.date_format.clone().into_iter().collect(),
            "separator" => self.separator.clone().into_iter().collect(),
            "case" => self.letter_case.clone().into_iter().collect(),
            _ => Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Sample {
    pub base: String,
    pub applied: String,
    pub people: Vec<String>,
}

#[derive(Default, Debug)]
struct Observation {
    leading: Option<&'static str>,
    trailing: Option<&'static str>,
    date_format: Option<&'static str>,
    separator: Option<char>,
    letter_case: Option<&'static str>,
    people_kept: Option<bool>,
    words: Vec<(String, bool)>,
    length: Option<(usize, usize)>,
}

fn strip_bump(stem: &[char]) -> &[char] {
    let digits = stem.iter().rev().take_while(|c| c.is_ascii_digit()).count();
    if (1..=2).contains(&digits) && stem.len() > digits + 1 && stem[stem.len() - digits - 1] == '_' {
        let value: u32 = stem[stem.len() - digits..].iter().collect::<String>().parse().unwrap_or(0);
        if (2..=99).contains(&value) {
            return &stem[..stem.len() - digits - 1];
        }
    }
    stem
}

fn digit_at(text: &[char], index: usize) -> bool {
    text.get(index).is_some_and(char::is_ascii_digit)
}

fn find(text: &[char], pattern: &[char], month: bool) -> Option<usize> {
    if pattern.len() > text.len() {
        return None;
    }
    (0..=text.len() - pattern.len()).find(|&start| {
        let end = start + pattern.len();
        text[start..end] == *pattern
            && (start == 0 || !digit_at(text, start - 1))
            && !digit_at(text, end)
            && !(month && text.get(end).is_some_and(|c| matches!(c, '-' | '_' | '.')) && digit_at(text, end + 1))
    })
}

fn iso_date(text: &[char]) -> Option<usize> {
    if text.len() < 10 {
        return None;
    }
    (0..=text.len() - 10).find(|&start| {
        (0..10).all(|offset| if offset == 4 || offset == 7 { text[start + offset] == '-' } else { digit_at(text, start + offset) })
            && (start == 0 || !digit_at(text, start - 1))
            && !digit_at(text, start + 10)
    })
}

fn body(text: &[char], cut: Option<(usize, usize)>) -> String {
    let mut chars: Vec<char> = text.to_vec();
    if let Some((start, end)) = cut {
        chars.drain(start..end);
    }
    let s: String = chars.into_iter().collect();
    s.trim_matches(&TRIM[..]).to_string()
}

fn letter_case(text: &str) -> Option<&'static str> {
    let mut found: Option<&'static str> = None;
    for token in text.split(|c: char| !(super::smart_name_style::is_word_char(c) || c == '\'')) {
        let token: String = token.chars().filter(|c| *c != '\'').collect();
        if lower(&token) == token && upper(&token) == token {
            continue;
        }
        let mut chars = token.chars();
        let first = chars.next().map(|c| c.to_string()).unwrap_or_default();
        let rest = chars.as_str();
        let class = if lower(&token) == token {
            "lower"
        } else if upper(&first) == first && lower(&first) != first && lower(rest) == rest {
            "title"
        } else if upper(&token) == token {
            "upper"
        } else {
            return None;
        };
        match found {
            Some(seen) if seen != class => return None,
            _ => found = Some(class),
        }
    }
    found
}

fn numeric(word: &str) -> bool {
    word.chars().all(char::is_numeric)
}

fn observe(sample: &Sample) -> Observation {
    let mut seen = Observation::default();
    let base: Vec<char> = sample.base.chars().collect();
    let (applied_stem, _) = split_name(&sample.applied);
    let applied_chars: Vec<char> = applied_stem.chars().collect();
    let applied = strip_bump(&applied_chars);

    let base_date = iso_date(&base).filter(|&start| start == 0 || start + 10 == base.len());
    let mut applied_cut = None;
    if let Some(start) = base_date {
        let date: String = base[start..start + 10].iter().collect();
        let (year, month, day) = (&date[0..4], &date[5..7], &date[8..10]);
        let formats = [
            ("iso", format!("{year}-{month}-{day}"), false),
            ("underscore", format!("{year}_{month}_{day}"), false),
            ("dotted", format!("{year}.{month}.{day}"), false),
            ("compact", format!("{year}{month}{day}"), false),
            ("month", format!("{year}-{month}"), true),
        ];
        let found = formats.iter().find_map(|(name, pattern, month)| {
            let pattern: Vec<char> = pattern.chars().collect();
            find(applied, &pattern, *month).map(|at| (*name, at, at + pattern.len()))
        });
        let position = match found {
            Some((_, 0, _)) => Some("first"),
            Some((_, _, end)) if end == applied.len() => Some("last"),
            Some(_) => Some("middle"),
            None if applied.windows(4).any(|w| w.iter().all(char::is_ascii_digit)) => None,
            None => Some("none"),
        };
        if let Some((name, start, end)) = found {
            seen.date_format = Some(name);
            applied_cut = Some((start, end));
        }
        if start == 0 {
            seen.leading = position.map(|p| match p {
                "first" => "keep",
                "last" => "end",
                other => other,
            });
        } else {
            seen.trailing = position.map(|p| match p {
                "last" => "keep",
                "first" => "start",
                other => other,
            });
        }
    }
    let base_body = body(&base, base_date.map(|start| (start, start + 10)));
    let applied_body = body(applied, applied_cut);
    let base_words = raw_words(&base_body);
    let applied_words = raw_words(&applied_body);
    let applied_set: HashSet<&String> = applied_words.iter().collect();

    if applied_words.len() >= 2 {
        let present: Vec<char> = SEPARATORS.into_iter().filter(|c| applied_body.contains(*c)).collect();
        if present.len() == 1 {
            seen.separator = Some(present[0]);
        }
    }
    seen.letter_case = letter_case(&applied_body);

    let people: HashSet<&String> = sample.people.iter().collect();
    let named: Vec<&String> = base_words.iter().filter(|w| people.contains(w)).collect();
    if !named.is_empty() {
        let kept = named.iter().filter(|w| applied_set.contains(**w)).count();
        if kept == named.len() {
            seen.people_kept = Some(true);
        } else if kept == 0 {
            seen.people_kept = Some(false);
        }
    }

    let base_plain: Vec<&String> = base_words.iter().filter(|w| !numeric(w) && !people.contains(w)).collect();
    let applied_plain: Vec<&String> = applied_words.iter().filter(|w| !numeric(w) && !people.contains(w)).collect();
    let mut distinct = HashSet::new();
    let candidates: Vec<&String> = base_plain.iter().copied().filter(|w| distinct.insert(*w)).collect();
    if candidates.iter().any(|w| applied_set.contains(*w)) {
        seen.words = candidates.iter().map(|w| ((*w).clone(), !applied_set.contains(*w))).collect();
    }
    if base_plain.len() >= 2
        && !applied_plain.is_empty()
        && applied_plain.len() <= base_plain.len()
        && applied_plain[..] == base_plain[..applied_plain.len()]
    {
        seen.length = Some((applied_plain.len(), base_plain.len()));
    }
    seen
}

fn majority<T: PartialEq + Copy>(votes: &[T]) -> Option<T> {
    if votes.len() < MIN_VOTES {
        return None;
    }
    votes.iter().copied().find(|vote| 2 * votes.iter().filter(|other| *other == vote).count() > votes.len())
}

/// Preferences from up to `WINDOW` samples, newest first; the order does not change the result.
pub fn derive(samples: &[Sample]) -> Learned {
    let observations: Vec<Observation> = samples.iter().take(WINDOW).map(observe).collect();
    let mut learned = Learned::default();
    let separators: Vec<char> = observations.iter().filter_map(|o| o.separator).collect();
    learned.separator = majority(&separators).filter(|c| *c != '-').map(|c| c.to_string());
    let cases: Vec<&str> = observations.iter().filter_map(|o| o.letter_case).collect();
    learned.letter_case = majority(&cases).filter(|c| *c != "lower").map(str::to_string);
    let leading: Vec<&str> = observations.iter().filter_map(|o| o.leading).collect();
    learned.leading_date = majority(&leading).filter(|v| matches!(*v, "end" | "none")).map(str::to_string);
    let trailing: Vec<&str> = observations.iter().filter_map(|o| o.trailing).collect();
    learned.trailing_date = majority(&trailing).filter(|v| matches!(*v, "start" | "none")).map(str::to_string);
    let formats: Vec<&str> = observations.iter().filter_map(|o| o.date_format).collect();
    learned.date_format = majority(&formats).filter(|f| *f != "iso").map(str::to_string);
    let people: Vec<bool> = observations.iter().filter_map(|o| o.people_kept).collect();
    learned.omit_people = majority(&people) == Some(false);
    let mut words: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (word, dropped) in observations.iter().flat_map(|o| &o.words) {
        let entry = words.entry(word.as_str()).or_default();
        if *dropped {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
    learned.drop_words = words
        .into_iter()
        .filter(|(_, (dropped, kept))| *dropped >= MIN_DROPS && dropped > kept)
        .map(|(word, _)| word.to_string())
        .collect();
    let lengths: Vec<(usize, usize)> = observations.iter().filter_map(|o| o.length).collect();
    learned.max_words = (1..=MAX_WORD_CAP).find(|&cap| {
        let eligible: Vec<&(usize, usize)> = lengths.iter().filter(|(_, available)| *available > cap).collect();
        eligible.len() >= MIN_VOTES && 2 * eligible.iter().filter(|(count, _)| *count <= cap).count() > eligible.len()
    });
    learned
}

pub fn tagged(reason: &str, learned: &[&str]) -> String {
    if learned.is_empty() { reason.to_string() } else { format!("{reason}|learned:{}", learned.join(",")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::smart_name_style::{add_person_words, propose, Evidence, Options, Style};

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SampleCase {
        base: String,
        applied: String,
        #[serde(default)]
        people: Vec<String>,
    }

    #[derive(serde::Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct Expected {
        #[serde(default)]
        separator: Option<String>,
        #[serde(default)]
        case: Option<String>,
        #[serde(default)]
        leading_date: Option<String>,
        #[serde(default)]
        trailing_date: Option<String>,
        #[serde(default)]
        date_format: Option<String>,
        #[serde(default)]
        omit_people: bool,
        #[serde(default)]
        words: Option<usize>,
        #[serde(default)]
        drop_words: Vec<String>,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Proposal {
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
        stored_name: Option<String>,
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
        #[serde(default)]
        learned: Vec<String>,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Case {
        name: String,
        samples: Vec<SampleCase>,
        learned: Expected,
        #[serde(default)]
        proposals: Vec<Proposal>,
    }

    fn learned_of(expected: &Expected) -> Learned {
        Learned {
            separator: expected.separator.clone(),
            letter_case: expected.case.clone(),
            leading_date: expected.leading_date.clone(),
            trailing_date: expected.trailing_date.clone(),
            date_format: expected.date_format.clone(),
            omit_people: expected.omit_people,
            max_words: expected.words,
            drop_words: expected.drop_words.clone(),
        }
    }

    #[test]
    fn shared_learning_cases() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../../shared/test-corpus/naming/learning-cases.json");
        let cases: Vec<Case> = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert!(cases.len() >= 8);
        for case in cases {
            let samples: Vec<Sample> = case
                .samples
                .iter()
                .map(|s| Sample { base: s.base.clone(), applied: s.applied.clone(), people: s.people.clone() })
                .collect();
            let learned = derive(&samples);
            assert_eq!(learned, learned_of(&case.learned), "{}", case.name);
            let mut reversed = samples.clone();
            reversed.reverse();
            assert_eq!(derive(&reversed), learned, "{} (order)", case.name);
            for proposal in &case.proposals {
                let mut person_words = HashSet::new();
                for name in &proposal.person_names {
                    add_person_words(&mut person_words, name);
                }
                let evidence = Evidence {
                    original_stem: proposal.original.clone(),
                    stored_name: proposal.stored_name.clone(),
                    tags: proposal.tags.clone(),
                    person_words,
                    people: proposal.people.clone(),
                    event: proposal.event.clone(),
                    date: proposal.date.clone(),
                    ..Evidence::default()
                };
                let options = Options {
                    style: Style::parse(&proposal.style).unwrap(),
                    max_length: proposal.max_length,
                    people: proposal.include_people,
                    event: proposal.include_event,
                    date: proposal.include_date,
                    learned: learned.clone(),
                };
                let outcome = propose(&evidence, &options);
                assert_eq!(outcome.stem, proposal.expected, "{} / {}", case.name, proposal.original);
                assert_eq!(outcome.reason, proposal.reason, "{} / {}", case.name, proposal.original);
                assert_eq!(outcome.learned, proposal.learned, "{} / {}", case.name, proposal.original);
            }
        }
    }

    #[test]
    fn reasons_are_tagged() {
        assert_eq!(tagged("tags", &[]), "tags");
        assert_eq!(tagged("tags", &["case", "separator"]), "tags|learned:case,separator");
    }
}
