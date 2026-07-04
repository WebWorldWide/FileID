// Windows shell + system integrations. Each Win32 submodule is a thin
// RAII wrapper over a Win32 / WinRT API:
//
//   reveal     → SHOpenFolderAndSelectItems
//   trash      → IFileOperation::DeleteItem (8-parallel from Cleanup tab)
//   thumbnail  → IThumbnailProvider
//   ocr        → Windows.Media.Ocr (WinRT)
//   tags       → IPropertyStore System.Keywords
//   video      → Media Foundation IMFSourceReader
//
// Sleep-prevention (SetThreadExecutionState) lives in `crate::platform`
// because it's cross-cutting, not shell-specific.
//
// On non-Windows targets each module is replaced by a same-surface fallback.
// Linux (`cfg(target_os = "linux")`) carries real, dependency-free backends
// built on std + libc + subprocess:
//
//   reveal  → org.freedesktop.FileManager1.ShowItems (dbus-send/gdbus),
//             falling back to `xdg-open` on the parent dir
//   trash   → freedesktop Trash spec (move to $XDG_DATA_HOME/Trash + .trashinfo)
//   tags    → user.xdg.tags xattr via libc {set,get,list,remove}xattr
//   ocr     → tesseract CLI (best-effort; empty when absent)
//   video   → ffmpeg keyframe → P6 PPM we parse (best-effort)
//   heic    → libheif tools (heif-dec/heif-convert) → temp PNG → `image`
//             decode (best-effort; needs the HEVC decoder plugin
//             `libheif-plugin-libde265`, else the file is cleanly skipped)
//
// macOS / other Unix keep a graceful stub
// (`cfg(all(not(windows), not(target_os = "linux")))`) so the macOS build
// still compiles. `thumbnail` has no non-Windows caller (each app thumbnails
// itself), so it stays a stub on every non-Windows OS.

#[cfg(windows)] pub mod reveal;
#[cfg(windows)] pub mod tags;
#[cfg(windows)] pub mod thumbnail;
#[cfg(windows)] pub mod trash;
#[cfg(windows)] pub mod ocr;
#[cfg(windows)] pub mod video;
#[cfg(windows)] pub mod heic;

// ────────────────────────────────────────────────────────────────────
// Linux shared helpers (path → URI, temp files, silent subprocess).
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
mod linux_util {
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};

    /// Absolute form of `path` without resolving symlinks (canonicalize would).
    pub fn absolute(path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .map(|d| d.join(path))
                .unwrap_or_else(|_| path.to_path_buf())
        }
    }

    /// RFC 3986 percent-encoding, leaving `/` and the unreserved set raw —
    /// the shape both `file://` URIs and `.trashinfo` `Path=` expect.
    pub fn percent_encode_path(path: &Path) -> String {
        let bytes: &[u8] = path.as_os_str().as_bytes();
        let mut out = String::with_capacity(bytes.len());
        for &b in bytes {
            match b {
                b'/' | b'-' | b'_' | b'.' | b'~'
                | b'0'..=b'9'
                | b'A'..=b'Z'
                | b'a'..=b'z' => out.push(b as char),
                _ => {
                    const HEX: &[u8; 16] = b"0123456789ABCDEF";
                    out.push('%');
                    out.push(HEX[(b >> 4) as usize] as char);
                    out.push(HEX[(b & 0x0f) as usize] as char);
                }
            }
        }
        out
    }

    /// Inverse of `percent_encode_path`: decode `%XX` byte escapes back to a
    /// path. Works on raw bytes so non-UTF-8 names round-trip; a malformed
    /// escape is passed through literally. Used to read the original location
    /// out of a `.trashinfo` `Path=` field on restore.
    pub fn percent_decode_path(s: &str) -> PathBuf {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bytes = s.as_bytes();
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                let hi = (bytes[i + 1] as char).to_digit(16);
                let lo = (bytes[i + 2] as char).to_digit(16);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push((hi * 16 + lo) as u8);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        PathBuf::from(OsString::from_vec(out))
    }

    /// A unique temp path under the system temp dir (pid + nanos + counter).
    pub fn temp_file(ext: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("fileid-{}-{nanos}-{n}.{ext}", std::process::id()))
    }

    /// Run a command with all stdio discarded; true iff it exited 0. A missing
    /// binary (ENOENT) is a clean `false`, never an error.
    pub fn run_silent(cmd: &mut Command) -> bool {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

// ────────────────────────────────────────────────────────────────────
// reveal
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub mod reveal {
    use super::linux_util::{absolute, percent_encode_path, run_silent};
    use anyhow::Result;
    use std::path::Path;
    use std::process::Command;

    /// Open the containing folder and select the file. Prefers the file-manager
    /// DBus interface (`org.freedesktop.FileManager1.ShowItems`, which selects
    /// the item); falls back to opening the parent dir with `xdg-open`.
    #[allow(dead_code)]
    pub fn reveal(path: &Path) -> Result<()> {
        let abs = absolute(path);
        let uri = format!("file://{}", percent_encode_path(&abs));

        // 1. ShowItems via dbus-send.
        if run_silent(
            Command::new("dbus-send")
                .arg("--session")
                .arg("--dest=org.freedesktop.FileManager1")
                .arg("--type=method_call")
                .arg("/org/freedesktop/FileManager1")
                .arg("org.freedesktop.FileManager1.ShowItems")
                .arg(format!("array:string:{uri}"))
                .arg("string:"),
        ) {
            return Ok(());
        }

        // 2. Same call via gdbus (GLib stack without dbus-send installed).
        //    The URI is percent-encoded, so it can't contain the `'` we wrap it
        //    in for the GVariant array literal.
        if run_silent(
            Command::new("gdbus")
                .arg("call")
                .arg("--session")
                .arg("--dest")
                .arg("org.freedesktop.FileManager1")
                .arg("--object-path")
                .arg("/org/freedesktop/FileManager1")
                .arg("--method")
                .arg("org.freedesktop.FileManager1.ShowItems")
                .arg(format!("['{uri}']"))
                .arg(""),
        ) {
            return Ok(());
        }

        // 3. Fallback: open the parent directory (no selection).
        let parent = abs.parent().unwrap_or_else(|| Path::new("/"));
        if run_silent(Command::new("xdg-open").arg(parent)) {
            return Ok(());
        }

        anyhow::bail!("reveal: dbus-send, gdbus, and xdg-open all unavailable or failed")
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub mod reveal {
    use anyhow::Result;
    use std::path::Path;
    #[allow(dead_code)]
    pub fn reveal(_path: &Path) -> Result<()> {
        anyhow::bail!("shell::reveal::reveal not implemented on this platform")
    }
}

// ────────────────────────────────────────────────────────────────────
// tags  (user.xdg.tags xattr)
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub mod tags {
    use anyhow::{Context, Result};
    use std::ffi::{CStr, CString};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    /// XDG-standard tag attribute, comma-separated UTF-8 (Nautilus/Tracker
    /// convention).
    const TAGS_ATTR: &CStr = c"user.xdg.tags";

    fn cpath(path: &Path) -> Result<CString> {
        CString::new(path.as_os_str().as_bytes())
            .with_context(|| format!("path {} contains an interior NUL", path.display()))
    }

    /// Replace the file's tag list. Empty `tags` removes the attribute.
    pub fn write_tags(path: &Path, tags: &[String]) -> Result<()> {
        let c = cpath(path)?;
        if tags.is_empty() {
            // Best-effort clear; a missing attribute (ENODATA) is success.
            unsafe { libc::removexattr(c.as_ptr(), TAGS_ATTR.as_ptr()) };
            return Ok(());
        }
        let value = tags.join(",");
        let rc = unsafe {
            libc::setxattr(
                c.as_ptr(),
                TAGS_ATTR.as_ptr(),
                value.as_ptr() as *const libc::c_void,
                value.len(),
                0,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error()).context("setxattr(user.xdg.tags)");
        }
        Ok(())
    }

    /// Read the file's tag list. Absent attribute (or a filesystem without
    /// `user.*` xattr support) yields an empty list, never an error.
    #[allow(dead_code)]
    pub fn read_tags(path: &Path) -> Result<Vec<String>> {
        let c = cpath(path)?;
        if !xattr_present(&c, TAGS_ATTR) {
            return Ok(Vec::new());
        }
        let size = unsafe { libc::getxattr(c.as_ptr(), TAGS_ATTR.as_ptr(), std::ptr::null_mut(), 0) };
        if size <= 0 {
            return Ok(Vec::new());
        }
        let mut buf = vec![0u8; size as usize];
        let got = unsafe {
            libc::getxattr(
                c.as_ptr(),
                TAGS_ATTR.as_ptr(),
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
            )
        };
        if got <= 0 {
            return Ok(Vec::new());
        }
        buf.truncate(got as usize);
        let joined = String::from_utf8_lossy(&buf);
        let tags = joined
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect();
        Ok(tags)
    }

    /// Tags live in the file's `user.xdg.tags` xattr, which `rename(2)` carries
    /// with the inode — so a move needs no sidecar fix-up. No-op, present only
    /// to mirror the Windows sidecar API.
    pub fn move_sidecar(_old: &Path, _new: &Path) {}

    /// True iff `want` appears in the file's xattr name list (NUL-separated).
    fn xattr_present(path: &CStr, want: &CStr) -> bool {
        let len = unsafe { libc::listxattr(path.as_ptr(), std::ptr::null_mut(), 0) };
        if len <= 0 {
            return false;
        }
        let mut buf = vec![0u8; len as usize];
        let got =
            unsafe { libc::listxattr(path.as_ptr(), buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if got <= 0 {
            return false;
        }
        buf.truncate(got as usize);
        let want = want.to_bytes();
        buf.split(|&b| b == 0).any(|name| name == want)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        // Use a dir under `target/` (ext4 on CI) since tmpfs historically
        // rejected `user.*` xattrs; skip gracefully if the fs still does.
        fn scratch_dir(tag: &str) -> std::path::PathBuf {
            let dir = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!("fileid-tags-test-{}-{tag}", std::process::id()));
            let _ = std::fs::create_dir_all(&dir);
            dir
        }

        #[test]
        fn write_then_read_round_trip() {
            let dir = scratch_dir("rt");
            let file = dir.join("tagme.txt");
            std::fs::write(&file, b"hi").unwrap();

            if write_tags(&file, &["holiday".into(), "2024".into()]).is_err() {
                let _ = std::fs::remove_dir_all(&dir);
                return; // filesystem without user-xattr support — skip.
            }
            let tags = read_tags(&file).unwrap();
            if tags.is_empty() {
                let _ = std::fs::remove_dir_all(&dir);
                return; // fs silently dropped the attr — skip.
            }
            assert!(tags.contains(&"holiday".to_string()));
            assert!(tags.contains(&"2024".to_string()));

            // Clearing removes the attribute.
            write_tags(&file, &[]).unwrap();
            assert!(read_tags(&file).unwrap().is_empty());

            let _ = std::fs::remove_dir_all(&dir);
        }

        #[test]
        fn read_missing_returns_empty() {
            let dir = scratch_dir("missing");
            let file = dir.join("untagged.txt");
            std::fs::write(&file, b"hi").unwrap();
            assert!(read_tags(&file).unwrap().is_empty());
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub mod tags {
    use anyhow::Result;
    use std::path::Path;
    pub fn write_tags(_path: &Path, _tags: &[String]) -> Result<()> {
        anyhow::bail!("shell::tags::write_tags not implemented on this platform")
    }
    #[allow(dead_code)]
    pub fn read_tags(_path: &Path) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    pub fn move_sidecar(_old: &Path, _new: &Path) {}
}

// ────────────────────────────────────────────────────────────────────
// thumbnail  (stubbed on every non-Windows OS — TODO(linux): gdk-pixbuf)
// ────────────────────────────────────────────────────────────────────
#[cfg(not(windows))]
pub mod thumbnail {
    use anyhow::Result;
    use std::path::Path;
    #[allow(dead_code)]
    pub const THUMB_DIM: i32 = 512;
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct Thumbnail {
        pub width: u32,
        pub height: u32,
        pub rgba: Vec<u8>,
    }
    #[allow(dead_code)]
    pub fn render(_path: &Path) -> Result<Thumbnail> {
        anyhow::bail!("shell::thumbnail::render not implemented on this platform")
    }
    #[allow(dead_code)]
    pub fn render_at(_path: &Path, _dim: i32) -> Result<Thumbnail> {
        anyhow::bail!("shell::thumbnail::render_at not implemented on this platform")
    }
}

// ────────────────────────────────────────────────────────────────────
// trash  (freedesktop Trash spec)
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub mod trash {
    use super::linux_util::{absolute, percent_decode_path, percent_encode_path};
    use anyhow::{Context, Result};
    use std::ffi::{OsStr, OsString};
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// Batch wrapper. Trashes each path; returns one bool per input, true =
    /// success. Order is preserved. Filesystem moves are cheap, so this runs
    /// sequentially (no worker pool, unlike the COM-apartment Windows path).
    pub fn trash(paths: &[PathBuf]) -> Vec<bool> {
        let trash = match home_trash_dir() {
            Ok(t) => t,
            Err(_) => return vec![false; paths.len()],
        };
        paths.iter().map(|p| trash_into(p, &trash).is_ok()).collect()
    }

    /// Move a single file to the home trash. Idempotent: a missing source is
    /// treated as success ("already not on disk").
    #[allow(dead_code)]
    pub fn trash_path(path: &Path) -> Result<()> {
        let trash = home_trash_dir()?;
        trash_into(path, &trash)
    }

    /// Restore files previously trashed by [`trash`]/[`trash_path`] back to
    /// their original locations — the Linux parity of the Windows Recycle-Bin
    /// batch restore (the Cleanup-tab undo). Best-effort: scans the home trash's
    /// `info/*.trashinfo`, and for any whose recorded original `Path` matches a
    /// requested path, moves the file back from `files/` (recreating the parent
    /// if it was since removed) and deletes the `.trashinfo`. Never clobbers — a
    /// path now occupied by something else is left alone. A requested path with
    /// no matching trash entry is silently skipped (the caller verifies on-disk
    /// presence, exactly like the Windows path).
    pub fn restore(wanted: &[&Path]) {
        let Ok(trash) = home_trash_dir() else {
            return;
        };
        restore_from(wanted, &trash);
    }

    /// Inner restore against an explicit trash dir (so it's unit-testable
    /// without mutating the process's `$XDG_DATA_HOME`). Mirrors how
    /// `trash_into` is the testable core of `trash`.
    fn restore_from(wanted: &[&Path], trash: &Path) {
        use std::collections::HashSet;
        if wanted.is_empty() {
            return;
        }
        let info_dir = trash.join("info");
        let files_dir = trash.join("files");
        let mut remaining: HashSet<PathBuf> = wanted.iter().map(|p| p.to_path_buf()).collect();

        let Ok(entries) = std::fs::read_dir(&info_dir) else {
            return;
        };
        for entry in entries.flatten() {
            if remaining.is_empty() {
                break;
            }
            let info_path = entry.path();
            if info_path.extension().and_then(|e| e.to_str()) != Some("trashinfo") {
                continue;
            }
            let Ok(contents) = std::fs::read_to_string(&info_path) else {
                continue;
            };
            let Some(orig) = parse_trashinfo_orig(&contents) else {
                continue;
            };
            if !remaining.contains(&orig) {
                continue;
            }
            // info name is "<files-entry-name>.trashinfo" → the trashed file's
            // name is the stem.
            let Some(stem) = info_path.file_stem() else {
                continue;
            };
            let src = files_dir.join(stem);
            if let Some(parent) = orig.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            // Don't overwrite whatever now sits at the original path.
            if orig.symlink_metadata().is_ok() {
                continue;
            }
            if move_into(&src, &orig).is_ok() {
                let _ = std::fs::remove_file(&info_path);
                remaining.remove(&orig);
            }
        }
    }

    /// Pull the original location out of a `.trashinfo` body's `Path=` line
    /// (percent-decoded). Returns None if there's no `Path=` key.
    fn parse_trashinfo_orig(contents: &str) -> Option<PathBuf> {
        contents
            .lines()
            .find_map(|l| l.strip_prefix("Path="))
            .map(|v| percent_decode_path(v.trim()))
    }

    fn home_trash_dir() -> Result<PathBuf> {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            let p = PathBuf::from(xdg);
            if p.is_absolute() {
                return Ok(p.join("Trash"));
            }
        }
        let home = std::env::var_os("HOME").context("neither XDG_DATA_HOME nor HOME is set")?;
        Ok(PathBuf::from(home).join(".local/share/Trash"))
    }

    fn trash_into(path: &Path, trash: &Path) -> Result<()> {
        if std::fs::symlink_metadata(path).is_err() {
            return Ok(());
        }
        let files_dir = trash.join("files");
        let info_dir = trash.join("info");
        std::fs::create_dir_all(&files_dir)
            .with_context(|| format!("create {}", files_dir.display()))?;
        std::fs::create_dir_all(&info_dir)
            .with_context(|| format!("create {}", info_dir.display()))?;

        let orig_name = path.file_name().context("path has no file name")?;
        let abs = absolute(path);

        let mut n = 0u32;
        loop {
            if n > 100_000 {
                anyhow::bail!("trash: exhausted collision-free names for {}", path.display());
            }
            let candidate = candidate_name(orig_name, n);
            let mut info_name = candidate.clone();
            info_name.push(".trashinfo");
            let info_path = info_dir.join(&info_name);
            let target = files_dir.join(&candidate);

            if target.exists() {
                n += 1;
                continue;
            }
            // Atomically claim the name by exclusively creating its .trashinfo.
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&info_path)
            {
                Ok(mut f) => {
                    let body = format!(
                        "[Trash Info]\nPath={}\nDeletionDate={}\n",
                        percent_encode_path(&abs),
                        deletion_date_now()
                    );
                    f.write_all(body.as_bytes())
                        .with_context(|| format!("write {}", info_path.display()))?;
                    drop(f);
                    return match move_into(path, &target) {
                        Ok(()) => Ok(()),
                        Err(e) => {
                            let _ = std::fs::remove_file(&info_path);
                            Err(e)
                        }
                    };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    n += 1;
                    continue;
                }
                Err(e) => return Err(e).with_context(|| format!("create {}", info_path.display())),
            }
        }
    }

    /// `name`, `name.1`, `name.2`, … keeping any extension on the tail so the
    /// trashed file stays recognizable.
    fn candidate_name(orig: &OsStr, n: u32) -> OsString {
        if n == 0 {
            return orig.to_os_string();
        }
        let p = Path::new(orig);
        let stem = p.file_stem().unwrap_or(orig);
        let ext = p.extension();
        let mut out = OsString::new();
        out.push(stem);
        out.push(format!(".{n}"));
        if let Some(e) = ext {
            out.push(".");
            out.push(e);
        }
        out
    }

    fn move_into(src: &Path, dst: &Path) -> Result<()> {
        match std::fs::rename(src, dst) {
            Ok(()) => Ok(()),
            // Cross-filesystem move (e.g. NAS mount → home disk): copy + unlink.
            Err(e) if e.raw_os_error() == Some(libc::EXDEV) => {
                std::fs::copy(src, dst)
                    .with_context(|| format!("copy {} across filesystems into trash", src.display()))?;
                std::fs::remove_file(src)
                    .with_context(|| format!("remove original {} after copy", src.display()))?;
                Ok(())
            }
            Err(e) => Err(e).with_context(|| format!("move {} to trash", src.display())),
        }
    }

    /// Local-time ISO-8601 (no offset), per the freedesktop trash spec. Uses
    /// libc's `localtime_r` so we don't hand-roll calendar/timezone math.
    fn deletion_date_now() -> String {
        unsafe {
            let t = libc::time(std::ptr::null_mut());
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&t, &mut tm).is_null() {
                return String::new();
            }
            format!(
                "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
                tm.tm_year + 1900,
                tm.tm_mon + 1,
                tm.tm_mday,
                tm.tm_hour,
                tm.tm_min,
                tm.tm_sec
            )
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn trashes_file_and_writes_info() {
            let base = std::env::temp_dir().join(format!("fileid-trash-test-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let trash = base.join("Trash");
            let src_dir = base.join("src");
            std::fs::create_dir_all(&src_dir).unwrap();
            let file = src_dir.join("hello.txt");
            std::fs::write(&file, b"data").unwrap();

            trash_into(&file, &trash).unwrap();

            assert!(!file.exists(), "original should be gone");
            assert!(trash.join("files/hello.txt").exists(), "file should be in trash/files");
            let info = std::fs::read_to_string(trash.join("info/hello.txt.trashinfo")).unwrap();
            assert!(info.contains("[Trash Info]"));
            assert!(info.contains("Path="));
            assert!(info.contains("DeletionDate="));

            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        fn handles_name_collision() {
            let base = std::env::temp_dir().join(format!("fileid-trash-coll-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let trash = base.join("Trash");
            let src_dir = base.join("src");
            std::fs::create_dir_all(&src_dir).unwrap();

            for _ in 0..2 {
                let file = src_dir.join("dup.txt");
                std::fs::write(&file, b"x").unwrap();
                trash_into(&file, &trash).unwrap();
            }

            assert!(trash.join("files/dup.txt").exists());
            assert!(trash.join("files/dup.1.txt").exists(), "collision should append .1");

            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        fn missing_source_is_success() {
            let base = std::env::temp_dir().join(format!("fileid-trash-missing-{}", std::process::id()));
            let trash = base.join("Trash");
            let ghost = base.join("does-not-exist.txt");
            assert!(trash_into(&ghost, &trash).is_ok());
            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        fn trash_then_restore_round_trip() {
            let base =
                std::env::temp_dir().join(format!("fileid-trash-restore-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let trash = base.join("Trash");
            let src_dir = base.join("src");
            std::fs::create_dir_all(&src_dir).unwrap();
            let file = src_dir.join("restoreme.txt");
            std::fs::write(&file, b"payload").unwrap();

            trash_into(&file, &trash).unwrap();
            assert!(!file.exists(), "original should be gone after trash");
            assert!(trash.join("files/restoreme.txt").exists());

            restore_from(&[file.as_path()], &trash);

            assert!(file.exists(), "file should be back at its original path");
            assert_eq!(std::fs::read(&file).unwrap(), b"payload");
            assert!(
                !trash.join("info/restoreme.txt.trashinfo").exists(),
                "the .trashinfo should be cleaned up after a successful restore"
            );
            assert!(
                !trash.join("files/restoreme.txt").exists(),
                "the trashed copy should be gone after restore"
            );

            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        fn restore_does_not_clobber_occupant() {
            let base = std::env::temp_dir()
                .join(format!("fileid-trash-restore-noclobber-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let trash = base.join("Trash");
            let src_dir = base.join("src");
            std::fs::create_dir_all(&src_dir).unwrap();
            let file = src_dir.join("keep.txt");
            std::fs::write(&file, b"old").unwrap();
            trash_into(&file, &trash).unwrap();

            // Something new now occupies the original path.
            std::fs::write(&file, b"new").unwrap();
            restore_from(&[file.as_path()], &trash);

            assert_eq!(
                std::fs::read(&file).unwrap(),
                b"new",
                "restore must not overwrite a file that now occupies the original path"
            );
            let _ = std::fs::remove_dir_all(&base);
        }
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub mod trash {
    use std::path::{Path, PathBuf};
    /// Fallback stub: returns all-false so the caller logs failure cleanly
    /// rather than silently claiming a successful trash.
    pub fn trash(paths: &[PathBuf]) -> Vec<bool> {
        vec![false; paths.len()]
    }
    #[allow(dead_code)]
    pub fn trash_path(_path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("shell::trash::trash_path not implemented on this platform")
    }
}

// ────────────────────────────────────────────────────────────────────
// ocr  (tesseract CLI, best-effort)
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub mod ocr {
    use super::linux_util::temp_file;
    use anyhow::Result;
    use std::io::Write;
    use std::path::Path;
    use std::process::{Command, Stdio};

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct OcrLine {
        pub text: String,
    }

    #[allow(dead_code)]
    pub struct OcrResult {
        pub text: String,
        pub lines: Vec<OcrLine>,
        pub locale: Option<String>,
    }

    fn empty() -> OcrResult {
        OcrResult { text: String::new(), lines: Vec::new(), locale: None }
    }

    /// Best-effort OCR via the `tesseract` CLI. The buffer is tightly-packed
    /// RGB8 (3 bytes/pixel), matching the Windows `recognize`. Writes a P6 PPM
    /// to a temp file, runs `tesseract <ppm> stdout`, returns the text. Never
    /// fails: a missing or erroring tesseract yields an empty result.
    #[allow(dead_code)]
    pub fn recognize(rgb: &[u8], width: u32, height: u32) -> Result<OcrResult> {
        const MAX_DIM: u32 = 16384;
        if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
            return Ok(empty());
        }
        let need = (width as usize) * (height as usize) * 3;
        if rgb.len() < need {
            return Ok(empty());
        }

        let img = temp_file("ppm");
        if write_ppm(&rgb[..need], width, height, &img).is_err() {
            let _ = std::fs::remove_file(&img);
            return Ok(empty());
        }

        let output = Command::new("tesseract")
            .arg(&img)
            .arg("stdout")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output();
        let _ = std::fs::remove_file(&img);

        let text = match output {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
            _ => return Ok(empty()),
        };

        let lines = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|l| OcrLine { text: l.to_string() })
            .collect();
        Ok(OcrResult { text: text.trim().to_string(), lines, locale: None })
    }

    fn write_ppm(rgb: &[u8], width: u32, height: u32, path: &Path) -> std::io::Result<()> {
        let mut f = std::fs::File::create(path)?;
        write!(f, "P6\n{width} {height}\n255\n")?;
        f.write_all(rgb)?;
        Ok(())
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub mod ocr {
    use anyhow::Result;
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct OcrLine { pub text: String }
    #[allow(dead_code)]
    pub struct OcrResult {
        pub text: String,
        pub lines: Vec<OcrLine>,
        pub locale: Option<String>,
    }
    #[allow(dead_code)]
    pub fn recognize(_rgba: &[u8], _width: u32, _height: u32) -> Result<OcrResult> {
        anyhow::bail!("shell::ocr::recognize not implemented on this platform")
    }
}

// ────────────────────────────────────────────────────────────────────
// video  (ffmpeg keyframe, best-effort)
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub mod video {
    use super::linux_util::temp_file;
    use anyhow::{Context, Result};
    use std::path::Path;
    use std::process::{Command, Stdio};

    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct VideoFrame {
        pub width: u32,
        pub height: u32,
        /// Tightly packed RGB8.
        pub rgb: Vec<u8>,
        pub time_seconds: f64,
    }

    /// Best-effort keyframe at ~25% of duration via the `ffmpeg` CLI. ffmpeg
    /// writes a self-describing P6 PPM (RGB) that we parse directly — no image
    /// decoder needed. Returns Err (gracefully, never a panic) when ffmpeg is
    /// absent or no frame can be extracted, matching the prior stub contract
    /// the callers already tolerate.
    pub fn keyframe_25pct(path: &Path) -> Result<VideoFrame> {
        let seconds = probe_duration(path).map(|d| (d * 0.25).max(0.0)).unwrap_or(0.0);
        let out = temp_file("ppm");

        let status = Command::new("ffmpeg")
            .arg("-nostdin")
            .arg("-loglevel")
            .arg("error")
            .arg("-ss")
            .arg(format!("{seconds:.3}"))
            .arg("-i")
            .arg(path)
            .arg("-frames:v")
            .arg("1")
            .arg("-f")
            .arg("image2")
            .arg("-vcodec")
            .arg("ppm")
            .arg("-y")
            .arg(&out)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();

        let read = match status {
            Ok(s) if s.success() => std::fs::read(&out).ok(),
            _ => None,
        };
        let _ = std::fs::remove_file(&out);

        let bytes = read.context("ffmpeg unavailable or produced no keyframe")?;
        let (width, height, rgb) =
            parse_ppm(&bytes).context("parse PPM keyframe emitted by ffmpeg")?;
        Ok(VideoFrame { width, height, rgb, time_seconds: seconds })
    }

    fn probe_duration(path: &Path) -> Option<f64> {
        let output = Command::new("ffprobe")
            .arg("-v")
            .arg("quiet")
            .arg("-show_entries")
            .arg("format=duration")
            .arg("-of")
            .arg("csv=p=0")
            .arg(path)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|d| d.is_finite() && *d > 0.0)
    }

    /// Parse a binary (P6) PPM: `P6 <w> <h> <maxval>` then one whitespace and
    /// raw RGB. Comments (`#…`) are skipped. Only 8-bit (maxval 255) is handled.
    fn parse_ppm(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        if bytes.len() < 2 || &bytes[0..2] != b"P6" {
            return None;
        }
        let mut pos = 2usize;
        let w = next_uint(bytes, &mut pos)?;
        let h = next_uint(bytes, &mut pos)?;
        let maxval = next_uint(bytes, &mut pos)?;
        if maxval != 255 {
            return None;
        }
        // Exactly one whitespace byte separates the header from the raster.
        pos += 1;
        let need = (w as usize).checked_mul(h as usize)?.checked_mul(3)?;
        if pos.checked_add(need)? > bytes.len() {
            return None;
        }
        Some((w as u32, h as u32, bytes[pos..pos + need].to_vec()))
    }

    fn next_uint(b: &[u8], pos: &mut usize) -> Option<u64> {
        loop {
            while *pos < b.len() && b[*pos].is_ascii_whitespace() {
                *pos += 1;
            }
            if *pos < b.len() && b[*pos] == b'#' {
                while *pos < b.len() && b[*pos] != b'\n' {
                    *pos += 1;
                }
                continue;
            }
            break;
        }
        let start = *pos;
        while *pos < b.len() && b[*pos].is_ascii_digit() {
            *pos += 1;
        }
        if *pos == start {
            return None;
        }
        std::str::from_utf8(&b[start..*pos]).ok()?.parse::<u64>().ok()
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub mod video {
    use anyhow::Result;
    use std::path::Path;
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct VideoFrame {
        pub width: u32,
        pub height: u32,
        /// Tightly packed RGB8.
        pub rgb: Vec<u8>,
        pub time_seconds: f64,
    }
    pub fn keyframe_25pct(_path: &Path) -> Result<VideoFrame> {
        anyhow::bail!("shell::video::keyframe_25pct not implemented on this platform")
    }
}

// ────────────────────────────────────────────────────────────────────
// heic  (stubbed on every non-Windows OS — TODO(linux): libheif)
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub mod heic {
    use super::linux_util::{run_silent, temp_file};
    use anyhow::{Context, Result};
    use std::path::{Path, PathBuf};
    use std::process::Command;

    /// Best-effort HEIC/HEIF decode on Linux via the libheif command-line tools
    /// (`heif-dec`, falling back to the older `heif-convert`). Converts to a temp
    /// PNG we then decode with the already-bundled `image` crate → RGB8 +
    /// dimensions. Returns Err (never panics) when the tools are absent or the
    /// conversion fails, so the caller cleanly skips the file — matching the
    /// prior stub contract. No new dependency and no GPL `libheif` linked in: the
    /// tools are an optional system package (`libheif-examples`), honoring the
    /// download-and-run / no-GPL-dep rule.
    pub fn decode(path: &Path) -> Result<(Vec<u8>, u32, u32)> {
        let out = temp_file("png");
        let mut produced: Option<PathBuf> = None;
        for tool in ["heif-dec", "heif-convert"] {
            if run_silent(Command::new(tool).arg(path).arg(&out)) {
                if let Some(p) = resolve_output(&out) {
                    produced = Some(p);
                    break;
                }
            }
        }
        let Some(png) = produced else {
            let _ = std::fs::remove_file(&out);
            anyhow::bail!("heif-dec/heif-convert unavailable or produced no output");
        };

        let bytes = std::fs::read(&png);
        let _ = std::fs::remove_file(&png);
        if png != out {
            let _ = std::fs::remove_file(&out);
        }
        let bytes = bytes.context("read converted heic png")?;
        let dyn_img = image::load_from_memory(&bytes).context("decode converted heic png")?;
        let rgb = dyn_img.to_rgb8();
        let (w, h) = rgb.dimensions();
        Ok((rgb.into_raw(), w, h))
    }

    /// `heif-convert` writes the requested name for a single-image file but
    /// suffixes multi-image files (`out.png` → `out-1.png` for the primary). Try
    /// the exact name first, then the `-1` variant.
    fn resolve_output(out: &Path) -> Option<PathBuf> {
        if out.exists() {
            return Some(out.to_path_buf());
        }
        let stem = out.file_stem()?.to_str()?;
        let alt = out.with_file_name(format!("{stem}-1.png"));
        alt.exists().then_some(alt)
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub mod heic {
    use anyhow::Result;
    use std::path::Path;
    #[allow(dead_code)]
    pub fn decode(_path: &Path) -> Result<(Vec<u8>, u32, u32)> {
        anyhow::bail!("shell::heic::decode not implemented on this platform")
    }
}
