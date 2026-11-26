//! Commands the parser has no action for. A request that refers to a card the last turn proposed (“apply it”) acts on
//! that card; anything else FileID cannot do is said plainly instead of being searched as if it described a picture.

use super::chat_actions::{self, Context, Resolution};
use super::chat_facts::{totals, Filter, ANALYZABLE};
use super::chat_intent::{count_label, imperative_words, noun, parse_intent, Intent};
use anyhow::Result;
use rusqlite::{params, Connection};

const CONFIRM_VERBS: [&str; 10] = ["apply", "accept", "confirm", "approve", "proceed", "execute", "yes", "yeah", "yep", "sure"];
const CONFIRM_FILLER: [&str; 17] = [
    "it", "that", "this", "these", "those", "them", "the", "plan", "layout", "suggestion", "changes", "one", "now", "ahead", "go", "do", "again",
];

fn is_confirmation(words: &[String]) -> bool {
    let Some(verb) = words.first() else { return false };
    let rest = &words[1..];
    let confirms = CONFIRM_VERBS.contains(&verb.as_str()) || (verb == "do" || verb == "go") && !rest.is_empty();
    confirms && words.len() <= 4 && rest.iter().all(|word| CONFIRM_FILLER.contains(&word.as_str()))
}

fn numbered_group(words: &[String]) -> bool {
    words.windows(2).any(|pair| ["person", "group", "cluster"].contains(&pair[0].as_str()) && pair[1].chars().all(|c| c.is_ascii_digit()) && !pair[1].is_empty())
}

pub(crate) fn resolve(conn: &Connection, text: &str, history: &[String], context: &Context<'_>) -> Result<Option<Resolution>> {
    let (words, _) = imperative_words(text);
    if is_confirmation(&words) {
        let card = history.iter().rev().find(|earlier| !is_confirmation(&imperative_words(earlier).0)).and_then(|earlier| parse_intent(earlier)).and_then(|earlier| match earlier.kind.as_str() {
            "suggestFolderLayout" => Some(Intent { kind: "applyFolderLayout".into(), ..earlier }),
            "moveDuplicatesToTrash" | "renameToSuggested" | "applyFolderLayout" | "undoLastMove" | "mergePeople" | "namePerson" => Some(earlier),
            _ => None,
        });
        return Ok(Some(match card {
            Some(intent) => chat_actions::resolve(conn, &intent, context)?,
            None => Resolution::say("There's nothing waiting for your OK. Ask me to organize a folder or move duplicates to the Trash first, and I'll show a card to confirm."),
        }));
    }
    let verb = words.first().map(String::as_str);
    if numbered_group(&words) && matches!(verb, Some("name" | "call" | "label" | "who" | "whos" | "merge" | "rename" | "show")) {
        return Ok(Some(Resolution::say("People groups aren't numbered in Chat. Open People to see each group and name it there.")));
    }
    let about_people = words.iter().skip(1).all(|word| ["the", "these", "those", "two", "them", "people", "persons", "faces", "groups", "clusters", "together", "please"].contains(&word.as_str()));
    if matches!(verb, Some("merge" | "combine" | "join")) && about_people {
        return Ok(Some(Resolution::say("Tell me which two people to merge, like “Merge Anna and Anne”, or merge them in People.")));
    }
    if asks_about_video_length(&words) {
        return Ok(Some(Resolution::say("FileID doesn't know how long videos run, so it can't filter or list them by length. Ask for the largest, smallest, newest or oldest videos instead.")));
    }
    if let Some(answer) = rename_question(conn, text, &words, context)? {
        return Ok(Some(answer));
    }
    described_count(conn, &words, context)
}

fn asks_about_video_length(words: &[String]) -> bool {
    let named = |any: &[&str]| words.iter().any(|word| any.contains(&word.as_str()));
    let about_videos = named(&["video", "videos", "clip", "clips", "movie", "movies"]);
    let by_length = named(&["longest", "shortest", "duration", "length"])
        || (named(&["longer", "shorter", "long"]) && named(&["second", "seconds", "sec", "secs", "minute", "minutes", "min", "mins", "hour", "hours"]));
    about_videos && by_length
}

fn asks_how_many(words: &[String]) -> bool {
    words.windows(2).any(|pair| pair == ["how", "many"])
}

/// "How many photos have been described?": counts files that have a description against those that can have one.
fn described_count(conn: &Connection, words: &[String], context: &Context<'_>) -> Result<Option<Resolution>> {
    let describes = words.iter().any(|word| ["described", "analyzed", "captioned"].contains(&word.as_str()));
    let negated = words.iter().any(|word| ["not", "no", "without", "missing", "havent", "hasnt", "need", "needs", "lacking", "undescribed", "unanalyzed"].contains(&word.as_str()));
    if !asks_how_many(words) || !describes || negated {
        return Ok(None);
    }
    let mut kinds = Vec::new();
    for word in words {
        match word.as_str() {
            "photos" | "pictures" | "images" => kinds.push("image".to_owned()),
            "videos" | "clips" => kinds.push("video".to_owned()),
            _ => {}
        }
    }
    let filter = Filter { kinds, root: context.root.cloned(), ..Filter::default() };
    let eligible_kinds = if filter.kinds.is_empty() { ANALYZABLE.iter().map(|kind| (*kind).to_owned()).collect() } else { filter.kinds.clone() };
    let eligible = Filter { kinds: eligible_kinds, ..filter.clone() };
    let (total, _) = totals(conn, &eligible)?;
    let (pending, _) = totals(conn, &Filter { undescribed: true, ..eligible.clone() })?;
    let label = if filter.kinds.is_empty() { "files".to_owned() } else { noun(&filter.kinds, total) };
    let message = if total == 0 {
        format!("There are no {label} to describe.")
    } else if pending == 0 {
        format!("All {} {label} have a description.", count_label(total))
    } else {
        format!(
            "{} of {} {label} have a description. {} still need one; ask me to describe them.",
            count_label(total - pending),
            count_label(total),
            count_label(pending)
        )
    };
    Ok(Some(Resolution::say(message)))
}

/// "What would you rename DSC01698 to?": the name Describe Files proposed for that file, or how to get one.
fn rename_question(conn: &Connection, text: &str, words: &[String], context: &Context<'_>) -> Result<Option<Resolution>> {
    let asks = words.first().is_some_and(|word| ["what", "which", "how"].contains(&word.as_str())) && words.iter().any(|word| word == "rename");
    if !asks {
        return Ok(None);
    }
    let stems: Vec<&str> = text.split_whitespace().map(|word| word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')).filter(|word| word.chars().any(|c| c.is_ascii_digit())).collect();
    for stem in stems {
        let mut statement = conn.prepare(&format!(
            "SELECT f.id,f.path_text FROM files f WHERE lower(f.path_text) LIKE ?1 ESCAPE '\\' AND {} \
             AND f.vlm_proposed_name IS NOT NULL AND trim(f.vlm_proposed_name)<>'' LIMIT 20",
            super::roots::clause("f.path_text", "?2")
        ))?;
        let pattern = format!("%{}.%", stem.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let found: Vec<(i64, String)> = statement
            .query_map(params![pattern, context.root.map(super::roots::RootScope::param)], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let ids: Vec<i64> = found
            .iter()
            .filter(|(_, path)| path.rsplit(['/', '\\']).next().and_then(|name| name.rsplit_once('.')).is_some_and(|(file_stem, _)| file_stem.eq_ignore_ascii_case(stem)))
            .map(|(id, _)| *id)
            .collect();
        if let Some((_, path, proposed)) = super::catalog_names::default_names(conn, &ids)?.into_iter().next() {
            let current = path.rsplit(['/', '\\']).next().unwrap_or(&path);
            return Ok(Some(Resolution::say(format!("Describe Files suggested “{proposed}” for {current}. Say “Rename {stem} to the suggested name” to get a card to confirm."))));
        }
    }
    Ok(Some(Resolution::say("I have no suggested name for that file yet. Run Describe Files to get one, or tell me the name: “Rename DSC01698 to Birthday cake”.")))
}
