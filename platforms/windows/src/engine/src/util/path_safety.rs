//! Path-traversal + filename-safety guards for IPC handlers.

use std::path::{Component, Path, PathBuf};

/// Returns true iff `name` is exactly one Normal path component:
/// no slashes, no "..", no ".", no drive letter, no UNC, no leading/trailing
/// whitespace that the OS would silently strip. Used as the path-traversal
/// guard for `renameFiles`. Conservative — extra reject is safer than
/// extra allow when the destination is computed by joining to a directory.
pub(crate) fn is_safe_filename(name: &str) -> bool {
    if name.is_empty() || name.trim() != name {
        return false;
    }
    if name == "." || name == ".." {
        return false;
    }
    // SEC: trailing dot or space is a Windows quirk that resolves to a
    // different file than the literal name. Reject either side.
    if name.ends_with('.') || name.ends_with(' ') {
        return false;
    }
    // SEC: reject ANY occurrence of a path separator. `Path::components()`
    // silently strips trailing separators ("A\\" → ["A"]), which would
    // otherwise let "A\\" sneak past the single-component check below.
    if name.contains('/') || name.contains('\\') {
        return false;
    }
    let p = Path::new(name);
    if p.is_absolute() {
        return false;
    }
    let mut comps = p.components();
    let first = match comps.next() {
        Some(c) => c,
        None => return false,
    };
    if comps.next().is_some() {
        return false; // multi-component path — definitely not a filename
    }
    if !matches!(first, Component::Normal(_)) {
        return false;
    }
    // SEC: reject Windows reserved names (CON, PRN, AUX, NUL, COM0..9,
    // LPT0..9), with or without an extension. MoveFileExW returns
    // cryptic errors and on some shells "rename to NUL" silently
    // discards the file. COM0 + LPT0 are reserved per Microsoft Naming
    // Files docs even though the original COM/LPT numbering started at 1.
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    !matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL"
            | "COM0" | "COM1" | "COM2" | "COM3" | "COM4" | "COM5"
            | "COM6" | "COM7" | "COM8" | "COM9"
            | "LPT0" | "LPT1" | "LPT2" | "LPT3" | "LPT4" | "LPT5"
            | "LPT6" | "LPT7" | "LPT8" | "LPT9"
    )
}

/// SEC-7: best-effort canonicalize for a path that may not exist (the file
/// is in the Recycle Bin). Returns the closest existing ancestor's canonical
/// path joined with the missing tail. Same shape as `canonicalize_safely`
/// in restructure_apply but lives here to avoid a cross-module dependency.
pub(crate) fn canonicalize_for_containment(p: &Path) -> PathBuf {
    if let Ok(c) = std::fs::canonicalize(p) {
        return c;
    }
    let mut cur = p.to_path_buf();
    let mut tail = PathBuf::new();
    while !cur.exists() {
        if let Some(name) = cur.file_name() {
            tail = if tail.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                Path::new(name).join(tail)
            };
        }
        if !cur.pop() {
            break;
        }
    }
    let mut canonical = std::fs::canonicalize(&cur).unwrap_or(cur);
    canonical.push(tail);
    canonical
}

/// Case-insensitive stable hash for a file path. NTFS is case-insensitive,
/// so a re-scan after a path-case change must produce the same hash — else
/// the next ingest creates a duplicate `files` row. `DefaultHasher` (SipHash)
/// keyed off `to_ascii_lowercase` is enough: NTFS uses a Unicode case-folding
/// table that's roughly equivalent for typical paths. A pathological
/// filename with Turkish dotted I would not round-trip exactly, but the
/// resulting hash collision is bounded and tolerable (worst case: one
/// duplicate row that the next scan overwrites via UPSERT).
///
/// macOS volumes default to case-insensitive HFS+/APFS, so the same
/// behavior is correct there — but each platform owns its own DB, so the
/// cross-platform implication is moot. The wire schema stores the resulting
/// i64 as-is.
pub(crate) fn stable_path_hash(path: &str) -> i64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    let normalized = path.to_ascii_lowercase();
    normalized.hash(&mut h);
    h.finish() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_filenames_accepted() {
        assert!(is_safe_filename("photo.jpg"));
        assert!(is_safe_filename("My Vacation Photo (2024).heic"));
        assert!(is_safe_filename("a"));
    }

    #[test]
    fn traversal_rejected() {
        assert!(!is_safe_filename(".."));
        assert!(!is_safe_filename("."));
        assert!(!is_safe_filename("../etc/passwd"));
        assert!(!is_safe_filename("..\\windows\\system32"));
        assert!(!is_safe_filename("a/b"));
        assert!(!is_safe_filename("a\\b"));
        assert!(!is_safe_filename("/abs"));
        assert!(!is_safe_filename("\\abs"));
        assert!(!is_safe_filename("C:\\evil.exe"));
        assert!(!is_safe_filename("\\\\unc\\share\\evil.exe"));
        assert!(!is_safe_filename(""));
        assert!(!is_safe_filename("  "));
        assert!(!is_safe_filename(" leading-space.jpg"));
        assert!(!is_safe_filename("trailing-space.jpg "));
    }

    // Property-based tests proving is_safe_filename and
    // canonicalize_for_containment invariants on randomized inputs.
    proptest::proptest! {
        // Any string containing a forward or back slash must be rejected:
        // is_safe_filename only accepts single Component::Normal names.
        #[test]
        fn any_string_with_slash_is_rejected(s in "[a-zA-Z0-9./\\\\]{1,40}") {
            if s.contains('/') || s.contains('\\') {
                proptest::prop_assert!(!is_safe_filename(&s));
            }
        }

        // Any leading or trailing whitespace must be rejected: Windows
        // resolves "name " vs. "name" to different paths and the trim
        // mismatch is exactly the bait for filesystem-rename ambiguity.
        #[test]
        fn leading_or_trailing_whitespace_rejected(
            inner in "[a-zA-Z0-9_]{1,20}",
            prefix in " {0,3}",
            suffix in " {0,3}",
        ) {
            let s = format!("{prefix}{inner}{suffix}");
            if !prefix.is_empty() || !suffix.is_empty() {
                proptest::prop_assert!(!is_safe_filename(&s));
            } else {
                // Bare alphanumeric/underscore IS safe (no slashes, no reserved name).
                proptest::prop_assert!(is_safe_filename(&s));
            }
        }

        // stable_path_hash must be case-insensitive: NTFS treats
        // C:\Users\Foo and c:\users\foo as the same file. A re-scan after
        // an Explorer rename mustn't create a duplicate row.
        #[test]
        fn stable_path_hash_is_case_insensitive(
            s in "[a-zA-Z0-9_./\\\\]{1,80}",
        ) {
            let lower = s.to_ascii_lowercase();
            let upper = s.to_ascii_uppercase();
            proptest::prop_assert_eq!(stable_path_hash(&lower), stable_path_hash(&upper));
        }

        // stable_path_hash must be deterministic: same input twice in a
        // row must produce the same hash.
        #[test]
        fn stable_path_hash_is_deterministic(s in "[\\PC]{1,200}") {
            proptest::prop_assert_eq!(stable_path_hash(&s), stable_path_hash(&s));
        }

        // Every Windows reserved device name (CON/PRN/AUX/NUL + COM0..9 +
        // LPT0..9) must be rejected with or without an extension. Bare
        // filenames like "COM3" and stems with up to four-letter
        // extensions like "lpt0.txt" must both fail.
        #[test]
        fn reserved_device_names_are_rejected(
            stem in "(CON|PRN|AUX|NUL|COM[0-9]|LPT[0-9])",
            case in 0u8..4,
            ext in proptest::option::of("[a-z]{1,4}"),
        ) {
            let normalized: String = match case {
                0 => stem.to_ascii_lowercase(),
                1 => stem.to_ascii_uppercase(),
                _ => stem.chars().enumerate().map(|(i, c)| if i % 2 == 0 { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() }).collect(),
            };
            let name = if let Some(e) = ext { format!("{normalized}.{e}") } else { normalized };
            proptest::prop_assert!(!is_safe_filename(&name), "reserved name {name} must be rejected");
        }
    }

    // SEC-7: the trash-restore containment check uses `Path::starts_with`
    // on canonicalized PathBufs. UNC paths must containment-match
    // correctly — a restore target of \\srv\share\user\file.jpg must
    // be ACCEPTED if \\srv\share\user is an authorized root, and
    // REJECTED if it isn't. Rust's Path::starts_with treats UNC paths
    // component-wise, which is what we want.
    #[test]
    #[cfg(windows)]
    fn unc_path_containment_starts_with_matches_when_nested() {
        let root = std::path::PathBuf::from(r"\\srv\share\user");
        let inside = std::path::PathBuf::from(r"\\srv\share\user\photos\trip.jpg");
        let outside = std::path::PathBuf::from(r"\\srv\share\other-user\file.jpg");
        let elsewhere = std::path::PathBuf::from(r"C:\Users\u\file.jpg");

        assert!(inside.starts_with(&root), "nested UNC path must be inside root");
        assert!(!outside.starts_with(&root), "different UNC share-leaf must NOT match root");
        assert!(!elsewhere.starts_with(&root), "drive-letter path must NOT match UNC root");
    }

    /// SEC-7 cross-server UNC paths must not collide. `\\srv1\share\x` is
    /// NOT inside `\\srv2\share\x` even though the trailing components
    /// match exactly.
    #[test]
    #[cfg(windows)]
    fn unc_paths_with_different_servers_dont_collide() {
        let root_srv1 = std::path::PathBuf::from(r"\\srv1\share");
        let path_srv2 = std::path::PathBuf::from(r"\\srv2\share\file.jpg");
        assert!(!path_srv2.starts_with(&root_srv1));
    }
}
