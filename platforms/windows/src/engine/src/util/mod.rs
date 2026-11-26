pub mod content_hash;
pub(crate) mod face_crop_stamp;
pub(crate) mod jpeg_complete;
pub(crate) mod video_container_dates;
pub(crate) mod video_creation_date;
pub mod display;
pub mod user_message;
pub(crate) mod hmac;
pub(crate) mod icc;
pub(crate) mod hnsw_index;
pub(crate) mod file_move;
pub(crate) mod keywords;
pub mod path_safety;
pub mod read_only;
#[allow(unused_imports)]
pub use path_safety::rename_no_replace;
pub(crate) mod zip;

pub mod smart_name;
pub(crate) mod smart_name_companions;
pub(crate) mod smart_name_learning;
pub(crate) mod smart_name_style;
pub mod model_memory;
pub(crate) mod qos;
pub(crate) mod vector_math;

/// Whole seconds plus nanoseconds / 1e9, the number the Swift engine stores (`FileModificationTime`); Foundation's
/// `timeIntervalSince1970` rounds the last bit differently for ~a quarter of timestamps. Pre-1970 has no value.
pub(crate) fn catalog_unix_seconds(time: std::time::SystemTime) -> Option<f64> {
    Some(time.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64())
}

/// A capture or creation date later than this many seconds after now is a clock or metadata error (a camera that was never set, a drive
/// that stamps 2040), not when the file was made. A day of slack covers time zones and a fast clock.
pub(crate) const FUTURE_DATE_SLACK_SECONDS: f64 = 86_400.0;

/// FAT and exFAT volumes report 1 January 1980 for a file with no date, so no file-system date before 1981 is real.
pub(crate) const EARLIEST_FILE_SYSTEM_DATE_UNIX: f64 = 347_155_200.0;

/// The date, unless it lies in the future.
pub(crate) fn not_in_the_future(time: f64, now: f64) -> Option<f64> {
    (time.is_finite() && time <= now + FUTURE_DATE_SLACK_SECONDS).then_some(time)
}

pub(crate) fn unix_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

/// SQL for the date a file was made: the stored capture or creation date, else the modified date, and neither when it lies in the future.
pub(crate) const SQL_TAKEN_DATE: &str = "CASE WHEN f.created_at <= CAST(strftime('%s','now') AS REAL) + 86400 THEN f.created_at \
     WHEN f.modified_at <= CAST(strftime('%s','now') AS REAL) + 86400 THEN f.modified_at END";

/// Within a microsecond: the scanners' rule, which also covers catalogs written before the engines stored one number.
pub(crate) fn same_modified_time(left: f64, right: f64) -> bool {
    left.is_finite() && right.is_finite() && (left - right).abs() < 0.000_001
}

/// Revisions are `"<size>:<IEEE bits of modified_at>"`.
pub(crate) fn same_source_revision(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    let (Some((left_size, left_bits)), Some((right_size, right_bits))) = (left.split_once(':'), right.split_once(':')) else {
        return false;
    };
    let (Ok(left_bits), Ok(right_bits)) = (left_bits.parse::<u64>(), right_bits.parse::<u64>()) else { return false };
    left_size == right_size && same_modified_time(f64::from_bits(left_bits), f64::from_bits(right_bits))
}

#[cfg(test)]
mod catalog_time_tests {
    use super::{catalog_unix_seconds, same_modified_time, same_source_revision};
    use std::time::{Duration, UNIX_EPOCH};

    /// (seconds, nanoseconds, bits of the stored double); pinned by the Swift `FileModificationTimeTests`.
    const TABLE: [(u64, u32, u64); 13] = [
        (1_724_989_629, 833_179_165, 0x41d9_b450_2f75_52cf),
        (1_603_520_313, 785_102_536, 0x41d7_e4f1_ce72_3f1f),
        (1_704_538_783, 171_373_719, 0x41d9_664c_a7ca_f7c9),
        (1_700_000_000, 0, 0x41d9_54fc_4000_0000),
        (1_719_409_671, 136_042_265, 0x41d9_9f07_01c8_b4eb),
        (1_635_498_135, 1_905_740, 0x41d8_5eee_25c0_1f39),
        (1_794_374_740, 262_792_743, 0x41da_bcff_1510_d199),
        (1_794_731_299, 518_318_480, 0x41da_be5b_48e1_2c21),
        (0, 0, 0),
        (0, 1, 0x3e11_2e0b_e826_d695),
        (1, 999_999_999, 0x3fff_ffff_ffbb_47d0),
        (2_147_483_648, 500_000_000, 0x41e0_0000_0010_0000),
        (4_102_444_800, 123_456_789, 0x41ee_90ca_e003_f35c),
    ];

    #[test]
    fn catalog_time_is_seconds_plus_nanoseconds_over_a_billion() {
        for (seconds, nanos, bits) in TABLE {
            let since = Duration::new(seconds, nanos);
            assert_eq!(since.as_secs_f64().to_bits(), bits, "{seconds}.{nanos:09}");
            // Windows keeps a `SystemTime` in 100 ns ticks, so only a whole tick survives there.
            if cfg!(unix) || nanos.is_multiple_of(100) {
                assert_eq!(catalog_unix_seconds(UNIX_EPOCH + since).unwrap().to_bits(), bits, "{seconds}.{nanos:09}");
            }
        }
    }

    #[test]
    fn a_time_before_1970_has_no_catalog_value() {
        assert_eq!(catalog_unix_seconds(UNIX_EPOCH - Duration::new(1, 500_000_000)), None);
    }

    #[test]
    fn times_a_bit_apart_are_the_same_instant() {
        let plain = f64::from_bits(4_745_314_456_662_256_673);
        let foundation = f64::from_bits(4_745_314_456_662_256_674);
        assert!(same_modified_time(plain, foundation));
        assert!(!same_modified_time(plain, plain + 0.001));
        assert!(!same_modified_time(plain, f64::NAN));
        let (left, right) = (format!("100:{}", plain.to_bits()), format!("100:{}", foundation.to_bits()));
        assert!(same_source_revision(&left, &right));
        assert!(!same_source_revision(&left, &format!("101:{}", foundation.to_bits())));
        assert!(!same_source_revision(&left, &format!("100:{}", (plain + 0.001).to_bits())));
        assert!(same_source_revision("100:unknown", "100:unknown"));
        assert!(!same_source_revision("100:unknown", &left));
    }
}
