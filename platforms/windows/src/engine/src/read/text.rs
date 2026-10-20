/// Safe FTS5 MATCH expression: every whitespace token that contains a letter or
/// digit becomes a quoted phrase (embedded `"` doubled), implicitly AND-ed.
/// Punctuation-only tokens are dropped. Empty means "no usable terms" and must
/// not be passed to MATCH.
pub fn fts_phrase_query(raw: &str) -> String {
    raw.split_whitespace()
        .filter(|token| token.chars().any(char::is_alphanumeric))
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Escape `\`, `%` and `_` so the text matches literally under
/// `LIKE ?1 ESCAPE '\'`.
pub fn escape_like(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fts_query_quotes_tokens_and_drops_punctuation_only_terms() {
        assert_eq!(fts_phrase_query("beach  sunset"), "\"beach\" \"sunset\"");
        assert_eq!(fts_phrase_query("say \"hi\""), "\"say\" \"\"\"hi\"\"\"");
        assert_eq!(fts_phrase_query("foo-bar NEAR OR"), "\"foo-bar\" \"NEAR\" \"OR\"");
        assert_eq!(fts_phrase_query("café 東京"), "\"café\" \"東京\"");
        assert_eq!(fts_phrase_query(" - \" * "), "");
        assert_eq!(fts_phrase_query(""), "");
    }

    #[test]
    fn fts_query_is_always_valid_fts5() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE VIRTUAL TABLE t USING fts5(body); \
             INSERT INTO t(body) VALUES ('foo bar baz'), ('say \"hi\" there');",
        )
        .unwrap();
        for raw in ["foo-bar", "\"", "a\"b", "OR AND NOT", "(x", "foo*", "col:val", "say \"hi\""] {
            let query = fts_phrase_query(raw);
            if query.is_empty() {
                continue;
            }
            conn.query_row("SELECT COUNT(*) FROM t WHERE t MATCH ?1", [&query], |row| row.get::<_, i64>(0))
                .unwrap_or_else(|error| panic!("{raw:?} -> {query:?}: {error}"));
        }
        let hits: i64 = conn
            .query_row("SELECT COUNT(*) FROM t WHERE t MATCH ?1", [fts_phrase_query("foo-bar")], |row| row.get(0))
            .unwrap();
        assert_eq!(hits, 1);
    }

    #[test]
    fn escape_like_neutralizes_wildcards_and_escape_char() {
        assert_eq!(escape_like("report_2023.pdf"), "report\\_2023.pdf");
        assert_eq!(escape_like("50%off.png"), "50\\%off.png");
        assert_eq!(escape_like("a\\b_c%"), "a\\\\b\\_c\\%");
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let matched: bool = conn
            .query_row(
                "SELECT 'reportX2023.pdf' LIKE ?1 ESCAPE '\\'",
                [format!("%{}", escape_like("report_2023.pdf"))],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!matched);
    }
}
