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
    // SEC: reject Windows-illegal filename characters. ':' is the dangerous
    // one — "name:stream" addresses an NTFS Alternate Data Stream, so a
    // rename to "photo.jpg:evil" would write a hidden stream that this guard
    // is supposed to block. Also reject < > " | ? * and control chars, which
    // are illegal in NTFS names and produce cryptic MoveFileExW failures.
    if name.contains(|c: char| matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*') || (c as u32) < 0x20)
    {
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

/// Stable hash for a file path, case-folded ONLY where the filesystem is
/// case-insensitive. On NTFS / exfat / default HFS+/APFS a re-scan after a
/// path-case change must produce the same hash — else the next ingest creates a
/// duplicate `files` row — so we key `DefaultHasher` (SipHash) off
/// `to_ascii_lowercase`. NTFS uses a Unicode case-folding table that's roughly
/// equivalent for typical paths; a pathological Turkish-dotted-I name wouldn't
/// round-trip exactly, but the collision is bounded (worst case: one duplicate
/// row the next scan overwrites via UPSERT).
///
/// Linux's default filesystems (ext4 / btrfs / xfs / zfs) are case-SENSITIVE,
/// so `Foo.jpg` and `foo.jpg` are genuinely distinct files. Lowercasing there
/// would hash them to the same `path_hash` (the dedup/lookup key), letting one
/// shadow or overwrite the other on UPSERT — silent data loss. So on Linux we
/// hash the path as-is. (A case-insensitive volume mounted on Linux, e.g. the
/// exfat test drive, can't hold two case-distinct names anyway, so preserving
/// case is harmless there.) Each platform owns its own DB, so this per-OS
/// difference has no cross-platform implication. The wire schema stores the
/// resulting i64 as-is.
pub(crate) fn stable_path_hash(path: &str) -> i64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    if cfg!(target_os = "linux") {
        path.hash(&mut h);
    } else {
        path.to_ascii_lowercase().hash(&mut h);
    }
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

/// Canonical comparison form for user folder-exclusion matching: strip any
/// "\\?\" verbatim prefix, unify '/'→'\', trim trailing separators, and
/// lowercase (NTFS is case-insensitive). On non-Windows only trailing '/'
/// are trimmed — Linux filesystems are case-sensitive and '\' is a legal
/// filename character there, so folding either would corrupt paths.
pub(crate) fn normalize_for_exclusion(p: &Path) -> String {
    let stripped = strip_extended_length(p);
    let s = stripped.as_os_str().to_string_lossy();
    if cfg!(windows) {
        let unified: String = s.chars().map(|c| if c == '/' { '\\' } else { c }).collect();
        unified.trim_end_matches('\\').to_lowercase()
    } else {
        let t = s.trim_end_matches('/');
        if t.is_empty() { "/".to_string() } else { t.to_string() }
    }
}

/// A user exclusion validated against a scan root. `original` keeps the
/// caller's casing (verbatim-stripped, separators unified, trailing
/// separators trimmed) for BINARY-collating `path_text` range scans;
/// `normalized` is the case-folded form the walker compares against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedExclusion {
    pub original: String,
    pub normalized: String,
}

/// Resolve a single absolute path with no root-containment check — used by
/// the `purgeExcluded` command, where any absolute folder is a valid purge
/// target. Returns None for relative paths.
pub(crate) fn resolve_exclusion_unrooted(p: &Path) -> Option<ResolvedExclusion> {
    if !p.is_absolute() {
        return None;
    }
    let normalized = normalize_for_exclusion(p);
    let stripped = strip_extended_length(p);
    let s = stripped.as_os_str().to_string_lossy();
    let original = if cfg!(windows) {
        let unified: String = s.chars().map(|c| if c == '/' { '\\' } else { c }).collect();
        unified.trim_end_matches('\\').to_string()
    } else {
        s.trim_end_matches('/').to_string()
    };
    Some(ResolvedExclusion { original, normalized })
}

/// Filter + normalize raw exclusion strings against a scan root: drops
/// relative paths, paths not strictly under the root, duplicates, and the
/// root itself (excluding the root would exclude everything — warn instead).
pub(crate) fn resolve_exclusions(root: &Path, raw: &[String]) -> Vec<ResolvedExclusion> {
    let norm_root = normalize_for_exclusion(root);
    let sep = if cfg!(windows) { '\\' } else { '/' };
    let mut child_prefix = norm_root.clone();
    if !child_prefix.ends_with(sep) {
        child_prefix.push(sep);
    }
    let mut out: Vec<ResolvedExclusion> = Vec::new();
    for r in raw {
        let Some(res) = resolve_exclusion_unrooted(Path::new(r)) else {
            continue;
        };
        if res.normalized == norm_root {
            tracing::warn!("exclusion equal to the scan root ignored");
            continue;
        }
        if !res.normalized.starts_with(&child_prefix) {
            continue;
        }
        if out.iter().any(|e| e.normalized == res.normalized) {
            continue;
        }
        out.push(res);
    }
    out
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
        "con", "prn", "aux", "nul", "com0", "com1", "com2", "com3", "com4", "com5",
        "com6", "com7", "com8", "com9", "lpt0", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5",
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

#[cfg(windows)]
#[allow(dead_code)]
pub fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVE_FILE_FLAGS};

    let source = to_extended_length(source);
    let destination = to_extended_length(destination);
    let source_wide: Vec<u16> = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        MoveFileExW(
            PCWSTR(source_wide.as_ptr()),
            PCWSTR(destination_wide.as_ptr()),
            MOVE_FILE_FLAGS(0),
        )
        .map_err(std::io::Error::other)
    }
}

#[cfg(unix)]
#[allow(dead_code)]
fn c_path(path: &Path, label: &str) -> std::io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;

    std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{label} path contains NUL"),
        )
    })
}

#[cfg(target_os = "linux")]
#[allow(dead_code)]
pub fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = c_path(source, "source")?;
    let destination = c_path(destination, "destination")?;
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub fn rename_no_replace(source: &Path, destination: &Path) -> std::io::Result<()> {
    let source = c_path(source, "source")?;
    let destination = c_path(destination, "destination")?;
    let result = unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
#[allow(dead_code)]
pub fn rename_no_replace(_source: &Path, _destination: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace rename is unavailable on this platform",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn normalize_for_exclusion_windows_forms() {
        let n = |s: &str| normalize_for_exclusion(Path::new(s));
        assert_eq!(n(r"C:\Pics\Raw"), r"c:\pics\raw");
        assert_eq!(n(r"C:\Pics\Raw\"), r"c:\pics\raw");
        assert_eq!(n(r"C:/Pics/Raw"), r"c:\pics\raw");
        assert_eq!(n(r"\\?\C:\Pics\Raw"), r"c:\pics\raw");
        assert_eq!(n(r"\\?\UNC\srv\share\Raw"), r"\\srv\share\raw");
    }

    #[test]
    #[cfg(not(windows))]
    fn normalize_for_exclusion_unix_forms() {
        let n = |s: &str| normalize_for_exclusion(Path::new(s));
        assert_eq!(n("/pics/Raw/"), "/pics/Raw"); // case preserved
        assert_eq!(n("/pics/with\\backslash"), "/pics/with\\backslash");
        assert_eq!(n("/"), "/");
    }

    #[test]
    fn resolve_exclusions_containment() {
        let (root, inside, inside_dup, outside) = if cfg!(windows) {
            (r"C:\Pics", r"C:\Pics\Raw\", r"c:\pics\RAW", r"D:\Other")
        } else {
            ("/pics", "/pics/raw/", "/pics/RAW", "/other")
        };
        let raw = vec![
            inside.to_string(),
            inside_dup.to_string(),
            outside.to_string(),
            root.to_string(),        // root-equal → dropped
            "relative/x".to_string(), // relative → dropped
        ];
        let resolved = resolve_exclusions(Path::new(root), &raw);
        // On Unix the "dup" differs by case and is a genuinely distinct dir.
        let expected = if cfg!(windows) { 1 } else { 2 };
        assert_eq!(resolved.len(), expected);
        assert_eq!(
            resolved[0].original,
            if cfg!(windows) { r"C:\Pics\Raw" } else { "/pics/raw" }
        );
        // Prefix-boundary: excluding \Pics must not match \PicsBackup.
        let sibling = if cfg!(windows) { r"C:\PicsBackup" } else { "/picsBackup" };
        assert!(resolve_exclusions(Path::new(root), &[sibling.to_string()]).is_empty());
    }

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
    fn no_replace_rename_preserves_an_occupied_destination() {
        let root = std::env::temp_dir().join(format!(
            "fileid-no-replace-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source");
        let destination = root.join("destination");
        std::fs::write(&source, b"source").unwrap();
        std::fs::write(&destination, b"destination").unwrap();
        assert!(rename_no_replace(&source, &destination).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"source");
        assert_eq!(std::fs::read(&destination).unwrap(), b"destination");
        std::fs::remove_file(&destination).unwrap();
        rename_no_replace(&source, &destination).unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"source");
        let _ = std::fs::remove_dir_all(root);
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
        // NTFS Alternate Data Stream + other Windows-illegal characters.
        assert!(!is_safe_filename("photo.jpg:evil"));
        assert!(!is_safe_filename("a:b"));
        assert!(!is_safe_filename("a<b"));
        assert!(!is_safe_filename("a>b"));
        assert!(!is_safe_filename("a|b"));
        assert!(!is_safe_filename("a?b"));
        assert!(!is_safe_filename("a*b"));
        assert!(!is_safe_filename("a\"b"));
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

        // Case folding follows the filesystem. On case-INSENSITIVE volumes
        // (NTFS/exfat/default APFS) C:\Users\Foo and c:\users\foo are the same
        // file, so a re-scan after an Explorer rename mustn't create a duplicate
        // row → equal hashes. On case-SENSITIVE Linux fs they're distinct files,
        // so case-distinct paths must hash differently (else one shadows the
        // other on UPSERT) while identical strings still collide.
        #[test]
        fn stable_path_hash_case_sensitivity(
            s in "[a-zA-Z0-9_./\\\\]{1,80}",
        ) {
            let lower = s.to_ascii_lowercase();
            let upper = s.to_ascii_uppercase();
            if cfg!(target_os = "linux") {
                proptest::prop_assert_eq!(
                    stable_path_hash(&lower) == stable_path_hash(&upper),
                    lower == upper
                );
            } else {
                proptest::prop_assert_eq!(stable_path_hash(&lower), stable_path_hash(&upper));
            }
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

    /// Cross-platform pin: the macOS engine re-implements this function in
    /// Swift (FileIDShared/StablePathHash.swift, mirrored vectors in
    /// StablePathHashTests) so `files.path_hash` is identical in both
    /// engines' DBs. If DefaultHasher's algorithm ever changes, this fails
    /// before the platforms silently drift apart.
    ///
    /// Linux is intentionally excluded: its case-SENSITIVE filesystems make
    /// `stable_path_hash` preserve case (see the fn doc), so these lowercased
    /// macOS/Windows-parity vectors don't apply there. Linux's contract is the
    /// `stable_path_hash_case_sensitivity` property test above. (DBs are never
    /// shared across OSes, so the divergence is invisible in practice.)
    #[test]
    #[cfg(not(target_os = "linux"))]
    fn stable_path_hash_pinned_vectors() {
        assert_eq!(stable_path_hash(""), 3_476_900_567_878_811_119);
        assert_eq!(stable_path_hash("a"), 8_186_225_505_942_432_243);
        assert_eq!(
            stable_path_hash("/Users/adam/Photos/IMG_0001.JPG"),
            -6_847_549_264_798_039_763
        );
        assert_eq!(
            stable_path_hash("C:\\Users\\Adam\\Pictures\\Photo.JPG"),
            -5_418_614_373_936_508_534
        );
        assert_eq!(
            stable_path_hash("/Users/ådam/Désktop/Café.jpg"),
            6_025_210_603_525_090_388
        );
        assert_eq!(
            stable_path_hash("/Users/adam/Photos/家族写真.jpg"),
            -1_257_796_233_084_950_905
        );
        assert_eq!(
            stable_path_hash(
                "/Users/adam/Library/Mobile Documents/com~apple~CloudDocs/Tax 2024 (final).pdf"
            ),
            1_387_562_067_336_403_736
        );
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
