//! Judges CLIP text search hits: a photo is a good match when its score for
//! "a photo of <words>." beats its score for a generic photo by the shared
//! margin. A fixed cosine floor fails because some photos score high for every
//! query; the generic baseline cancels that. Values come from
//! shared/models/clip-search-baseline.json (calibration in DECISIONS.md) and
//! mirror Swift `SearchStrength`.

use std::sync::OnceLock;

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Baseline {
    model: String,
    strong_margin: f32,
    baseline: Vec<f32>,
}

fn baseline() -> &'static Baseline {
    static BASELINE: OnceLock<Baseline> = OnceLock::new();
    BASELINE.get_or_init(|| {
        serde_json::from_str(include_str!("../../../../../../shared/models/clip-search-baseline.json"))
            .expect("shared/models/clip-search-baseline.json is valid")
    })
}

pub fn applies() -> bool {
    let shared = baseline();
    shared.model == super::clip_space::model_id() && shared.baseline.len() == super::clip_space::DIMENSION
}

pub fn prompt(words: &str) -> String {
    format!("a photo of {words}.")
}

/// The vector that ranks and judges a text search: the photo prompt when the
/// text model can embed it, else the client's vector.
pub fn scoring_vector(query: &str, supplied: &[f32], encode: &dyn Fn(&str) -> Option<Vec<f32>>) -> Vec<f32> {
    let words = query.trim();
    if words.is_empty() {
        return supplied.to_vec();
    }
    encode(&prompt(words)).filter(|vector| vector.len() == supplied.len()).unwrap_or_else(|| supplied.to_vec())
}

/// The installed text model's embedding of `text`, without downloading.
pub fn live_encoder(text: &str) -> Option<Vec<f32>> {
    super::clip_text::default_weights_path()
        .is_ok_and(|path| path.exists())
        .then(|| crate::commands::embed::embed_text_blocking(text).ok())
        .flatten()
}

/// (cosine, margin over the baseline), summed in index order as Swift does.
pub fn score(query: &[f32], embedding: &[f32]) -> (f32, f32) {
    let base = &baseline().baseline;
    let mut cosine = 0f32;
    let mut generic = 0f32;
    for index in 0..query.len().min(embedding.len()).min(base.len()) {
        cosine += query[index] * embedding[index];
        generic += base[index] * embedding[index];
    }
    (cosine, cosine - generic)
}

pub fn is_strong(margin: f32) -> bool {
    margin >= baseline().strong_margin
}

pub fn summary(good: usize, weak: usize) -> String {
    let noun = if good == 1 { "match" } else { "matches" };
    match (good, weak) {
        (0, 0) => "No matches.".into(),
        (0, _) => format!("No good matches, {weak} weaker."),
        (_, 0) => format!("{good} good {noun}."),
        _ => format!("{good} good {noun}, {weak} weaker."),
    }
}

const SIMILAR_WORDS: [&str; 8] = ["similar", "closest", "nearest", "lookalike", "lookalikes", "resemble", "resembles", "resembling"];

/// "show the closest photos of a dog" keeps weaker look-alikes.
pub fn wants_similar(text: &str) -> bool {
    text.to_lowercase().split(|c: char| !c.is_alphabetic()).any(|word| SIMILAR_WORDS.contains(&word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Deserialize)]
    struct Fixture {
        cases: Vec<Case>,
        summaries: Vec<SummaryCase>,
        similar: Vec<SimilarCase>,
    }
    #[derive(Deserialize)]
    struct Case {
        name: String,
        query: std::collections::BTreeMap<usize, f32>,
        photos: Vec<Photo>,
        order: Vec<i64>,
        strong: Vec<i64>,
    }
    #[derive(Deserialize)]
    struct Photo {
        id: i64,
        vector: std::collections::BTreeMap<usize, f32>,
        margin: f32,
    }
    #[derive(Deserialize)]
    struct SummaryCase {
        good: usize,
        weak: usize,
        text: String,
    }
    #[derive(Deserialize)]
    struct SimilarCase {
        text: String,
        similar: bool,
    }

    fn dense(sparse: &std::collections::BTreeMap<usize, f32>) -> Vec<f32> {
        let mut vector = vec![0f32; super::super::clip_space::DIMENSION];
        for (index, value) in sparse {
            vector[*index] = *value;
        }
        vector
    }

    fn fixture() -> Fixture {
        serde_json::from_str(include_str!("../../../../../../shared/test-corpus/search-strength.json")).unwrap()
    }

    #[test]
    fn baseline_is_the_current_model_and_unit_length() {
        assert!(applies());
        let norm: f64 = baseline().baseline.iter().map(|v| f64::from(*v).powi(2)).sum();
        assert!((norm - 1.0).abs() < 1e-5);
    }

    #[test]
    fn shared_cases_rank_and_judge_like_swift() {
        for case in fixture().cases {
            let query = dense(&case.query);
            let mut scored: Vec<(i64, f32)> = case
                .photos
                .iter()
                .map(|photo| {
                    let margin = score(&query, &dense(&photo.vector)).1;
                    assert!((margin - photo.margin).abs() < 1e-4, "{}: photo {} margin {margin}", case.name, photo.id);
                    (photo.id, margin)
                })
                .collect();
            scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            assert_eq!(scored.iter().map(|s| s.0).collect::<Vec<_>>(), case.order, "{}", case.name);
            let strong: Vec<i64> = scored.iter().filter(|s| is_strong(s.1)).map(|s| s.0).collect();
            assert_eq!(strong, case.strong, "{}", case.name);
        }
    }

    #[test]
    fn shared_summaries_and_similar_wording() {
        let fixture = fixture();
        for case in fixture.summaries {
            assert_eq!(summary(case.good, case.weak), case.text);
        }
        for case in fixture.similar {
            assert_eq!(wants_similar(&case.text), case.similar, "{}", case.text);
        }
    }

    #[test]
    fn scoring_falls_back_to_the_client_vector() {
        let supplied = dense(&[(0, 1.0)].into_iter().collect());
        assert_eq!(scoring_vector("dog", &supplied, &|_| None), supplied);
        assert_eq!(scoring_vector("  ", &supplied, &|_| panic!("no prompt for empty words")), supplied);
        let prompted = dense(&[(1, 1.0)].into_iter().collect());
        let seen = std::sync::Mutex::new(String::new());
        let encode = |text: &str| {
            *seen.lock().unwrap() = text.to_owned();
            Some(prompted.clone())
        };
        assert_eq!(scoring_vector(" dog ", &supplied, &encode), prompted);
        assert_eq!(*seen.lock().unwrap(), "a photo of dog.");
        assert_eq!(scoring_vector("dog", &supplied, &|_| Some(vec![1.0])), supplied);
    }
}
