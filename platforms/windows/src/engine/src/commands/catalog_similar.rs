use super::catalog::response;
use crate::ipc::{CatalogRequest, CatalogResponse, CatalogSimilarGroup};
use crate::read::dupes::{self, DEFAULT_SIMILAR_THRESHOLD, MAX_SIMILAR_THRESHOLD};
use anyhow::{bail, Result};
use rusqlite::Connection;
use std::collections::HashMap;

const DEFAULT_PAGE: usize = 50;
const MAX_PAGE: usize = 200;
const MAX_MEMBERS: usize = 2000;

pub(crate) fn page(conn: &Connection, request: &CatalogRequest) -> Result<CatalogResponse> {
    let threshold = request.similar_threshold.map_or(DEFAULT_SIMILAR_THRESHOLD, |bits| bits as u32);
    if threshold > MAX_SIMILAR_THRESHOLD {
        bail!("Invalid look-alike threshold")
    }
    let limit = request.limit.unwrap_or(DEFAULT_PAGE);
    if !(1..=MAX_PAGE).contains(&limit) {
        bail!("Invalid look-alike page size")
    }
    let offset = request.offset.unwrap_or(0).max(0) as usize;
    let mut groups = dupes::similar_groups(conn, threshold, || false)?.unwrap_or_default();
    drop_byte_identical(conn, &mut groups)?;
    groups.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
    let mut result = response(&request.request_id, "ok", None);
    result.similar_group_total = Some(groups.len() as i64);
    result.similar_groups = Some(
        groups
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|mut ids| {
                let total_count = ids.len() as i64;
                ids.truncate(MAX_MEMBERS);
                CatalogSimilarGroup { file_ids: ids, total_count }
            })
            .collect(),
    );
    Ok(result)
}

/// A group whose members all share one content hash is an exact-duplicate group, which Cleanup's Exact view already shows.
fn drop_byte_identical(conn: &Connection, groups: &mut Vec<Vec<i64>>) -> Result<()> {
    let ids: Vec<i64> = groups.iter().flatten().copied().collect();
    let mut hashes: HashMap<i64, Option<Vec<u8>>> = HashMap::with_capacity(ids.len());
    let mut stmt = conn.prepare("SELECT id, content_hash FROM files WHERE id IN (SELECT value FROM json_each(?1))")?;
    for chunk in ids.chunks(5000) {
        let encoded = serde_json::to_string(chunk)?;
        let rows = stmt.query_map([encoded], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<Vec<u8>>>(1)?)))?;
        for row in rows {
            let (id, hash) = row?;
            hashes.insert(id, hash);
        }
    }
    groups.retain(|ids| {
        let first = hashes.get(&ids[0]).and_then(|hash| hash.as_ref());
        !(first.is_some() && ids.iter().all(|id| hashes.get(id).and_then(|hash| hash.as_ref()) == first))
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn library() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        conn
    }

    fn add(conn: &Connection, name: &str, hash: Option<&[u8]>, phash: i64) -> i64 {
        conn.execute(
            "INSERT INTO files (path_text, path_hash, size_bytes, modified_at, scanned_at, kind, extension, content_hash, phash) \
             VALUES (?1, 1, 10, 1.0, 1.0, 'image', 'jpg', ?2, ?3)",
            params![format!("/lib/{name}.jpg"), hash, phash],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn request(mut value: serde_json::Value) -> CatalogRequest {
        value["requestID"] = "r".into();
        value["action"] = "similarGroups".into();
        value.as_object_mut().unwrap().retain(|_, field| !field.is_null());
        serde_json::from_value(value).unwrap()
    }

    fn ask(conn: &Connection, offset: Option<i64>, limit: Option<i64>) -> CatalogResponse {
        page(conn, &request(serde_json::json!({"offset": offset, "limit": limit}))).unwrap()
    }

    #[test]
    fn look_alikes_come_largest_first_and_page_without_repeating() {
        let conn = library();
        let a = [add(&conn, "a1", Some(b"x"), 0b1), add(&conn, "a2", Some(b"y"), 0b11)];
        let b: Vec<i64> = (0..3).map(|i| add(&conn, &format!("b{i}"), Some(&[i as u8]), (0xFFFF_FFFF_0000_0000u64 | i as u64) as i64)).collect();
        let first = ask(&conn, None, Some(1));
        assert_eq!(first.similar_group_total, Some(2));
        assert_eq!(first.similar_groups.as_ref().unwrap()[0].file_ids, b);
        let second = ask(&conn, Some(1), Some(1));
        assert_eq!(second.similar_groups.unwrap()[0].file_ids, a);
        assert!(ask(&conn, Some(2), Some(1)).similar_groups.unwrap().is_empty());
    }

    #[test]
    fn a_group_of_byte_identical_files_is_left_to_the_exact_view() {
        let conn = library();
        add(&conn, "p", Some(b"same"), 0b1);
        add(&conn, "q", Some(b"same"), 0b1);
        add(&conn, "r", Some(b"one"), 0xFFFF_FFFF_0000_0000u64 as i64);
        add(&conn, "s", Some(b"two"), 0xFFFF_FFFF_0000_0000u64 as i64);
        add(&conn, "t", None, 0x0000_FFFF_FFFF_0000u64 as i64);
        add(&conn, "u", None, 0x0000_FFFF_FFFF_0000u64 as i64);
        let reply = ask(&conn, None, None);
        assert_eq!(reply.similar_group_total, Some(2));
    }

    #[test]
    fn an_out_of_range_page_size_is_refused() {
        let conn = library();
        assert!(page(&conn, &request(serde_json::json!({"limit": 201}))).is_err());
        assert!(page(&conn, &request(serde_json::json!({"limit": 0}))).is_err());
    }

    #[test]
    fn one_request_can_carry_every_group_cleanup_displays() {
        let conn = library();
        add(&conn, "a", Some(b"x"), 0b1);
        add(&conn, "b", Some(b"y"), 0b11);
        assert!(page(&conn, &request(serde_json::json!({"limit": 200}))).is_ok(), "Cleanup shows at most 200 groups and asks for them in one request");
    }
}
