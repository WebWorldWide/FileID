//! Catalogs written before scans read EXIF capture times (or before the RAW
//! readers existed) store the file's copy date for photos, and an incremental
//! scan skips unchanged files, so those dates would never be corrected. After a
//! library folder's next completed scan this reads each photo's EXIF time once
//! (file headers only, no decode) and fixes rows that differ by more than a
//! second. A marker per folder keeps it to one pass. Mirrors macOS
//! `CaptureDateBackfill`.

use std::path::{Path, PathBuf};

use anyhow::Result;
use parking_lot::Mutex;
use rusqlite::{params, Connection};
use sha2::Digest;

use crate::scan_session::{prefix_upper_bound, root_descendant_prefix};

const VERSION: &str = "v1";
const WRITE_CHUNK: usize = 500;

pub(crate) fn marker(state_dir: &Path, root: &Path) -> PathBuf {
    let root = root.to_string_lossy();
    let digest = sha2::Sha256::digest(root.trim_end_matches(['/', '\\']).as_bytes());
    state_dir.join(format!("capture-dates-{VERSION}")).join(hex::encode(&digest[..16]))
}

/// True when the catalog already has rows under `root`, i.e. a scan of it could
/// skip files whose stored date predates EXIF capture times.
pub(crate) fn catalog_has_rows(conn: &Connection, root: &Path) -> bool {
    let lo = root_descendant_prefix(root);
    match prefix_upper_bound(&lo) {
        Some(hi) => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM files WHERE path_text >= ?1 AND path_text < ?2)",
            params![lo, hi],
            |row| row.get(0),
        ),
        None => conn.query_row("SELECT EXISTS(SELECT 1 FROM files WHERE path_text >= ?1)", [lo], |row| row.get(0)),
    }
    .unwrap_or(true)
}

/// Marks `root` done without a pass: the scan just read every photo's EXIF.
pub(crate) fn mark_done(state_dir: &Path, root: &Path) {
    let mark = marker(state_dir, root);
    let written = mark.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&mark, b""));
    if let Err(error) = written {
        tracing::warn!(%error, "could not record the capture-date backfill marker");
    }
}

/// Runs the pass for `root` unless its marker exists. Returns the rows fixed.
/// Files are read with no database lock held; the writer is taken per chunk.
pub(crate) fn run_if_needed(database: &Mutex<Connection>, state_dir: &Path, root: &Path, cancelled: &dyn Fn() -> bool) -> usize {
    if marker(state_dir, root).exists() {
        return 0;
    }
    let photos = match photos_under(database, root) {
        Ok(photos) => photos,
        Err(error) => {
            tracing::warn!(%error, "capture-date backfill could not list photos");
            return 0;
        }
    };
    let mut fixes: Vec<(i64, f64)> = Vec::new();
    for (id, path, stored) in &photos {
        if cancelled() {
            return 0;
        }
        let path = Path::new(path);
        let readable_here = std::fs::metadata(path).is_ok_and(|metadata| !super::discovery::is_online_only(&metadata));
        let Some(captured) = readable_here.then(|| super::tagging::capture_time_from_head(path)).flatten() else {
            continue;
        };
        if stored.is_none_or(|stored| (stored - captured).abs() > 1.0) {
            fixes.push((*id, captured));
        }
    }
    for chunk in fixes.chunks(WRITE_CHUNK) {
        if let Err(error) = write_chunk(&mut database.lock(), chunk) {
            tracing::warn!(%error, "capture-date backfill could not update the catalog; retrying next scan");
            return 0;
        }
    }
    mark_done(state_dir, root);
    tracing::info!(checked = photos.len(), fixed = fixes.len(), "capture dates backfilled");
    fixes.len()
}

fn photos_under(database: &Mutex<Connection>, root: &Path) -> Result<Vec<(i64, String, Option<f64>)>> {
    let path = database.lock().path().filter(|path| !path.is_empty()).map(PathBuf::from);
    match path {
        Some(path) => query_photos(&crate::db::open_read(&path)?, root),
        None => query_photos(&database.lock(), root),
    }
}

fn query_photos(conn: &Connection, root: &Path) -> Result<Vec<(i64, String, Option<f64>)>> {
    let lo = root_descendant_prefix(root);
    let hi = prefix_upper_bound(&lo).unwrap_or_else(|| char::MAX.to_string());
    let mut statement = conn.prepare(
        "SELECT id, path_text, created_at FROM files \
         WHERE kind = 'image' AND failed = 0 AND path_text >= ?1 AND path_text < ?2",
    )?;
    let rows = statement
        .query_map(params![lo, hi], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn write_chunk(conn: &mut Connection, chunk: &[(i64, f64)]) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    {
        let mut update = tx.prepare_cached("UPDATE files SET created_at = ?1 WHERE id = ?2")?;
        for (id, captured) in chunk {
            update.execute(params![captured, id])?;
        }
    }
    tx.commit()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg_taken_at(text: &str, offset: &str, subsec: &str) -> Vec<u8> {
        let ascii = |tag: u16, value: &str| {
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            (tag, bytes)
        };
        let fields = [ascii(0x9003, text), ascii(0x9011, offset), ascii(0x9291, subsec)];
        let mut tiff = b"II\x2A\x00\x08\x00\x00\x00".to_vec();
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&[0x69, 0x87, 4, 0, 1, 0, 0, 0]);
        tiff.extend_from_slice(&26u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        let data_at = 26 + 2 + fields.len() * 12 + 4;
        let mut data = Vec::new();
        tiff.extend_from_slice(&(fields.len() as u16).to_le_bytes());
        for (tag, value) in &fields {
            tiff.extend_from_slice(&tag.to_le_bytes());
            tiff.extend_from_slice(&2u16.to_le_bytes());
            tiff.extend_from_slice(&(value.len() as u32).to_le_bytes());
            if value.len() <= 4 {
                let mut inline = value.clone();
                inline.resize(4, 0);
                tiff.extend_from_slice(&inline);
            } else {
                tiff.extend_from_slice(&((data_at + data.len()) as u32).to_le_bytes());
                data.extend_from_slice(value);
            }
        }
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&data);

        let mut encoded = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(8, 8, image::Rgb([90, 140, 200]))
            .write_to(&mut encoded, image::ImageFormat::Jpeg)
            .unwrap();
        let encoded = encoded.into_inner();
        let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1];
        out.extend_from_slice(&((tiff.len() + 8) as u16).to_be_bytes());
        out.extend_from_slice(b"Exif\0\0");
        out.extend_from_slice(&tiff);
        out.extend_from_slice(&encoded[2..]);
        out
    }

    #[test]
    fn copy_dates_on_photos_are_replaced_by_exif_times_once_per_folder() {
        let scratch = std::env::temp_dir().join(format!("fileid-capture-dates-{}", uuid::Uuid::new_v4()));
        let (root, state) = (scratch.join("Library"), scratch.join("state"));
        std::fs::create_dir_all(&root).unwrap();
        let dated = root.join("dated.jpg");
        let plain = root.join("plain.jpg");
        let close = root.join("close.jpg");
        std::fs::write(&dated, jpeg_taken_at("2019:07:04 12:00:00", "+00:00", "25")).unwrap();
        std::fs::write(&close, jpeg_taken_at("2019:07:04 12:00:00", "+00:00", "")).unwrap();
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3])).write_to(&mut encoded, image::ImageFormat::Jpeg).unwrap();
        std::fs::write(&plain, encoded.into_inner()).unwrap();
        let outside = scratch.join("Elsewhere.jpg");
        std::fs::write(&outside, jpeg_taken_at("2019:07:04 12:00:00", "+00:00", "")).unwrap();

        let conn = crate::db::test_support::memory_db();
        for (id, path, created) in [(1, &dated, 1_700_000_000.0), (2, &plain, 1_700_000_000.0), (3, &close, 1_562_241_600.5), (4, &outside, 1_700_000_000.0)] {
            conn.execute(
                "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,created_at) VALUES(?1,?2,?1,1,0,'image','jpg',?3)",
                params![id, path.to_str().unwrap(), created],
            )
            .unwrap();
        }
        assert!(catalog_has_rows(&conn, &root));
        assert!(!catalog_has_rows(&conn, &scratch.join("Empty")));
        let database = Mutex::new(conn);

        assert_eq!(run_if_needed(&database, &state, &root, &|| true), 0, "a cancelled pass changes nothing");
        assert!(!marker(&state, &root).exists());
        assert_eq!(run_if_needed(&database, &state, &root, &|| false), 1);
        let stored: Vec<f64> = {
            let conn = database.lock();
            let mut statement = conn.prepare("SELECT created_at FROM files ORDER BY id").unwrap();
            statement.query_map([], |row| row.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap()
        };
        assert_eq!(stored, [1_562_241_600.25, 1_700_000_000.0, 1_562_241_600.5, 1_700_000_000.0]);
        assert!(marker(&state, &root).exists());
        assert_eq!(marker(&state, &root), marker(&state, &root.join("")));

        database.lock().execute("UPDATE files SET created_at = 1700000000 WHERE id = 1", []).unwrap();
        assert_eq!(run_if_needed(&database, &state, &root, &|| false), 0, "the marker keeps it to one pass per folder");
        mark_done(&state, &scratch);
        assert!(marker(&state, &scratch).exists());
        std::fs::remove_dir_all(scratch).unwrap();
    }
}
