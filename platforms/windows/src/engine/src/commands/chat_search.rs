#[derive(Clone, Debug, PartialEq)]
pub(super) struct SearchPlan {
    pub query: String,
    pub kinds: Vec<String>,
    pub person_ids: Vec<i64>,
    pub person_names: Vec<String>,
    /// "Kyle and Maya": files with every named person, not any of them.
    pub all_people: bool,
    /// "without Maya": people whose files are left out.
    pub excluded_person_ids: Vec<i64>,
    pub excluded_person_names: Vec<String>,
    pub event_ids: Vec<String>,
    pub event_names: Vec<String>,
    pub time_seconds: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn confirmed_names_events_time_refinement_and_negation() {
        let people = vec![
            (1, vec!["Alice Smith".into(), "Alice".into()]),
            (2, vec!["Alice Smith".into()]),
        ];
        let events = vec![("birthday".into(), vec!["Family Birthday".into()])];
        let plan = SearchPlan::resolve_known(
            "find Alice Smith's videos from Family Birthday at 01:02:03",
            None,
            &people,
            &events,
            false,
        );
        assert!(plan.query.is_empty());
        assert_eq!(plan.person_ids, vec![1, 2]);
        assert_eq!(plan.event_ids, vec!["birthday"]);
        assert_eq!(plan.time_seconds, Some(3723.0));
        let refined = SearchPlan::resolve_known("only photos", Some(&plan), &people, &events, false);
        assert_eq!(refined.person_ids, plan.person_ids);
        assert_eq!(refined.event_ids, plan.event_ids);
        assert_eq!(refined.time_seconds, plan.time_seconds);
        let negative = SearchPlan::resolve_known("not birthday videos at 12:03", None, &people, &events, false);
        assert!(negative.person_ids.is_empty() && negative.kinds.is_empty() && negative.time_seconds.is_none());
        assert!(negative.query.contains("birthday"));
        let excluded = SearchPlan::resolve_known("not Alice videos at 12:03", None, &people, &events, false);
        assert_eq!((excluded.excluded_person_ids.clone(), excluded.kinds.clone(), excluded.time_seconds), (vec![1], vec!["video".to_owned()], Some(723.0)));
        assert!(excluded.query.is_empty());
        for bad in ["1:60", "1:60:00", "1:2:60", "-1:00", "1000001:00"] {
            assert_eq!(timestamp(bad), None);
        }
        assert_eq!(timestamp("12:03"), Some(723.0));
    }
    #[test]
    fn both_people_excluded_people_and_follow_ups() {
        let people = vec![(1, vec!["Kyle".into()]), (2, vec!["Maya".into()]), (3, vec!["Ann Lee".into()])];
        let plan = SearchPlan::resolve_known("photos of Kyle and Maya at the beach", None, &people, &[], false);
        assert_eq!((plan.query.as_str(), plan.person_ids.clone(), plan.all_people), ("beach", vec![1, 2], true));
        let either = SearchPlan::resolve_known("photos of Kyle or Maya", None, &people, &[], false);
        assert!(!either.all_people && either.person_ids == [1, 2]);
        let without = SearchPlan::resolve_known("beach photos without Maya or Ann Lee", None, &people, &[], false);
        assert_eq!((without.query.as_str(), without.excluded_person_ids.clone()), ("beach", vec![2, 3]));
        assert_eq!(without.excluded_person_names, ["Ann Lee", "Maya"]);
        assert!(without.person_ids.is_empty() && without.kinds == ["image"]);
        let unknown = SearchPlan::resolve_known("videos not about cats", None, &people, &[], false);
        assert!(unknown.excluded_person_ids.is_empty() && unknown.kinds.is_empty());
        let narrowed = SearchPlan::resolve_known("", Some(&without), &people, &[], true);
        assert_eq!((narrowed.query.as_str(), narrowed.excluded_person_ids.clone(), narrowed.kinds.clone()), ("beach", vec![2, 3], vec!["image".to_owned()]));
        assert_eq!(SearchPlan::resolve_known("", Some(&without), &people, &[], false).query, "");
        assert_eq!(SearchPlan::resolve_known("what is there about taxes", None, &people, &[], false).query, "taxes");
    }

    #[test]
    fn shared_plans_preserve_subjects_and_explicit_filters() {
        #[derive(serde::Deserialize)]
        struct Fixture {
            messages: Vec<String>,
            query: String,
            kinds: Vec<String>,
        }
        let fixtures: Vec<Fixture> =
            serde_json::from_str(include_str!("../../../../../../shared/test-corpus/chat-search.json")).unwrap();
        for fixture in fixtures {
            let plan = fixture
                .messages
                .iter()
                .fold(None, |previous, text| {
                    Some(SearchPlan::resolve(text, previous.as_ref()))
                })
                .unwrap();
            assert_eq!(
                plan,
                SearchPlan {
                    query: fixture.query,
                    kinds: fixture.kinds,
                    person_ids: vec![],
                    person_names: vec![],
                    all_people: false,
                    excluded_person_ids: vec![],
                    excluded_person_names: vec![],
                    event_ids: vec![],
                    event_names: vec![],
                    time_seconds: None
                }
            );
        }
    }
}

impl SearchPlan {
    #[cfg(test)]
    pub fn resolve(text: &str, previous: Option<&Self>) -> Self {
        Self::resolve_known(text, previous, &[], &[], false)
    }
    /// `follow_up` narrows `previous` even without a refinement word
    /// ("how many are from 2018?").
    pub fn resolve_known(
        text: &str,
        previous: Option<&Self>,
        people: &[(i64, Vec<String>)],
        events: &[(String, Vec<String>)],
        follow_up: bool,
    ) -> Self {
        let words: Vec<_> = text.split_whitespace().collect();
        let mut normalized: Vec<_> = words.iter().map(|word| normalize(word)).collect();
        let negations = ["not", "except", "without", "excluding"];
        let (excluded_ids, excluded_names, excluded_indices) = match_exclusions(&normalized, &negations, people);
        for index in &excluded_indices {
            normalized[*index].clear();
        }
        let negative = normalized.iter().any(|word| negations.contains(&word.as_str()));
        let (mut person_ids, mut person_names, people_indices) = if negative {
            Default::default()
        } else {
            match_names(&normalized, people)
        };
        let (mut event_ids, mut event_names, event_indices) = if negative {
            Default::default()
        } else {
            match_names(&normalized, events)
        };
        let mut time_seconds = None;
        let mut time_indices = std::collections::BTreeSet::new();
        if !negative {
            for (index, word) in normalized.iter().enumerate() {
                if ["at", "around", "near"].contains(&word.as_str())
                    && let Some(seconds) = normalized.get(index + 1).and_then(|value| timestamp(value)) {
                        time_seconds = Some(seconds);
                        time_indices.extend([index, index + 1]);
                        break;
                    }
            }
        }
        let aliases = |word: &str| -> &[&str] {
            match word {
                "video" | "videos" | "clip" | "clips" => &["video"],
                "photo" | "photos" | "picture" | "pictures" | "image" | "images" => &["image"],
                "document" | "documents" => &["doc", "pdf"],
                "pdf" | "pdfs" => &["pdf"],
                "audio" => &["audio"],
                _ => &[],
            }
        };
        let fillers = [
            "find", "show", "me", "please", "search", "for", "from", "at", "around", "near", "the", "a", "an", "where",
            "of", "my", "files", "file", "with", "in", "all", "only", "just", "now", "these", "those", "instead",
            "also", "and", "what", "whats", "which", "who", "is", "are", "was", "were", "do", "does", "did", "have", "has",
            "there", "any", "about", "that", "i", "mentioning", "mention", "mentions", "named", "called", "titled",
            "regarding", "ones",
        ];
        let refinement = follow_up
            || normalized
                .iter()
                .any(|word| ["only", "just", "now", "these", "those", "instead", "also", "and"].contains(&word.as_str()));
        let mut kinds: Vec<String> = if negative {
            vec![]
        } else {
            normalized
                .iter()
                .flat_map(|word| aliases(word).iter().map(|kind| (*kind).to_owned()))
                .collect()
        };
        kinds.sort();
        kinds.dedup();
        let mut query = words
            .iter()
            .zip(&normalized)
            .enumerate()
            .filter(|(index, (_, word))| {
                !people_indices.contains(index)
                    && !event_indices.contains(index)
                    && !time_indices.contains(index)
                    && !excluded_indices.contains(index)
                    && !fillers.contains(&word.as_str())
                    && (negative || aliases(word).is_empty())
            })
            .map(|(_, (word, _))| *word)
            .collect::<Vec<_>>()
            .join(" ");
        if refinement && !normalized.iter().any(|word| word == "all")
            && let Some(previous) = previous {
                if query.is_empty() {
                    query.clone_from(&previous.query);
                } else if (normalized.iter().any(|word| word == "also")
                    || normalized.first().is_some_and(|word| word == "and"))
                    && !previous.query.is_empty()
                {
                    query = format!("{} {}", previous.query, query);
                }
            }
        if kinds.is_empty() && !negative && refinement && !normalized.iter().any(|word| word == "all")
            && let Some(previous) = previous {
                kinds.clone_from(&previous.kinds);
            }
        let mut all_people = person_ids.len() > 1
            && !normalized.iter().any(|word| word == "or")
            && normalized.iter().any(|word| ["and", "with", "both", "together", "plus"].contains(&word.as_str()));
        let (mut excluded_ids, mut excluded_names) = (excluded_ids, excluded_names);
        if refinement
            && let Some(previous) = previous {
                if person_ids.is_empty() {
                    person_ids.clone_from(&previous.person_ids);
                    person_names.clone_from(&previous.person_names);
                    all_people = previous.all_people;
                } else if normalized.iter().any(|word| word == "also") {
                    person_ids.extend(&previous.person_ids);
                    person_names.extend(previous.person_names.clone());
                    all_people = false;
                }
                if excluded_ids.is_empty() {
                    excluded_ids.clone_from(&previous.excluded_person_ids);
                    excluded_names.clone_from(&previous.excluded_person_names);
                }
                if event_ids.is_empty() {
                    event_ids.clone_from(&previous.event_ids);
                    event_names.clone_from(&previous.event_names);
                } else if normalized.iter().any(|word| word == "also") {
                    event_ids.extend(previous.event_ids.clone());
                    event_names.extend(previous.event_names.clone());
                }
                if time_seconds.is_none() {
                    time_seconds = previous.time_seconds;
                }
            }
        person_ids.sort();
        person_ids.dedup();
        person_names.sort();
        person_names.dedup();
        event_ids.sort();
        event_ids.dedup();
        event_names.sort();
        event_names.dedup();
        excluded_ids.retain(|id| !person_ids.contains(id));
        excluded_ids.sort();
        excluded_ids.dedup();
        excluded_names.retain(|name| !person_names.contains(name));
        excluded_names.sort();
        excluded_names.dedup();
        Self {
            query: query.chars().take(2000).collect(),
            kinds,
            person_ids,
            person_names,
            all_people,
            excluded_person_ids: excluded_ids,
            excluded_person_names: excluded_names,
            event_ids,
            event_names,
            time_seconds,
        }
    }
}

fn normalize(value: &str) -> String {
    value
        .to_lowercase()
        .replace("’s", "")
        .replace("'s", "")
        .replace(['’', '\''], "")
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_owned()
}
fn timestamp(value: &str) -> Option<f64> {
    let parts = value.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len())
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    let values = parts
        .iter()
        .map(|part| part.parse::<u64>().ok())
        .collect::<Option<Vec<_>>>()?;
    if values[0] > 1_000_000 || values[1] >= 60 && values.len() == 3 || *values.last()? >= 60 {
        return None;
    }
    Some(if values.len() == 2 {
        values[0] * 60 + values[1]
    } else {
        values[0] * 3600 + values[1] * 60 + values[2]
    } as f64)
}
/// People named right after "without"/"except"/"not" (optionally "of"/"the",
/// joined by "or"/"and"/","), with the words they used, negation included.
/// Mirrors macOS `ChatSearchPlan.matchExclusions`.
fn match_exclusions(
    words: &[String],
    negations: &[&str],
    people: &[(i64, Vec<String>)],
) -> (Vec<i64>, Vec<String>, std::collections::BTreeSet<usize>) {
    let (mut ids, mut names, mut used) = (Vec::new(), Vec::new(), std::collections::BTreeSet::new());
    for index in 0..words.len() {
        if !negations.contains(&words[index].as_str()) {
            continue;
        }
        let mut at = index + 1;
        let mut consumed = std::collections::BTreeSet::from([index]);
        let mut found = false;
        while at < words.len() {
            while at < words.len() && ["of", "the", "any", "or", "and", "nor", ""].contains(&words[at].as_str()) {
                consumed.insert(at);
                at += 1;
            }
            if at >= words.len() {
                break;
            }
            let window = &words[at..(at + 4).min(words.len())];
            let (_, _, indices) = match_names(window, people);
            if !indices.contains(&0) {
                break;
            }
            let length = (0..window.len()).take_while(|offset| indices.contains(offset)).count();
            let (single_ids, single_names, _) = match_names(&window[..length], people);
            ids.extend(single_ids);
            names.extend(single_names);
            consumed.extend(at..at + length);
            at += length;
            found = true;
        }
        if found {
            used.extend(consumed.into_iter().filter(|offset| *offset < at));
        }
    }
    (ids, names, used)
}

fn match_names<T: Clone + Ord>(
    words: &[String],
    known: &[(T, Vec<String>)],
) -> (Vec<T>, Vec<String>, std::collections::BTreeSet<usize>) {
    let candidates = known
        .iter()
        .flat_map(|(id, names)| {
            names.iter().map(move |name| {
                (
                    id,
                    name,
                    name.split_whitespace()
                        .map(normalize)
                        .filter(|word| !word.is_empty())
                        .collect::<Vec<_>>(),
                )
            })
        })
        .collect::<Vec<_>>();
    let mut ids = Vec::new();
    let mut names = Vec::new();
    let mut consumed = std::collections::BTreeSet::new();
    let mut index = 0;
    while index < words.len() {
        let matches = candidates
            .iter()
            .filter(|(_, _, tokens)| {
                !tokens.is_empty()
                    && index + tokens.len() <= words.len()
                    && words[index..index + tokens.len()] == **tokens
            })
            .collect::<Vec<_>>();
        if let Some(length) = matches.iter().map(|(_, _, tokens)| tokens.len()).max() {
            for (id, name, tokens) in matches {
                if tokens.len() == length {
                    ids.push((*id).clone());
                    names.push(name.trim().to_owned());
                }
            }
            consumed.extend(index..index + length);
            index += length;
        } else {
            index += 1;
        }
    }
    (ids, names, consumed)
}
