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

/// Convert an absolute path to Windows extended-length ("\\?\") form so
/// Win32 file APIs accept it past the 260-char MAX_PATH limit. The engine
/// process has no long-path manifest (the app's `longPathAware` doesn't
/// cover this separate `.exe`) and the system `LongPathsEnabled` registry
/// flag is off by default, so std::fs / jwalk silently fail on deep paths
/// unless we prefix explicitly — a verbatim path bypasses MAX_PATH
/// unconditionally. Stored + displayed paths stay in normal form (see
/// `strip_extended_length`); the prefix is applied only at FS-access sites.
///
/// Only absolute paths convert (a verbatim path must be fully-qualified,
/// backslash-separated, with no `.`/`..`). Relative paths, already-verbatim
/// paths, and non-Windows builds pass through unchanged. Forward slashes are
/// normalized to backslashes because verbatim paths reject `/`.
///   `C:\a\b`            → `\\?\C:\a\b`
///   `\\server\share\x`  → `\\?\UNC\server\share\x`
#[cfg(windows)]
pub(crate) fn to_extended_length(path: &Path) -> PathBuf {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};

    if !path.is_absolute() {
        return path.to_path_buf();
    }
    const BS: u16 = b'\\' as u16;
    const FS: u16 = b'/' as u16;
    let mut wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .map(|c| if c == FS { BS } else { c })
        .collect();

    // Already "\\?\…" (after slash normalization) → leave it.
    if wide.starts_with(&[BS, BS, b'?' as u16, BS]) {
        return PathBuf::from(OsString::from_wide(&wide));
    }
    let out: Vec<u16> = if wide.starts_with(&[BS, BS]) {
        // UNC "\\server\share\…" → "\\?\UNC\server\share\…"
        let mut v: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
        v.extend_from_slice(&wide[2..]);
        v
    } else {
        // Drive "C:\…" → "\\?\C:\…"
        let mut v: Vec<u16> = r"\\?\".encode_utf16().collect();
        v.append(&mut wide);
        v
    };
    PathBuf::from(OsString::from_wide(&out))
}

#[cfg(not(windows))]
pub(crate) fn to_extended_length(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// Inverse of `to_extended_length`: strip a "\\?\" / "\\?\UNC\" prefix so
/// stored + displayed paths stay in normal user-facing form (matching the
/// cross-platform DB + the C# side). Non-prefixed paths pass through.
///   `\\?\C:\a\b`              → `C:\a\b`
///   `\\?\UNC\server\share\x`  → `\\server\share\x`
#[cfg(windows)]
pub(crate) fn strip_extended_length(path: &Path) -> PathBuf {
    let s = path.as_os_str().to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        return PathBuf::from(rest.to_owned());
    }
    path.to_path_buf()
}

#[cfg(not(windows))]
pub(crate) fn strip_extended_length(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// Map an arbitrary string to a filename component safe on Windows NTFS, Linux,
/// and BSD — byte-faithful with macOS `FilesystemNameSafe.componentSafe` so the
/// restructure planner produces IDENTICAL folder names on every platform
/// (otherwise the same library lays out two incompatible trees and learn-your-
/// style folder prototypes never match cross-platform). Unlike the old
/// restructure sanitizer it REPLACES illegal/control chars with `_` (not
/// delete), trims trailing dots/spaces, suffixes Windows reserved basenames,
/// caps length, and never returns empty. (PAR-69 / PAR-96)
pub fn safe_filename_component(raw: &str) -> String {
    const ILLEGAL: &[char] = &['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    const MAX_LEN: usize = 200;
    const RESERVED: &[&str] = &[
        "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5",
        "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
        "lpt6", "lpt7", "lpt8", "lpt9",
    ];
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if (ch as u32) < 32 || ILLEGAL.contains(&ch) {
            out.push('_');
        } else {
            out.push(ch);
        }
    }
    // Cap by Unicode scalar count (matches Swift's unicodeScalars.prefix).
    if out.chars().count() > MAX_LEN {
        out = out.chars().take(MAX_LEN).collect();
    }
    // Windows strips trailing dots/spaces; do it ourselves so the name is stable.
    while matches!(out.chars().last(), Some('.' | ' ')) {
        out.pop();
    }
    if out.is_empty() {
        return "_".to_string();
    }
    let basename = match out.find('.') {
        Some(dot) => out[..dot].to_ascii_lowercase(),
        None => out.to_ascii_lowercase(),
    };
    if RESERVED.contains(&basename.as_str()) {
        out.insert(0, '_');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_safe_matches_macos_rules() {
        // Illegal chars → '_', not deleted (parity with macOS componentSafe).
        assert_eq!(safe_filename_component("Mom: Vacation"), "Mom_ Vacation");
        // Windows reserved basename → '_' prefix.
        assert_eq!(safe_filename_component("CON"), "_CON");
        assert_eq!(safe_filename_component("com1.txt"), "_com1.txt");
        // Trailing dots/spaces stripped; control chars → '_'.
        assert_eq!(safe_filename_component("trip.  "), "trip");
        assert_eq!(safe_filename_component("a\tb"), "a_b");
        // All-illegal collapses to placeholders, never empty.
        assert_eq!(safe_filename_component("///"), "___");
        assert_eq!(safe_filename_component(""), "_");
    }

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

    /// A normal drive path round-trips through the verbatim helpers: prefix
    /// for FS access, strip back to the form we store + display.
    #[test]
    #[cfg(windows)]
    fn extended_length_roundtrip_drive() {
        let p = Path::new(r"C:\Users\me\pic.jpg");
        let ext = to_extended_length(p);
        assert_eq!(ext.as_os_str().to_string_lossy(), r"\\?\C:\Users\me\pic.jpg");
        assert_eq!(strip_extended_length(&ext), p.to_path_buf());
    }

    /// UNC paths use the "\\?\UNC\" verbatim form and round-trip back to the
    /// "\\server\share" form.
    #[test]
    #[cfg(windows)]
    fn extended_length_roundtrip_unc() {
        let p = Path::new(r"\\server\share\dir\file.png");
        let ext = to_extended_length(p);
        assert_eq!(ext.as_os_str().to_string_lossy(), r"\\?\UNC\server\share\dir\file.png");
        assert_eq!(strip_extended_length(&ext), p.to_path_buf());
    }

    /// Already-verbatim paths and relative paths pass through unchanged;
    /// stripping a non-prefixed path is a no-op.
    #[test]
    #[cfg(windows)]
    fn extended_length_idempotent_and_passthrough() {
        let v = Path::new(r"\\?\C:\a\b");
        assert_eq!(to_extended_length(v), v.to_path_buf());
        let rel = Path::new(r"sub\file.jpg");
        assert_eq!(to_extended_length(rel), rel.to_path_buf());
        let plain = Path::new(r"C:\a\b");
        assert_eq!(strip_extended_length(plain), plain.to_path_buf());
    }

    /// IPC callers may hand us forward slashes; the verbatim form must use
    /// backslashes or Win32 rejects it.
    #[test]
    #[cfg(windows)]
    fn extended_length_normalizes_forward_slashes() {
        let p = Path::new("C:/Users/me/pic.jpg");
        assert_eq!(
            to_extended_length(p).as_os_str().to_string_lossy(),
            r"\\?\C:\Users\me\pic.jpg"
        );
    }
}
