#[derive(Clone, Debug, PartialEq)]
pub(super) struct SearchPlan {
    pub query: String,
    pub kinds: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_plans_preserve_subjects_and_explicit_filters() {
        #[derive(serde::Deserialize)]
        struct Fixture { messages: Vec<String>, query: String, kinds: Vec<String> }
        let fixtures: Vec<Fixture> = serde_json::from_str(include_str!("../../../../../../shared/test-corpus/chat-search.json")).unwrap();
        for fixture in fixtures {
            let plan = fixture.messages.iter().fold(None, |previous,text| Some(SearchPlan::resolve(text,previous.as_ref()))).unwrap();
            assert_eq!(plan,SearchPlan { query:fixture.query,kinds:fixture.kinds });
        }
    }
}

impl SearchPlan {
    pub fn resolve(text: &str, previous: Option<&Self>) -> Self {
        let words: Vec<_> = text.split_whitespace().collect();
        let normalized: Vec<_> = words.iter().map(|word| word.to_lowercase().trim_matches(|c: char| !c.is_alphanumeric()).to_owned()).collect();
        let negative = normalized.iter().any(|word| ["not", "except", "without", "excluding"].contains(&word.as_str()));
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
        let fillers = ["find", "show", "me", "please", "search", "for", "the", "a", "an", "where", "of", "my", "files", "file", "with", "in", "all", "only", "just", "now", "these", "those", "instead", "also", "and"];
        let refinement = normalized.iter().any(|word| ["only", "just", "now", "these", "those", "instead", "also", "and"].contains(&word.as_str()));
        let mut kinds: Vec<String> = if negative { vec![] } else { normalized.iter().flat_map(|word| aliases(word).iter().map(|kind| (*kind).to_owned())).collect() };
        kinds.sort(); kinds.dedup();
        let mut query = words.iter().zip(&normalized).filter(|(_,word)| !fillers.contains(&word.as_str()) && (negative || aliases(word).is_empty())).map(|(word,_)| *word).collect::<Vec<_>>().join(" ");
        if refinement && !normalized.iter().any(|word| word == "all") {
            if let Some(previous) = previous {
                if query.is_empty() { query.clone_from(&previous.query); }
                else if (normalized.iter().any(|word| word == "also") || normalized.first().is_some_and(|word| word == "and")) && !previous.query.is_empty() {
                    query = format!("{} {}",previous.query,query);
                }
            }
        }
        if kinds.is_empty() && !negative && refinement && !normalized.iter().any(|word| word == "all") {
            if let Some(previous) = previous { kinds.clone_from(&previous.kinds); }
        }
        Self { query: query.chars().take(2000).collect(), kinds }
    }
}
