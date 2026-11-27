use std::collections::BTreeSet;

pub fn readable_stem(raw: &str, confirmed_subjects: &[String]) -> Option<String> {
    let mut words: Vec<String> = raw.split(|c: char| c.is_whitespace() || c == '-' || c == '_').map(|s| s.trim_start_matches('.')).filter(|s|!s.is_empty()).map(str::to_owned).collect();
    for prefix in [&["a","photo","of"][..], &["photo","of"][..], &["an","image","of"][..], &["image","of"][..], &["a","video","of"][..], &["video","of"][..]] {
        if words.iter().take(prefix.len()).map(|s|s.to_lowercase()).eq(prefix.iter().map(|s|(*s).to_string())) { words.drain(..prefix.len()); break; }
    }
    let subjects: Vec<String> = confirmed_subjects.iter().map(|s|s.trim().to_string()).filter(|s|!s.is_empty()).collect::<BTreeSet<_>>().into_iter().collect();
    let subject_prefixes: Vec<Vec<String>> = subjects.iter().flat_map(|subject| {
        let full: Vec<String> = subject.split(|c: char| c.is_whitespace() || c == '-' || c == '_')
            .filter(|s| !s.is_empty()).map(str::to_lowercase).collect();
        match full.first() {
            Some(first) => vec![full.clone(), vec![first.clone()]],
            None => vec![],
        }
    }).collect();
    let prefix_length = |candidate: &[String]| {
        subject_prefixes.iter().filter(|prefix| {
            candidate.len() >= prefix.len() && candidate.iter().zip(prefix.iter())
                .all(|(word, expected)| word.to_lowercase() == *expected)
        }).map(Vec::len).max()
    };
    while let Some(count) = prefix_length(&words) {
        words.drain(..count);
        if words.first().is_some_and(|word| ["and", "&"].contains(&word.to_lowercase().as_str()))
            && prefix_length(&words[1..]).is_some() {
            words.remove(0);
        }
    }
    let generic = ["untitled","filename","file","photo","picture","image","video","document"];
    if words.is_empty() || !words.iter().any(|s|!generic.contains(&s.to_lowercase().as_str())) { return None; }
    let event = words.iter().map(|s| {
        if s.chars().skip(1).any(char::is_uppercase) { return s.clone(); }
        let mut chars=s.chars();
        match chars.next() {Some(c)=>format!("{}{}",c.to_uppercase(),chars.as_str().to_lowercase()),None=>String::new()}
    }).collect::<Vec<_>>().join(" ");
    let prefix = if subjects.len() <= 2 {subjects.join(" & ")} else {String::new()};
    let raw = if prefix.is_empty() {event} else {format!("{prefix} - {event}")};
    let safe = crate::util::path_safety::safe_filename_component(&raw);
    let mut result: String = safe.chars().take(60).collect();
    if safe.chars().nth(60).is_some_and(|next| !next.is_whitespace())
        && let Some(space) = result.rfind(' ') {
        result.truncate(space);
    }
    while result.len()>200 { result.pop(); }
    let result=result.trim_matches([' ','-','_','.']).to_string();
    if result.is_empty() {None} else {Some(result)}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn produces_readable_evidence_names() {
        assert_eq!(readable_stem("alex-baseball-hit", &["Alex".into()]).as_deref(),Some("Alex - Baseball Hit"));
        assert_eq!(readable_stem("birthday-gift-opening", &[]).as_deref(),Some("Birthday Gift Opening"));
        assert_eq!(readable_stem("roof-repair-estimate", &[]).as_deref(),Some("Roof Repair Estimate"));
        assert!(readable_stem("untitled", &[]).is_none());
        assert!(readable_stem(&"longword-".repeat(40),&[]).unwrap().chars().count()<=60);
    }
    #[test]
    fn a_long_name_is_cut_at_a_word_and_capitals_inside_words_survive() {
        let cut = readable_stem("birthday-gift-opening", &["Alex".into(), "Mira".into()]).unwrap();
        assert_eq!(cut, "Alex & Mira - Birthday Gift Opening");
        let long = readable_stem("birthday-gift-opening-party-with-the-whole-extended-family", &["Alex".into(), "Mira".into()]).unwrap();
        assert!(long.chars().count() <= 60, "{long}");
        assert!(long.ends_with("Gift") || long.ends_with("Opening") || long.ends_with("Party") || long.ends_with("The") || long.ends_with("With") || long.ends_with("Whole"), "{long}");
        assert!(!long.ends_with("Ope") && !long.ends_with("Part"));
        assert_eq!(readable_stem("NASA-iPhone-launch", &[]).as_deref(), Some("NASA iPhone Launch"));
    }

    #[test]
    fn a_name_that_starts_with_dots_never_hides_the_renamed_file() {
        assert_eq!(readable_stem("...sunset-beach", &[]).as_deref(), Some("Sunset Beach"));
        assert!(readable_stem("....", &[]).is_none());
    }

    #[test]
    fn person_prefixes_preserve_event_words() {
        assert_eq!(readable_stem("jones-beach-sunset", &["Alex Jones".into()]).as_deref(), Some("Alex Jones - Jones Beach Sunset"));
        assert_eq!(readable_stem("alex-jones-baseball-hit", &["Alex Jones".into()]).as_deref(), Some("Alex Jones - Baseball Hit"));
        assert_eq!(readable_stem("alex-and-mira-birthday-gift", &["Alex".into(), "Mira".into()]).as_deref(), Some("Alex & Mira - Birthday Gift"));
        assert_eq!(readable_stem("alex-&-mira-birthday-gift", &["Mira".into(), "Alex".into()]).as_deref(), Some("Alex & Mira - Birthday Gift"));
        assert_eq!(readable_stem("alex-and-sunrise", &["Alex".into()]).as_deref(), Some("Alex - And Sunrise"));
        assert_eq!(readable_stem("alex-mira-baseball-hit", &["Alex Jones".into(), "Mira Smith".into()]).as_deref(), Some("Alex Jones & Mira Smith - Baseball Hit"));
    }
}
