//! Commands the parser has no action for. A request that refers to a card the last turn proposed (“apply it”) acts on
//! that card; anything else FileID cannot do is said plainly instead of being searched as if it described a picture.

use super::chat_actions::{self, Context, Resolution};
use super::chat_intent::{imperative_words, parse_intent, Intent};
use anyhow::Result;
use rusqlite::Connection;

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
    let video_length = words.iter().any(|word| ["longest", "shortest", "duration", "length"].contains(&word.as_str()))
        && words.iter().any(|word| ["video", "videos", "clip", "clips", "movie", "movies"].contains(&word.as_str()));
    if video_length {
        return Ok(Some(Resolution::say("FileID doesn't list videos by length yet. Ask for the largest, smallest, newest or oldest videos instead.")));
    }
    Ok(None)
}
