//! What a scan still owes a file after its main pass, and what it remembers
//! about files it could not read. Both live in `scan_catchup` (catalog v34),
//! shared with the Swift engine's `ScanCatchUp`, so either engine can finish
//! the other's work.

/// RAM++ tags are owed (`ScanCatchUp.tagsOwed`).
pub const TAGS_OWED: i64 = 1;
/// The careful text read is owed (`ScanCatchUp.carefulTextOwed`).
pub const CAREFUL_TEXT_OWED: i64 = 2;
/// The fast text read ran during the scan (`ScanCatchUp.textReadAtScan`).
pub const TEXT_READ_AT_SCAN: i64 = 4;
pub const OWED_MASK: i64 = TAGS_OWED | CAREFUL_TEXT_OWED;

/// What the writer stores in `scan_catchup` next to a file's scan results.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub stages: i64,
    /// The classifier context JSON (`ScanCatchUp.VisionContext`).
    pub vision: Option<String>,
    pub face_count: i64,
}

// MARK: - Files that keep failing

/// A file that could not be analysed is retried on this many scans, then left
/// alone until its size or date changes, the engine is updated, or the user
/// asks for a full rescan.
pub const FAILURE_RETRY_LIMIT: i64 = 3;

/// `scan_catchup.stages` of a failure-memory row. The value owes nothing
/// (no bit of `OWED_MASK`), so neither engine's catch-up pass selects it, and
/// neither engine writes it for an analysed file (a marker that owes nothing
/// is deleted). `attempts` counts the failures, `size_bytes`/`modified_at`
/// hold the file as it was, and `vision` names the engine build that failed.
pub const FAILURE_STAGE: i64 = TEXT_READ_AT_SCAN;

/// Names the engine and decoder set whose failures are remembered: a new
/// build, or a switch between the Mac adapters and the portable decoders,
/// retries every file.
pub fn failure_context() -> &'static str {
    static CONTEXT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    CONTEXT.get_or_init(|| {
        let decoders = if crate::pipeline::tagging::apple_scan_enabled() { "apple" } else { "portable" };
        format!("{{\"failedBy\":\"fileid-engine {} {decoders}\"}}", env!("CARGO_PKG_VERSION"))
    })
}

/// ?1 file id, ?2 size, ?3 modified, ?4 `failure_context()`, ?5 now.
pub const RECORD_FAILURE_SQL: &str = "INSERT INTO scan_catchup \
     (file_id, stages, size_bytes, modified_at, vision, face_count, attempts, created_at) \
     VALUES (?1, 4, ?2, ?3, ?4, 0, 1, ?5) \
     ON CONFLICT(file_id) DO UPDATE SET \
       attempts = CASE WHEN scan_catchup.stages = 4 AND scan_catchup.size_bytes = excluded.size_bytes \
                        AND scan_catchup.modified_at IS excluded.modified_at \
                        AND scan_catchup.vision IS excluded.vision \
                   THEN scan_catchup.attempts + 1 ELSE 1 END, \
       stages = 4, size_bytes = excluded.size_bytes, modified_at = excluded.modified_at, \
       vision = excluded.vision, face_count = 0, created_at = excluded.created_at";

pub const CLEAR_FAILURE_SQL: &str = "DELETE FROM scan_catchup WHERE file_id = ?1 AND stages = 4";

/// Failed rows that have used up their retries and still describe the file
/// on record. ?1 `failure_context()`; callers append a path-range clause.
pub const GIVEN_UP_SQL: &str = "SELECT f.path_text, f.size_bytes, f.modified_at \
     FROM scan_catchup c JOIN files f ON f.id = c.file_id \
     WHERE c.stages = 4 AND c.attempts >= 3 AND f.failed = 1 \
       AND c.size_bytes = f.size_bytes AND c.modified_at IS f.modified_at AND c.vision = ?1";

const _: () = assert!(FAILURE_STAGE == 4 && FAILURE_RETRY_LIMIT == 3 && FAILURE_STAGE & OWED_MASK == 0);

/// Whether a failure says something about the file. A file that cannot even
/// be opened right now (drive offline, permission, deleted) is not counted:
/// nothing about its size or date changes when that is fixed.
pub fn failure_is_about_the_file(path: &std::path::Path) -> bool {
    use std::io::Read;
    let long = crate::util::path_safety::to_extended_length(path);
    std::fs::File::open(long).and_then(|mut file| file.read(&mut [0u8; 1])).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_context_is_json_naming_this_build() {
        let parsed: serde_json::Value = serde_json::from_str(failure_context()).unwrap();
        let name = parsed["failedBy"].as_str().unwrap();
        assert!(name.starts_with("fileid-engine ") && name.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn an_unopenable_file_does_not_count_as_a_failure_of_the_file() {
        let dir = std::env::temp_dir().join(format!("fileid-failure-memory-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.jpg");
        std::fs::write(&broken, b"not a jpeg").unwrap();
        assert!(failure_is_about_the_file(&broken));
        assert!(!failure_is_about_the_file(&dir.join("missing.jpg")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
