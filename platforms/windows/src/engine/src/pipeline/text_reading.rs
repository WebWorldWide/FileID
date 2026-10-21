//! When a file's text deserves a careful read: the scan's OCR gate and Deep
//! Analyze's VLM transcription. Mirrors macOS `TextReading.swift` rule for rule
//! (macOS adds Vision's accurate tier in the scan; this engine's OCR has no
//! handwriting tier, so the VLM transcription is its handwriting path).

use rusqlite::{params, Connection, OptionalExtension};

pub(crate) const MINIMUM_READABLE_CHARACTERS: usize = 24;
const GARBLED_SHARE_THRESHOLD: f64 = 0.3;
const MAX_TRANSCRIPTION_CHARACTERS: usize = 4_000;

const HANDWRITING_HINTS: &[&str] = &[
    "handwriting", "whiteboard", "blackboard", "chalkboard", "chalk", "sticky note", "notebook", "notepad", "note",
    "manuscript", "calligraphy", "signature", "writing", "write", "journal", "diary", "letter", "envelope", "postcard",
    "greeting card",
];

const PAPER_HINTS: &[&str] = &[
    "document", "printed page", "paper", "receipt", "bill", "form", "contract", "certificate", "recipe", "list", "menu",
    "newspaper", "poster", "screenshot", "sign", "diagram", "calendar", "checkbook", "text", "presentation",
];

pub(crate) const TRANSCRIPTION_PROMPT: &str = "Transcribe every word of handwritten or printed text visible in this image, exactly as written, one line per line of text. Do not describe the image and do not guess at illegible words. If there is no readable text, reply NONE. End with one final line: HANDWRITTEN: yes if most of the text is handwritten, otherwise HANDWRITTEN: no.";

fn normalized(tag: &str) -> String {
    tag.to_lowercase().replace('_', " ").trim().to_owned()
}

pub(crate) fn is_text_bearing<S: AsRef<str>>(tags: &[S]) -> bool {
    tags.iter().any(|t| {
        let tag = normalized(t.as_ref());
        HANDWRITING_HINTS.contains(&tag.as_str()) || PAPER_HINTS.contains(&tag.as_str())
    })
}

/// A scanner-shaped page (US Letter 11:8.5 or A4 √2, either orientation) with no
/// camera EXIF. Tight bands, because 4:3 is every camera-less phone export.
pub(crate) fn is_document_shape(width: u32, height: u32, has_camera: bool) -> bool {
    if has_camera || width == 0 || height == 0 {
        return false;
    }
    let ratio = f64::from(width.max(height)) / f64::from(width.min(height));
    (ratio - 11.0 / 8.5).abs() < 0.015 || (ratio - std::f64::consts::SQRT_2).abs() < 0.015
}

/// Words a character-level reader typically mangles on handwriting: case flips
/// inside a word, symbols between letters, or five consonants in a row.
fn is_garbled_word(raw: &str) -> bool {
    let word = raw.trim_matches(|c: char| ".,:;!?'\"()[]{}".contains(c));
    let chars: Vec<char> = word.chars().collect();
    if chars.iter().filter(|c| c.is_alphabetic()).count() < 2 {
        return false;
    }
    let mut consonant_run = 0;
    for (index, &char) in chars.iter().enumerate() {
        if index > 0 {
            let previous = chars[index - 1];
            if previous.is_lowercase() && char.is_uppercase() {
                return true;
            }
            if char.is_lowercase()
                && index >= 2
                && previous.is_uppercase()
                && chars[index - 2].is_uppercase()
                && !(char == 's' && index == chars.len() - 1)
            {
                return true;
            }
            if !char.is_alphabetic()
                && !char.is_numeric()
                && !"'-.".contains(char)
                && previous.is_alphabetic()
                && chars.get(index + 1).is_some_and(|c| c.is_alphabetic())
            {
                return true;
            }
        }
        if char.is_alphabetic() && !"aeiouyAEIOUY".contains(char) {
            consonant_run += 1;
            if consonant_run >= 5 {
                return true;
            }
        } else {
            consonant_run = 0;
        }
    }
    false
}

pub(crate) fn garbled(text: &str) -> (usize, f64) {
    let words: Vec<&str> = text
        .split_whitespace()
        .filter(|w| w.chars().filter(|c| c.is_alphabetic()).count() >= 2)
        .collect();
    if words.is_empty() {
        return (0, 0.0);
    }
    let count = words.iter().filter(|w| is_garbled_word(w)).count();
    (count, count as f64 / words.len() as f64)
}

/// Deep Analyze: should the VLM transcribe this file's text? Text-bearing images
/// whose stored reading is missing, short or mangled, and PDFs with no text layer
/// and no OCR. Never repeats a VLM transcription.
pub(crate) fn needs_transcription<S: AsRef<str>>(
    kind: &str,
    tags: &[S],
    ocr_text: Option<&str>,
    ocr_source: Option<&str>,
    has_doc_text: bool,
) -> bool {
    if ocr_source == Some("vlm") {
        return false;
    }
    let compact = ocr_text.unwrap_or("").trim();
    let length = compact.chars().count();
    match kind {
        "pdf" => !has_doc_text && length < MINIMUM_READABLE_CHARACTERS,
        "image" => {
            is_text_bearing(tags)
                && (length < MINIMUM_READABLE_CHARACTERS || garbled(compact).1 >= GARBLED_SHARE_THRESHOLD)
        }
        _ => false,
    }
}

/// Parsed transcription, or `None` when the model saw no readable text.
pub(crate) fn parse_transcription(raw: &str) -> Option<(String, bool)> {
    let mut handwritten = false;
    let mut lines = Vec::new();
    for line in raw.replace('\r', "").split('\n') {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("```") {
            continue;
        }
        let upper = trimmed.to_uppercase();
        if let Some(rest) = upper.strip_prefix("HANDWRITTEN:") {
            handwritten = rest.trim().starts_with("YES");
            continue;
        }
        lines.push(trimmed.to_owned());
    }
    let text: String = lines.join("\n").chars().take(MAX_TRANSCRIPTION_CHARACTERS).collect();
    let text = text.trim().to_owned();
    let folded = text.to_uppercase();
    let folded = folded.trim_matches(|c| c == '.' || c == ' ');
    if text.is_empty() || folded == "NONE" || folded.starts_with("NO READABLE TEXT") || folded.starts_with("THE IMAGE") {
        return None;
    }
    Some((text, handwritten))
}

pub(crate) fn transcription_candidate(conn: &Connection, file_id: i64) -> rusqlite::Result<bool> {
    let row: Option<(String, Option<String>, Option<String>, bool)> = conn
        .query_row(
            "SELECT f.kind,o.text,o.source,EXISTS(SELECT 1 FROM doc_text d WHERE d.file_id=f.id AND length(trim(d.text))>0) \
             FROM files f LEFT JOIN ocr_text o ON o.file_id=f.id WHERE f.id=?1 AND f.failed=0",
            params![file_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((kind, text, source, has_doc)) = row else {
        return Ok(false);
    };
    let tags: Vec<String> = conn
        .prepare_cached("SELECT tag FROM tags WHERE file_id=?1 AND source IN ('auto','user','vlm')")?
        .query_map(params![file_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(needs_transcription(&kind, &tags, text.as_deref(), source.as_deref(), has_doc))
}

pub(crate) fn store_transcription(conn: &Connection, file_id: i64, text: &str, handwritten: bool) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM ocr_text WHERE file_id=?1", params![file_id])?;
    conn.execute(
        "INSERT INTO ocr_text (file_id, text, source, handwritten) VALUES (?1, ?2, 'vlm', ?3)",
        params![file_id, text, i64::from(handwritten)],
    )?;
    conn.execute("UPDATE files SET has_text=1 WHERE id=?1", params![file_id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn garbled_reads_flag_handwriting_but_not_print() {
        let handwriting = "4roGeries for Swvbda\nZLIGGhbvbb avbd basbL\ntwo Levwovbs, oat\nGaLL AKvbt RosaLivbd";
        assert!(garbled(handwriting).1 >= GARBLED_SHARE_THRESHOLD);
        let whiteboard = "Quarterly roadMap\nship pelican beta\nfix syMc bugs\nowner.' PMitri";
        assert!(garbled(whiteboard).1 >= GARBLED_SHARE_THRESHOLD);
        let receipt = "CORNER MARKET\nBananas 1 . 2 9\nCof fee beans 11.99\nTOTAL 13.28\nThank you";
        assert_eq!(garbled(receipt).0, 0);
        assert!(!is_garbled_word("PDFs"));
        assert!(is_garbled_word("denti&t"));
    }

    #[test]
    fn transcription_targets_unread_text_bearing_images_and_scanned_pdfs() {
        assert!(needs_transcription("image", &["whiteboard"], None, None, false));
        assert!(needs_transcription("image", &["Sticky_Note"], Some("wifi paoword denti&t"), Some("ocr"), false));
        assert!(!needs_transcription("image", &["dog"], None, None, false));
        assert!(!needs_transcription(
            "image",
            &["document"],
            Some("Quarterly revenue grew in every region this year"),
            Some("ocr"),
            false
        ));
        assert!(!needs_transcription("image", &["whiteboard"], Some("x"), Some("vlm"), false));
        assert!(needs_transcription::<&str>("pdf", &[], None, None, false));
        assert!(!needs_transcription::<&str>("pdf", &[], None, None, true));
        assert!(!needs_transcription("video", &["whiteboard"], None, None, false));
    }

    #[test]
    fn document_shape_matches_letter_and_a4_but_not_photos() {
        assert!(is_document_shape(1700, 2200, false));
        assert!(is_document_shape(2480, 3508, false));
        assert!(!is_document_shape(4032, 3024, false));
        assert!(!is_document_shape(1700, 2200, true));
    }

    #[test]
    fn parse_transcription_strips_marker_and_rejects_none() {
        assert_eq!(
            parse_transcription("Groceries for Sunday\nzucchini and basil\nHANDWRITTEN: yes"),
            Some(("Groceries for Sunday\nzucchini and basil".to_owned(), true))
        );
        assert_eq!(parse_transcription("NONE\nHANDWRITTEN: no"), None);
        assert_eq!(parse_transcription("  none. "), None);
        assert_eq!(parse_transcription("EXIT\nHANDWRITTEN: no"), Some(("EXIT".to_owned(), false)));
    }

    #[test]
    fn stored_transcription_survives_rescan_until_the_file_changes() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn.execute(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, modified_at) \
             VALUES (1, '/n.jpg', 1, 10, 0, 'image', 'jpg', 5)",
            [],
        )
        .unwrap();
        conn.execute("INSERT INTO tags (file_id, tag, source) VALUES (1, 'whiteboard', 'auto')", []).unwrap();
        assert!(transcription_candidate(&conn, 1).unwrap());
        store_transcription(&conn, 1, "ship pelican beta", true).unwrap();
        assert!(!transcription_candidate(&conn, 1).unwrap());
        let hits: i64 = conn
            .query_row("SELECT COUNT(*) FROM ocr_fts WHERE ocr_fts MATCH 'pelican'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(hits, 1);
        conn.execute("UPDATE files SET modified_at=6 WHERE id=1", []).unwrap();
        let left: i64 = conn.query_row("SELECT COUNT(*) FROM ocr_text", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0);
    }
}
