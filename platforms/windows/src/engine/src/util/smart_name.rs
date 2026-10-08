use std::collections::BTreeSet;

pub fn readable_stem(raw: &str, confirmed_subjects: &[String]) -> Option<String> {
    let mut words: Vec<String> = raw.split(|c: char| c.is_whitespace() || c == '-' || c == '_').filter(|s|!s.is_empty()).map(str::to_owned).collect();
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
    let event = words.iter().map(|s| {let mut chars=s.chars();match chars.next() {Some(c)=>format!("{}{}",c.to_uppercase(),chars.as_str().to_lowercase()),None=>String::new()}}).collect::<Vec<_>>().join(" ");
    let prefix = if subjects.len() <= 2 {subjects.join(" & ")} else {String::new()};
    let raw = if prefix.is_empty() {event} else {format!("{prefix} - {event}")};
    let mut result: String = crate::util::path_safety::safe_filename_component(&raw).chars().take(60).collect();
    while result.len()>200 { result.pop(); }
    let result=result.trim_matches([' ','-','_']).to_string();
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
    fn person_prefixes_preserve_event_words() {
        assert_eq!(readable_stem("jones-beach-sunset", &["Alex Jones".into()]).as_deref(), Some("Alex Jones - Jones Beach Sunset"));
        assert_eq!(readable_stem("alex-jones-baseball-hit", &["Alex Jones".into()]).as_deref(), Some("Alex Jones - Baseball Hit"));
        assert_eq!(readable_stem("alex-and-mira-birthday-gift", &["Alex".into(), "Mira".into()]).as_deref(), Some("Alex & Mira - Birthday Gift"));
        assert_eq!(readable_stem("alex-&-mira-birthday-gift", &["Mira".into(), "Alex".into()]).as_deref(), Some("Alex & Mira - Birthday Gift"));
        assert_eq!(readable_stem("alex-and-sunrise", &["Alex".into()]).as_deref(), Some("Alex - And Sunrise"));
        assert_eq!(readable_stem("alex-mira-baseball-hit", &["Alex Jones".into(), "Mira Smith".into()]).as_deref(), Some("Alex Jones & Mira Smith - Baseball Hit"));
    }

}
