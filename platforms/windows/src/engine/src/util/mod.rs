//! Small cross-cutting helpers used by multiple command handlers.
//!
//! Each submodule is leaf-level (no engine state, no cross-deps) so handlers
//! can pull them in without dragging in the wider command graph.

pub mod content_hash;
pub mod display;
pub(crate) mod hmac;
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
pub(crate) mod model_memory;

/// File timestamps as catalog seconds since 1970. The app-hosted Mac engine
/// shares its catalog with the Swift engine, which compares stored mtimes
/// exactly against Foundation's `Date` (seconds since 2001 plus nanoseconds,
/// then shifted to 1970); rounding the same way keeps those rows current.
pub(crate) fn catalog_unix_seconds(time: std::time::SystemTime) -> Option<f64> {
    let since = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(if cfg!(all(target_os = "macos", feature = "apple-adapters")) {
        foundation_unix_seconds(since)
    } else {
        since.as_secs_f64()
    })
}

const FOUNDATION_REFERENCE_DATE: f64 = 978_307_200.0;

fn foundation_unix_seconds(since: std::time::Duration) -> f64 {
    since.as_secs() as f64 - FOUNDATION_REFERENCE_DATE + 1.0e-9 * f64::from(since.subsec_nanos())
        + FOUNDATION_REFERENCE_DATE
}

#[cfg(test)]
mod catalog_time_tests {
    use super::foundation_unix_seconds;
    use std::time::Duration;

    #[test]
    fn foundation_rounding_matches_the_swift_engine_values() {
        // Values the Swift engine stored for copied Sony files whose plain
        // `as_secs_f64` differs in the last bit.
        assert_eq!(foundation_unix_seconds(Duration::new(1_794_374_740, 262_792_743)), 1794374740.2627926);
        assert_ne!(Duration::new(1_794_374_740, 262_792_743).as_secs_f64(), 1794374740.2627926);
    }
}
