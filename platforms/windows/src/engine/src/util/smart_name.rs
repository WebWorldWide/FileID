use std::collections::BTreeSet;

pub fn readable_stem(raw: &str, confirmed_subjects: &[String]) -> Option<String> {
    let mut words: Vec<String> = raw.split(|c: char| c.is_whitespace() || c == '-' || c == '_').filter(|s|!s.is_empty()).map(str::to_owned).collect();
    for prefix in [&["a","photo","of"][..], &["photo","of"][..], &["an","image","of"][..], &["image","of"][..], &["a","video","of"][..], &["video","of"][..]] {
        if words.iter().take(prefix.len()).map(|s|s.to_lowercase()).eq(prefix.iter().map(|s|s.to_string())) { words.drain(..prefix.len()); break; }
    }
    let subjects: Vec<String> = confirmed_subjects.iter().map(|s|s.trim().to_string()).filter(|s|!s.is_empty()).collect::<BTreeSet<_>>().into_iter().collect();
    let subject_words: BTreeSet<String> = subjects.iter().flat_map(|s|s.split_whitespace().map(str::to_lowercase)).collect();
    while words.first().is_some_and(|s|subject_words.contains(&s.to_lowercase())) { words.remove(0); }
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
}
