//! `files.name_sort`: the key the Library sorts file names by (v38). Casefolded, diacritic-folded and numeric-aware, so a plain byte comparison of two keys reads like Finder's order and an index can serve the Name and Kind sorts. Written by the engine; `''` means "not computed yet" and sorts first until [`fill_missing`] runs.

use anyhow::Result;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::sync::OnceLock;

const BATCH: usize = 5_000;
const UNKEYED: &str = "\u{1}";

fn decompositions() -> &'static HashMap<char, char> {
    static MAP: OnceLock<HashMap<char, char>> = OnceLock::new();
    MAP.get_or_init(|| {
        crate::pipeline::dbwriter::NFC_LATIN_COMPOSE
            .iter()
            .filter_map(|&(base, _, composed)| Some((char::from_u32(composed)?, char::from_u32(base)?)))
            .collect()
    })
}

fn push_folded(out: &mut String, c: char) {
    if matches!(c as u32, 0x300..=0x36F) {
        return;
    }
    if c.is_ascii() {
        out.push(c.to_ascii_lowercase());
        return;
    }
    let mut c = c;
    while let Some(&base) = decompositions().get(&c) {
        c = base;
    }
    match c {
        'ß' => out.push_str("ss"),
        'æ' | 'Æ' => out.push_str("ae"),
        'œ' | 'Œ' => out.push_str("oe"),
        'ø' | 'Ø' => out.push('o'),
        'đ' | 'Đ' => out.push('d'),
        'ł' | 'Ł' => out.push('l'),
        'ı' => out.push('i'),
        _ => out.extend(c.to_lowercase().filter(|l| !matches!(*l as u32, 0x300..=0x36F))),
    }
}

/// The key for the last component of `path`. A run of digits becomes its length (two digits) then its value without leading zeros, so "file2" sorts before "file10".
pub fn name_sort_key(path: &str) -> String {
    let separators: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };
    let name = path.rsplit(separators).next().unwrap_or(path);
    let mut out = String::with_capacity(name.len() + 4);
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            let mut digits = String::from(c);
            while let Some(next) = chars.next_if(char::is_ascii_digit) {
                digits.push(next);
            }
            let trimmed = digits.trim_start_matches('0');
            let value = if trimmed.is_empty() { "0" } else { trimmed };
            let value = &value[..value.len().min(99)];
            out.push_str(&format!("{:02}{value}", value.len()));
        } else {
            push_folded(&mut out, c);
        }
    }
    if out.is_empty() { UNKEYED.to_string() } else { out }
}

/// Keys every row that has none, `BATCH` rows per transaction so a first open of a large catalog never holds the writer for long. Returns how many rows it keyed.
pub fn fill_missing(conn: &Connection) -> Result<usize> {
    let mut total = 0;
    loop {
        let rows: Vec<(i64, String)> = conn
            .prepare_cached("SELECT id, path_text FROM files WHERE name_sort = '' LIMIT ?1")?
            .query_map([BATCH as i64], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        if rows.is_empty() {
            return Ok(total);
        }
        let tx = conn.unchecked_transaction()?;
        {
            let mut update = tx.prepare_cached("UPDATE files SET name_sort = ?2 WHERE id = ?1")?;
            for (id, path) in &rows {
                update.execute(params![id, name_sort_key(path)])?;
            }
        }
        tx.commit()?;
        total += rows.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ordered(names: &[&str]) {
        let keys: Vec<String> = names.iter().map(|n| name_sort_key(&format!("/lib/{n}"))).collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "{names:?} -> {keys:?}");
    }

    #[test]
    fn numbers_inside_names_sort_by_value() {
        ordered(&["file2.jpg", "file10.jpg", "file100.jpg"]);
        ordered(&["IMG_0099.jpg", "IMG_0100.jpg", "IMG_1000.jpg"]);
    }

    #[test]
    fn case_and_accents_do_not_split_names() {
        assert_eq!(name_sort_key("/a/Éclair.JPG"), name_sort_key("/b/eclair.jpg"));
        assert_eq!(name_sort_key("/a/Cafe\u{301}.txt"), name_sort_key("/a/café.txt"));
        ordered(&["apple.jpg", "Bäcker.jpg", "cherry.jpg", "Zebra.jpg"]);
    }

    #[test]
    fn the_key_ignores_the_folders() {
        assert_eq!(name_sort_key("/z/z/a.jpg"), name_sort_key("/a/a/a.jpg"));
    }

    #[test]
    fn a_name_always_has_a_key_that_is_not_the_unkeyed_marker() {
        assert_ne!(name_sort_key("/a/"), "");
        assert_ne!(name_sort_key("/a/\u{301}"), "");
    }

    #[test]
    fn rows_without_a_key_get_one_in_batches_and_renames_clear_it() {
        let conn = crate::db::test_support::memory_db();
        for id in 1..=(BATCH as i64 + 7) {
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?1,?2,?1,1,0,'image','jpg')",
                params![id, format!("/lib/file{id}.jpg")],
            )
            .unwrap();
        }
        conn.execute("UPDATE files SET name_sort = ''", []).unwrap();
        assert_eq!(fill_missing(&conn).unwrap(), BATCH + 7);
        let key: String = conn.query_row("SELECT name_sort FROM files WHERE id = 3", [], |r| r.get(0)).unwrap();
        assert_eq!(key, name_sort_key("/lib/file3.jpg"));
        conn.execute("UPDATE files SET path_text = '/lib/renamed.jpg' WHERE id = 3", []).unwrap();
        let stale: String = conn.query_row("SELECT name_sort FROM files WHERE id = 3", [], |r| r.get(0)).unwrap();
        assert_eq!(stale, "", "a path change that leaves the key alone marks it for recomputing");
        assert_eq!(fill_missing(&conn).unwrap(), 1);
    }
}
