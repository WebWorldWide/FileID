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
