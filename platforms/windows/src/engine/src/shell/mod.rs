// Windows shell + system integrations. Each Win32 submodule is a thin
// RAII wrapper over a Win32 / WinRT API:
//
//   trash      → IFileOperation::DeleteItem (8-parallel from Cleanup tab)
//   ocr        → Windows.Media.Ocr (WinRT)
//   tags       → IPropertyStore System.Keywords
//   video      → Media Foundation IMFSourceReader
//   heic       → WIC HEIF decode
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
// macOS (`cfg(all(not(windows), not(target_os = "linux")))`, where the CLI
// and TUI run this engine) uses system tools where they always exist:
//
//   reveal  → `open -R`
//   heic    → `sips` (ImageIO) → temp PNG → `image` decode
//   video   → FileID's vetted FFmpeg runtime, as on Linux
//   ocr     → unavailable: empty text, one notice per process
//   trash / tags → refuse with a message naming the macOS app; the Rust
//             engine has no Finder Trash or Finder-tag writer

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
    use std::process::{Child, Command, Stdio};

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

    fn read_with_limit(
        mut reader: impl std::io::Read,
        max_bytes: usize,
        initial_capacity: usize,
    ) -> std::io::Result<Vec<u8>> {
        let limit = max_bytes.checked_add(1).ok_or_else(|| {
            std::io::Error::other("bounded read limit overflow")
        })?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(initial_capacity).map_err(|_| {
            std::io::Error::other("bounded read allocation failed")
        })?;
        let mut chunk = [0u8; 64 * 1024];
        while bytes.len() < limit {
            let read_len = chunk.len().min(limit - bytes.len());
            let n = reader.read(&mut chunk[..read_len])?;
            if n == 0 {
                return Ok(bytes);
            }
            bytes.try_reserve_exact(n).map_err(|_| {
                std::io::Error::other("bounded read allocation failed")
            })?;
            bytes.extend_from_slice(&chunk[..n]);
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stream exceeds bounded read limit",
        ))
    }

    pub fn read_stream_bounded(
        reader: impl std::io::Read,
        max_bytes: usize,
    ) -> std::io::Result<Vec<u8>> {
        read_with_limit(reader, max_bytes, 0)
    }

    pub fn read_bounded(path: &Path, max_bytes: usize) -> std::io::Result<Vec<u8>> {
        let file = std::fs::File::open(path)?;
        let initial = file
            .metadata()
            .ok()
            .and_then(|m| usize::try_from(m.len()).ok())
            .unwrap_or(0);
        if initial > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "file exceeds bounded read limit",
            ));
        }
        read_with_limit(file, max_bytes, initial)
    }

    pub fn terminate_process_group(child: &mut Child) {
        crate::platform::terminate_child_tree(child);
    }

    pub fn run_output_bounded(
        cmd: &mut Command,
        max_stdout_bytes: usize,
        timeout: std::time::Duration,
    ) -> std::io::Result<Vec<u8>> {
        crate::platform::configure_child_lifetime(cmd);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd.spawn()?;
        let Some(stdout) = child.stdout.take() else {
            terminate_process_group(&mut child);
            return Err(std::io::Error::other("child stdout unavailable"));
        };
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let reader = std::thread::spawn(move || {
            let _ = tx.send(read_stream_bounded(stdout, max_stdout_bytes));
        });
        let deadline = std::time::Instant::now() + timeout;
        let mut output = None;
        let mut status = None;

        loop {
            if output.is_none() {
                match rx.try_recv() {
                    Ok(result) => output = Some(result),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        terminate_process_group(&mut child);
                        let _ = reader.join();
                        return Err(std::io::Error::other("child output reader stopped"));
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
            }
            if output.as_ref().is_some_and(Result::is_err) {
                terminate_process_group(&mut child);
                let _ = reader.join();
                if let Some(output) = output {
                    return output;
                }
                return Err(std::io::Error::other("child output result disappeared"));
            }
            if status.is_none() {
                match child.try_wait() {
                    Ok(current) => status = current,
                    Err(error) => {
                        terminate_process_group(&mut child);
                        let _ = reader.join();
                        return Err(error);
                    }
                }
            }
            if let (Some(output), Some(status)) = (output.take(), status) {
                let _ = reader.join();
                if !status.success() {
                    return Err(std::io::Error::other(format!(
                        "child exited with status {status}"
                    )));
                }
                return output;
            }
            if std::time::Instant::now() >= deadline {
                terminate_process_group(&mut child);
                let _ = reader.join();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "child process timed out",
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Run a command with discarded output and a finite deadline. A missing or
    /// failed binary is a clean `false`.
    pub fn run_silent(cmd: &mut Command) -> bool {
        crate::platform::configure_child_lifetime(cmd);
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let Ok(mut child) = cmd.spawn() else {
            return false;
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return status.success(),
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Ok(None) | Err(_) => {
                    terminate_process_group(&mut child);
                    return false;
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::{read_bounded, read_stream_bounded, run_output_bounded, run_silent, temp_file};
        use std::io::Write;
        use std::process::Command;
        use std::time::{Duration, Instant};

        #[test]
        fn bounded_reader_rejects_cap_plus_one_without_reading_past_it() {
            let path = temp_file("bounded-read-test");
            std::fs::write(&path, [1, 2, 3, 4, 5]).expect("write test file");
            assert_eq!(read_bounded(&path, 5).expect("bounded read"), [1, 2, 3, 4, 5]);
            assert!(read_bounded(&path, 4).is_err());
            assert!(read_stream_bounded(std::io::Cursor::new([1, 2, 3, 4, 5]), 4).is_err());
            let _ = std::fs::remove_file(path);
        }

        #[test]
        fn child_output_is_bounded_and_silent_children_time_out() {
            let output = run_output_bounded(
                Command::new("sh").args(["-c", "printf 1234"]),
                4,
                Duration::from_secs(2),
            )
            .expect("bounded child output");
            assert_eq!(output, b"1234");
            assert!(run_output_bounded(
                Command::new("sh").args(["-c", "printf 12345"]),
                4,
                Duration::from_secs(2),
            )
            .is_err());

            let started = Instant::now();
            let error = run_output_bounded(
                Command::new("sh").args(["-c", "sleep 30"]),
                4,
                Duration::from_millis(100),
            )
            .expect_err("silent child must time out");
            assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
            assert!(started.elapsed() < Duration::from_secs(5));
        }

        #[test]
        fn silent_child_may_write_output() {
            assert!(run_silent(Command::new("sh").args(["-c", "printf output; printf error >&2"])));
        }

        #[test]
        fn process_group_termination_kills_helper_descendants() {
            use std::io::BufRead as _;

            let mut command = Command::new("sh");
            command
                .args(["-c", "sleep 30 & echo $!; wait"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null());
            crate::platform::configure_child_lifetime(&mut command);
            let mut child = command.spawn().expect("spawn process tree");
            let mut line = String::new();
            std::io::BufReader::new(child.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            let descendant = line.trim().parse::<i32>().expect("descendant pid");
            crate::platform::terminate_child_tree(&mut child);

            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let result = unsafe { libc::kill(descendant, 0) };
                if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    break;
                }
                assert!(Instant::now() < deadline, "helper descendant survived group termination");
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        #[test]
        fn pdeath_signal_kills_helper_when_engine_parent_exits() {
            const HELPER_ENV: &str = "FILEID_TEST_PDEATH_HELPER";
            if std::env::var_os(HELPER_ENV).is_some() {
                let mut command = Command::new("sleep");
                command.arg("30");
                crate::platform::configure_child_lifetime(&mut command);
                let child = command.spawn().expect("spawn lifetime-bound child");
                let pid = child.id();
                std::mem::forget(child);
                println!("FILEID_PDEATH_PID={pid}");
                std::io::stdout().flush().unwrap();
                return;
            }

            let output = Command::new(std::env::current_exe().unwrap())
                .arg("pdeath_signal_kills_helper_when_engine_parent_exits")
                .arg("--nocapture")
                .arg("--test-threads=1")
                .env(HELPER_ENV, "1")
                .output()
                .expect("spawn isolated helper parent");
            assert!(output.status.success());
            let stdout = String::from_utf8_lossy(&output.stdout);
            let pid = stdout
                .split("FILEID_PDEATH_PID=")
                .nth(1)
                .and_then(|value| value.split_whitespace().next())
                .and_then(|value| value.parse::<i32>().ok())
                .expect("isolated parent must report helper pid");
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                let result = unsafe { libc::kill(pid, 0) };
                if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    break;
                }
                assert!(Instant::now() < deadline, "helper survived its engine parent");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
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
    use anyhow::{Context, Result};
    use std::path::Path;
    use std::process::{Command, Stdio};

    /// Select the file in Finder.
    pub fn reveal(path: &Path) -> Result<()> {
        anyhow::ensure!(path.is_absolute(), "reveal needs an absolute path");
        let status = Command::new("/usr/bin/open")
            .arg("-R")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("start Finder reveal")?;
        anyhow::ensure!(status.success(), "Finder could not reveal the file");
        Ok(())
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
        crate::util::read_only::require_source_mutation(path)?;
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
    pub const UNSUPPORTED: &str = "Writing tags into files is not supported by the FileID \
        engine on macOS; tags stay in the FileID catalog (the FileID macOS app writes Finder tags)";
    pub fn write_tags(_path: &Path, _tags: &[String]) -> Result<()> {
        anyhow::bail!(UNSUPPORTED)
    }
    pub fn move_sidecar(_old: &Path, _new: &Path) {}

    #[cfg(test)]
    mod tests {
        #[test]
        fn write_tags_refuses_with_a_user_facing_reason() {
            let error = super::write_tags(std::path::Path::new("/tmp/x.jpg"), &["a".into()])
                .unwrap_err();
            assert_eq!(error.to_string(), super::UNSUPPORTED);
        }
    }
}

/// Image-rs thumbnails for the Linux app; the WinUI app renders its own.
#[cfg(not(windows))]
pub mod thumbnail {
    use anyhow::{Context, Result};
    use std::path::Path;

    #[derive(Debug, Clone)]
    pub struct Thumbnail {
        pub width: u32,
        pub height: u32,
        pub rgba: Vec<u8>,
    }

    /// Upright thumbnail, like macOS ImageIO with the EXIF transform applied:
    /// camera RAWs use their embedded preview and container orientation, other
    /// images honour their EXIF orientation, oversized JPEGs DCT-scale toward
    /// `dim`, and HEIC/HEIF falls back to libheif on Linux. Full decodes are
    /// capped at the scan's decoded-pixel limit, so file size needs no cap.
    pub fn render_oriented(path: &Path, dim: i32) -> Result<Thumbnail> {
        let dim = u32::try_from(dim).ok().filter(|dim| *dim > 0)
            .context("thumbnail dimension must be positive")?;
        let image = decode_upright(path, dim)?.thumbnail(dim, dim).into_rgba8();
        Ok(Thumbnail {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }

    fn decode_upright(path: &Path, dim: u32) -> Result<image::DynamicImage> {
        use crate::pipeline::{raw_preview, tagging};
        let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
        let file = tagging::open_image_file(path).context("opening thumbnail source")?;
        // SAFETY: read-only map of a file we only read; a concurrent truncation
        // can fault, the same trade-off the scan decoder makes.
        let bytes = unsafe { memmap2::Mmap::map(&file) }.context("mapping thumbnail source")?;
        if raw_preview::is_raw_extension(&ext) {
            if let Some((rgb, w, h)) = tagging::decode_raw_preview_at_least(path, Some(&bytes), 1, dim) {
                return rgb_image(rgb, w, h);
            }
        }
        match decode_with_orientation(&bytes, dim) {
            Ok(image) => Ok(image),
            Err(err) if ext == "heic" || ext == "heif" => {
                let (rgb, w, h) = super::heic::decode(path).map_err(|_| err)?;
                rgb_image(rgb, w, h)
            }
            Err(err) => Err(err),
        }
    }

    fn decode_with_orientation(bytes: &[u8], dim: u32) -> Result<image::DynamicImage> {
        use crate::pipeline::tagging;
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<image::DynamicImage> {
            let reader = || {
                image::ImageReader::new(std::io::Cursor::new(bytes))
                    .with_guessed_format()
                    .context("detecting image format")
            };
            let orientation = tagging::exif_orientation(bytes);
            let decoder = reader()?;
            let scaled = (decoder.format() == Some(image::ImageFormat::Jpeg))
                .then(|| tagging::decode_jpeg_scaled_to(bytes, dim))
                .flatten();
            let mut image = match scaled {
                Some((rgb, w, h)) => rgb_image(rgb, w, h)?,
                None => {
                    let (w, h) = reader()?.into_dimensions().context("reading thumbnail dimensions")?;
                    anyhow::ensure!(
                        u64::from(w) * u64::from(h) <= tagging::MAX_DECODED_PIXELS,
                        "image too large to thumbnail"
                    );
                    decoder.decode().context("decoding thumbnail source")?
                }
            };
            if let Some(orientation) = orientation {
                image.apply_orientation(orientation);
            }
            Ok(image)
        }))
        .unwrap_or_else(|_| Err(anyhow::anyhow!("thumbnail decoder panicked")))
    }

    fn rgb_image(rgb: Vec<u8>, width: u32, height: u32) -> Result<image::DynamicImage> {
        image::RgbImage::from_raw(width, height, rgb)
            .map(image::DynamicImage::ImageRgb8)
            .context("decoded buffer does not match its dimensions")
    }

    /// Scale a decoded RGB frame (a video keyframe) to fit `dim`.
    pub fn from_rgb(width: u32, height: u32, rgb: Vec<u8>, dim: i32) -> Result<Thumbnail> {
        let dim = u32::try_from(dim).ok().filter(|dim| *dim > 0)
            .context("thumbnail dimension must be positive")?;
        let image = image::RgbImage::from_raw(width, height, rgb)
            .context("frame buffer does not match its dimensions")?;
        let image = image::DynamicImage::ImageRgb8(image).thumbnail(dim, dim).into_rgba8();
        Ok(Thumbnail {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn jpeg_with_orientation(width: u32, height: u32, orientation: Option<u8>) -> Vec<u8> {
            let img = image::RgbImage::from_pixel(width, height, image::Rgb([90, 140, 200]));
            let mut encoded = std::io::Cursor::new(Vec::new());
            img.write_to(&mut encoded, image::ImageFormat::Jpeg).unwrap();
            let encoded = encoded.into_inner();
            let Some(value) = orientation else { return encoded };
            let mut out = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x22];
            out.extend_from_slice(b"Exif\0\0");
            out.extend_from_slice(&[0x49, 0x49, 0x2A, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00]);
            out.extend_from_slice(&[0x12, 0x01, 0x03, 0x00, 0x01, 0x00, 0x00, 0x00, value, 0x00, 0x00, 0x00]);
            out.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
            out.extend_from_slice(&encoded[2..]);
            out
        }

        /// TIFF-style RAW: IFD0 with orientation 6 and a JPEG preview.
        fn fake_raw(preview: &[u8]) -> Vec<u8> {
            let mut b = b"II\x2A\x00\x08\x00\x00\x00".to_vec();
            let data_at = 8 + 2 + 3 * 12 + 4;
            b.extend_from_slice(&3u16.to_le_bytes());
            for (tag, kind, value) in [(0x0112u16, 3u16, 6u32), (0x0201, 4, data_at), (0x0202, 4, preview.len() as u32)] {
                b.extend_from_slice(&tag.to_le_bytes());
                b.extend_from_slice(&kind.to_le_bytes());
                b.extend_from_slice(&1u32.to_le_bytes());
                b.extend_from_slice(&value.to_le_bytes());
            }
            b.extend_from_slice(&0u32.to_le_bytes());
            b.extend_from_slice(preview);
            b
        }

        #[test]
        fn linux_thumbnail_scales_generated_image_and_rejects_invalid_size() {
            let fixture = std::env::temp_dir().join(format!(
                "fileid-thumbnail-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            std::fs::create_dir_all(&fixture).unwrap();
            let path = fixture.join("generated.png");
            image::RgbaImage::from_pixel(40, 20, image::Rgba([200, 40, 60, 255]))
                .save(&path).unwrap();
            let thumb = render_oriented(&path, 8).unwrap();
            assert_eq!((thumb.width, thumb.height), (8, 4));
            let rotated = fixture.join("portrait.jpg");
            std::fs::write(&rotated, jpeg_with_orientation(40, 20, Some(6))).unwrap();
            let upright = render_oriented(&rotated, 8).unwrap();
            assert_eq!((upright.width, upright.height), (4, 8), "EXIF orientation 6 renders upright");
            let raw = fixture.join("camera.ARW");
            std::fs::write(&raw, fake_raw(&jpeg_with_orientation(60, 45, None))).unwrap();
            let preview = render_oriented(&raw, 8).unwrap();
            assert_eq!((preview.width, preview.height), (6, 8), "RAW preview with its container orientation");
            assert_eq!(thumb.rgba.len(), 8 * 4 * 4);
            assert_eq!(&thumb.rgba[0..4], &[200, 40, 60, 255]);
            assert!(render_oriented(&path, 0).is_err());
            std::fs::remove_dir_all(fixture).unwrap();
        }

        #[test]
        fn oversized_jpeg_scales_toward_the_thumbnail_and_stays_upright() {
            let fixture = std::env::temp_dir().join(format!("fileid-thumbnail-scaled-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&fixture).unwrap();
            let path = fixture.join("large.jpg");
            let encoded = jpeg_with_orientation(2048, 1024, Some(8));
            std::fs::write(&path, &encoded).unwrap();
            let (rgb, w, h) = crate::pipeline::tagging::decode_jpeg_scaled_to(&encoded, 256).expect("DCT-scaled");
            assert_eq!((w, h), (256, 128), "1/8 scale still covers the 256 px request");
            assert_eq!(rgb.len(), 256 * 128 * 3);
            let thumb = render_oriented(&path, 256).unwrap();
            assert_eq!((thumb.width, thumb.height), (128, 256), "orientation 8 turns the frame upright");
            std::fs::remove_dir_all(fixture).unwrap();
        }

        #[test]
        fn video_frames_scale_to_fit_and_reject_short_buffers() {
            let thumb = from_rgb(64, 32, vec![10; 64 * 32 * 3], 16).unwrap();
            assert_eq!((thumb.width, thumb.height), (16, 8));
            assert!(from_rgb(64, 32, vec![10; 10], 16).is_err());
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// trash  (freedesktop Trash spec)
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub mod trash {
    use super::linux_util::{absolute, percent_decode_path, percent_encode_path};
    use anyhow::{Context, Result};
    use std::ffi::{CString, OsStr, OsString};
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::path::{Component, Path, PathBuf};

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
    pub fn trash_path(path: &Path) -> Result<()> {
        let trash = home_trash_dir()?;
        trash_into(path, &trash)
    }

    pub(crate) fn trash_path_as(
        source: &Path,
        original_path: &Path,
        expected: crate::platform::FileIdentity,
    ) -> Result<()> {
        let trash = home_trash_dir()?;
        trash_into_as(source, original_path, &trash, expected)
    }

    #[derive(Debug)]
    pub(crate) enum RestoreOutcome {
        Restored(crate::platform::FileIdentity),
        Conflict,
        Failed(String),
    }

    pub(crate) struct RestoreTarget {
        original: PathBuf,
        parent: std::fs::File,
        leaf: CString,
    }

    impl RestoreTarget {
        pub(crate) fn prepare(original: &Path, authorized_roots: &[PathBuf]) -> Result<Self> {
            let destination = crate::util::path_safety::canonicalize_for_containment(original);
            let root = authorized_roots
                .iter()
                .filter(|root| destination.starts_with(root))
                .max_by_key(|root| root.components().count())
                .context("destination is outside every authorized library root")?;
            let parent_path = destination.parent().context("restore destination has no parent")?;
            let leaf = destination
                .file_name()
                .context("restore destination has no filename")?;
            let leaf = CString::new(leaf.as_bytes()).context("restore filename contains NUL")?;
            let root_handle = open_absolute_directory(root)?;
            let relative_parent = parent_path
                .strip_prefix(root)
                .context("restore parent is outside the authorized root")?;
            let parent = open_relative_directory(&root_handle, relative_parent)?;
            Ok(Self {
                original: original.to_path_buf(),
                parent,
                leaf,
            })
        }

        pub(crate) fn original(&self) -> &Path {
            &self.original
        }

        pub(crate) fn restore_claim(
            &self,
            claim: &Path,
            expected: Option<crate::platform::FileIdentity>,
        ) -> Result<crate::platform::FileIdentity> {
            let claim_parent = claim.parent().context("claim has no parent")?;
            if claim_parent != self.original.parent().context("destination has no parent")? {
                anyhow::bail!("claim is not a sibling of the restore destination");
            }
            let claim_leaf = claim.file_name().context("claim has no filename")?;
            let claim_leaf = CString::new(claim_leaf.as_bytes()).context("claim contains NUL")?;
            let expected = expected.context("Trash journal has no source identity")?;
            let actual = identity_at(self.parent.as_raw_fd(), &claim_leaf)?;
            if actual != expected {
                anyhow::bail!("restored claim identity does not match the Trash journal");
            }
            renameat2_no_replace(
                self.parent.as_raw_fd(),
                &claim_leaf,
                self.parent.as_raw_fd(),
                &self.leaf,
            )
            .context("restore claimed file")?;
            if let Err(sync_error) = self.parent.sync_all() {
                if renameat2_no_replace(
                    self.parent.as_raw_fd(),
                    &self.leaf,
                    self.parent.as_raw_fd(),
                    &claim_leaf,
                )
                .is_ok()
                {
                    let _ = self.parent.sync_all();
                    return Err(sync_error).context("sync restored destination parent");
                }
                if identity_at(self.parent.as_raw_fd(), &self.leaf).ok() == Some(expected) {
                    tracing::warn!(?sync_error, "restore committed but its parent sync failed");
                    return Ok(expected);
                }
                return Err(sync_error).context("sync restored destination parent");
            }
            identity_at(self.parent.as_raw_fd(), &self.leaf)
        }

        fn restore_external(
            &self,
            source: &Path,
            expected: Option<crate::platform::FileIdentity>,
        ) -> Result<crate::platform::FileIdentity> {
            let expected = expected.context("Trash journal has no source identity")?;
            let source_parent_path = source.parent().context("Trash source has no parent")?;
            let source_leaf = source.file_name().context("Trash source has no filename")?;
            let source_parent = std::fs::File::open(source_parent_path)
                .context("open Trash source parent")?;
            let source_leaf =
                CString::new(source_leaf.as_bytes()).context("Trash source contains NUL")?;
            let mut quarantine_name = b".fileid-restore-finalize-".to_vec();
            quarantine_name.extend_from_slice(source_leaf.as_bytes());
            let quarantine_leaf = CString::new(quarantine_name)?;
            let rollback_source = || {
                let _ = renameat2_no_replace(
                    source_parent.as_raw_fd(),
                    &quarantine_leaf,
                    source_parent.as_raw_fd(),
                    &source_leaf,
                );
                let _ = source_parent.sync_all();
            };
            match identity_at(source_parent.as_raw_fd(), &quarantine_leaf) {
                Ok(actual) if actual == expected => {}
                Ok(_) => anyhow::bail!("restore quarantine is occupied by another Trash item"),
                Err(error)
                    if error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
                {
                    if identity_at(source_parent.as_raw_fd(), &source_leaf)? != expected {
                        anyhow::bail!("Trash source identity does not match the recovery journal");
                    }
                    renameat2_no_replace(
                        source_parent.as_raw_fd(),
                        &source_leaf,
                        source_parent.as_raw_fd(),
                        &quarantine_leaf,
                    )
                    .context("claim Trash source for restore")?;
                    if let Err(error) = source_parent.sync_all() {
                        rollback_source();
                        return Err(error).context("sync claimed Trash source");
                    }
                }
                Err(error) => return Err(error).context("inspect restore quarantine"),
            }
            let actual = match identity_at(source_parent.as_raw_fd(), &quarantine_leaf) {
                Ok(actual) => actual,
                Err(error) => {
                    rollback_source();
                    return Err(error).context("revalidate claimed Trash source");
                }
            };
            if actual != expected {
                rollback_source();
                anyhow::bail!("Trash source identity does not match the recovery journal");
            }

            match renameat2_no_replace(
                source_parent.as_raw_fd(),
                &quarantine_leaf,
                self.parent.as_raw_fd(),
                &self.leaf,
            ) {
                Ok(()) => {
                    let sync_result = self
                        .parent
                        .sync_all()
                        .and_then(|()| source_parent.sync_all());
                    if let Err(sync_error) = sync_result {
                        if renameat2_no_replace(
                            self.parent.as_raw_fd(),
                            &self.leaf,
                            source_parent.as_raw_fd(),
                            &quarantine_leaf,
                        )
                        .is_ok()
                        {
                            let _ = self.parent.sync_all();
                            let _ = source_parent.sync_all();
                            return Err(sync_error).context("sync restored Trash item");
                        }
                        if identity_at(self.parent.as_raw_fd(), &self.leaf).ok() == Some(expected) {
                            tracing::warn!(?sync_error, "restore committed but a directory sync failed");
                            return Ok(expected);
                        }
                        return Err(sync_error).context("sync restored Trash item");
                    }
                    identity_at(self.parent.as_raw_fd(), &self.leaf)
                }
                Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                    match self.copy_claimed_external(&source_parent, &quarantine_leaf, expected) {
                        Ok(identity) => Ok(identity),
                        Err(error) => {
                            rollback_source();
                            Err(error)
                        }
                    }
                }
                Err(error) => {
                    rollback_source();
                    Err(error).context("restore claimed Trash item")
                }
            }
        }

        fn copy_claimed_external(
            &self,
            source_parent: &std::fs::File,
            source_leaf: &CString,
            expected: crate::platform::FileIdentity,
        ) -> Result<crate::platform::FileIdentity> {
            use std::os::unix::fs::MetadataExt;

            let input_fd = unsafe {
                libc::openat(
                    source_parent.as_raw_fd(),
                    source_leaf.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            if input_fd < 0 {
                return Err(std::io::Error::last_os_error()).context("open claimed Trash source");
            }
            let mut input = unsafe { std::fs::File::from_raw_fd(input_fd) };
            let before = input.metadata()?;
            if !before.is_file()
                || before.dev() != expected.volume
                || before.ino() != expected.file
            {
                anyhow::bail!("claimed Trash source identity changed before restore copy");
            }
            let output_fd = unsafe {
                libc::openat(
                    self.parent.as_raw_fd(),
                    self.leaf.as_ptr(),
                    libc::O_WRONLY
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    before.mode(),
                )
            };
            if output_fd < 0 {
                return Err(std::io::Error::last_os_error()).context("create restore destination");
            }
            let mut output = unsafe { std::fs::File::from_raw_fd(output_fd) };
            let rollback_destination = || {
                unsafe {
                    libc::unlinkat(self.parent.as_raw_fd(), self.leaf.as_ptr(), 0);
                }
                let _ = self.parent.sync_all();
            };
            if let Err(error) = std::io::copy(&mut input, &mut output) {
                rollback_destination();
                return Err(error).context("copy Trash item to restore destination");
            }
            if let Err(error) = output.sync_all() {
                rollback_destination();
                return Err(error).context("sync restored file");
            }
            if let Err(error) = self.parent.sync_all() {
                rollback_destination();
                return Err(error).context("durably commit restored file");
            }
            if unsafe { libc::unlinkat(source_parent.as_raw_fd(), source_leaf.as_ptr(), 0) } != 0 {
                let error = std::io::Error::last_os_error();
                rollback_destination();
                return Err(error).context("remove claimed Trash source after restore copy");
            }
            if let Err(error) = source_parent.sync_all() {
                tracing::warn!(?error, "restore copy committed but the Trash parent sync failed");
            }
            identity_at(self.parent.as_raw_fd(), &self.leaf)
        }
    }

    fn open_absolute_directory(path: &Path) -> Result<std::fs::File> {
        if !path.is_absolute() {
            anyhow::bail!("authorized root is not absolute");
        }
        let mut current = std::fs::File::open("/").context("open filesystem root")?;
        for component in path.components() {
            match component {
                Component::RootDir => {}
                Component::Normal(name) => current = open_directory_at(&current, name)?,
                _ => anyhow::bail!("authorized root contains an unsafe component"),
            }
        }
        Ok(current)
    }

    fn open_relative_directory(base: &std::fs::File, path: &Path) -> Result<std::fs::File> {
        let mut current = base.try_clone().context("clone authorized root handle")?;
        for component in path.components() {
            match component {
                Component::Normal(name) => current = open_directory_at(&current, name)?,
                _ => anyhow::bail!("restore parent contains an unsafe component"),
            }
        }
        Ok(current)
    }

    fn open_directory_at(parent: &std::fs::File, name: &OsStr) -> Result<std::fs::File> {
        let name = CString::new(name.as_bytes()).context("directory name contains NUL")?;
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).context("open restore parent without links");
        }
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }

    fn renameat2_no_replace(
        source_dir: i32,
        source: &CString,
        destination_dir: i32,
        destination: &CString,
    ) -> std::io::Result<()> {
        let result = unsafe {
            libc::renameat2(
                source_dir,
                source.as_ptr(),
                destination_dir,
                destination.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        // NFS and some FUSE filesystems reject the flag (EINVAL); kernels
        // before 3.15 have no renameat2 (ENOSYS).
        if matches!(error.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
            return renameat_no_replace_emulated(source_dir, source, destination_dir, destination);
        }
        Err(error)
    }

    fn stat_at(parent: i32, leaf: &CString) -> std::io::Result<libc::stat> {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let result =
            unsafe { libc::fstatat(parent, leaf.as_ptr(), &mut stat, libc::AT_SYMLINK_NOFOLLOW) };
        if result == 0 {
            Ok(stat)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Directory-relative twin of `util::path_safety::rename_no_replace_emulated`:
    /// claim the destination with an exclusive create, then rename over that
    /// empty placeholder only.
    fn renameat_no_replace_emulated(
        source_dir: i32,
        source: &CString,
        destination_dir: i32,
        destination: &CString,
    ) -> std::io::Result<()> {
        let occupied = || std::io::Error::from_raw_os_error(libc::EEXIST);
        let source_stat = stat_at(source_dir, source)?;
        match stat_at(destination_dir, destination) {
            Ok(existing) => {
                let same_entry = existing.st_dev == source_stat.st_dev
                    && existing.st_ino == source_stat.st_ino
                    && source_stat.st_nlink == 1;
                if !same_entry {
                    return Err(occupied());
                }
                let result = unsafe {
                    libc::renameat(source_dir, source.as_ptr(), destination_dir, destination.as_ptr())
                };
                return if result == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) };
            }
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {}
            Err(error) => return Err(error),
        }
        let fd = unsafe {
            libc::openat(
                destination_dir,
                destination.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600_u32,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let placeholder = unsafe { std::fs::File::from_raw_fd(fd) };
        let claimed = placeholder.metadata()?;
        drop(placeholder);
        let still_claimed = || {
            use std::os::unix::fs::MetadataExt;
            stat_at(destination_dir, destination).is_ok_and(|current| {
                current.st_mode & libc::S_IFMT == libc::S_IFREG
                    && current.st_size == 0
                    && current.st_dev == claimed.dev()
                    && current.st_ino == claimed.ino()
            })
        };
        if !still_claimed() {
            return Err(occupied());
        }
        let result = unsafe {
            libc::renameat(source_dir, source.as_ptr(), destination_dir, destination.as_ptr())
        };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if still_claimed() {
                unsafe {
                    libc::unlinkat(destination_dir, destination.as_ptr(), 0);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    fn identity_at(parent: i32, leaf: &CString) -> Result<crate::platform::FileIdentity> {
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        let result = unsafe {
            libc::fstatat(
                parent,
                leaf.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("read claim identity");
        }
        Ok(crate::platform::FileIdentity {
            volume: stat.st_dev,
            file: stat.st_ino,
        })
    }

    pub(crate) fn restore(
        targets: &[(&RestoreTarget, Option<crate::platform::FileIdentity>)],
    ) -> Vec<RestoreOutcome> {
        let Ok(trash) = home_trash_dir() else {
            return targets
                .iter()
                .map(|_| RestoreOutcome::Failed("could not resolve the Trash directory".into()))
                .collect();
        };
        restore_from(targets, &trash)
    }

    pub fn forget_restore_record(original: &Path) {
        let Ok(trash) = home_trash_dir() else {
            return;
        };
        let info_dir = trash.join("info");
        let files_dir = trash.join("files");
        let Ok(entries) = std::fs::read_dir(&info_dir) else {
            return;
        };
        for entry in entries.flatten() {
            let info_path = entry.path();
            let Ok(contents) = std::fs::read_to_string(&info_path) else {
                continue;
            };
            if parse_trashinfo_orig(&contents).as_deref() != Some(original) {
                continue;
            }
            let trashed_file_exists = info_path
                .file_stem()
                .is_some_and(|stem| files_dir.join(stem).symlink_metadata().is_ok());
            if !trashed_file_exists {
                let _ = std::fs::remove_file(info_path);
            }
        }
    }

    fn restore_from(
        targets: &[(&RestoreTarget, Option<crate::platform::FileIdentity>)],
        trash: &Path,
    ) -> Vec<RestoreOutcome> {
        let info_dir = trash.join("info");
        let files_dir = trash.join("files");
        let mut outcomes: Vec<Option<RestoreOutcome>> =
            std::iter::repeat_with(|| None).take(targets.len()).collect();
        let mut failures: Vec<Option<String>> =
            std::iter::repeat_with(|| None).take(targets.len()).collect();

        if let Ok(entries) = std::fs::read_dir(&info_dir) {
            for entry in entries.flatten() {
                let info_path = entry.path();
                if info_path.extension().and_then(|extension| extension.to_str())
                    != Some("trashinfo")
                {
                    continue;
                }
                let Ok(contents) = std::fs::read_to_string(&info_path) else {
                    continue;
                };
                let Some(original) = parse_trashinfo_orig(&contents) else {
                    continue;
                };
                let Some(stem) = info_path.file_stem() else {
                    continue;
                };
                let source = files_dir.join(stem);
                for (index, (target, expected)) in targets.iter().enumerate() {
                    if outcomes[index].is_some() || target.original() != original {
                        continue;
                    }
                    match target.restore_external(&source, *expected) {
                        Ok(identity) => outcomes[index] = Some(RestoreOutcome::Restored(identity)),
                        Err(error)
                            if error
                                .downcast_ref::<std::io::Error>()
                                .is_some_and(|error| error.raw_os_error() == Some(libc::EEXIST)) =>
                        {
                            outcomes[index] = Some(RestoreOutcome::Conflict);
                        }
                        Err(error) => failures[index] = Some(error.to_string()),
                    }
                }
            }
        }

        outcomes
            .into_iter()
            .zip(failures)
            .map(|(outcome, failure)| {
                outcome.unwrap_or_else(|| {
                    RestoreOutcome::Failed(
                        failure.unwrap_or_else(|| "Trash item was not found".into()),
                    )
                })
            })
            .collect()
    }

    /// Pull the original location out of a `.trashinfo` body's `Path=` line
    /// (percent-decoded). Returns None if there's no `Path=` key.
    fn parse_trashinfo_orig(contents: &str) -> Option<PathBuf> {
        contents
            .lines()
            .find_map(|l| l.strip_prefix("Path="))
            .map(|v| percent_decode_path(v.trim()))
    }

    fn sync_dir(path: &Path) -> Result<()> {
        std::fs::File::open(path)
            .with_context(|| format!("open directory {} for durability", path.display()))?
            .sync_all()
            .with_context(|| format!("sync directory {}", path.display()))
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
        let expected = crate::platform::file_identity(path)
            .context("capture volume-qualified Trash source identity")?;
        trash_into_as(path, path, trash, expected)
    }

    fn trash_into_as(
        path: &Path,
        original_path: &Path,
        trash: &Path,
        expected: crate::platform::FileIdentity,
    ) -> Result<()> {
        if std::fs::symlink_metadata(path).is_err() {
            return Ok(());
        }
        crate::util::read_only::require_source_mutation(path)?;
        let files_dir = trash.join("files");
        let info_dir = trash.join("info");
        std::fs::create_dir_all(&files_dir)
            .with_context(|| format!("create {}", files_dir.display()))?;
        std::fs::create_dir_all(&info_dir)
            .with_context(|| format!("create {}", info_dir.display()))?;
        sync_dir(trash)?;
        if let Some(parent) = trash.parent() {
            sync_dir(parent)?;
        }

        let orig_name = original_path
            .file_name()
            .context("original path has no file name")?;
        let abs = absolute(original_path);

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
                    let result = (|| -> Result<()> {
                        let body = format!(
                            "[Trash Info]\nPath={}\nDeletionDate={}\n",
                            percent_encode_path(&abs),
                            deletion_date_now()
                        );
                        f.write_all(body.as_bytes())
                            .with_context(|| format!("write {}", info_path.display()))?;
                        f.sync_all()
                            .with_context(|| format!("sync {}", info_path.display()))?;
                        drop(f);
                        sync_dir(&info_dir)?;
                        move_into(path, &target, expected)
                    })();
                    if result.is_err() && crate::platform::file_identity(&target) != Some(expected) {
                        let _ = std::fs::remove_file(&info_path);
                        let _ = sync_dir(&info_dir);
                    }
                    return result;
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

    fn move_into(
        src: &Path,
        dst: &Path,
        expected: crate::platform::FileIdentity,
    ) -> Result<()> {
        let src_parent = src.parent().context("source has no parent directory")?;
        let dst_parent = dst.parent().context("destination has no parent directory")?;
        sync_dir(src_parent)?;
        sync_dir(dst_parent)?;
        if crate::platform::file_identity(src) != Some(expected) {
            anyhow::bail!("Trash source identity changed at the mutation boundary");
        }
        match crate::util::rename_no_replace(src, dst) {
            Ok(()) => {
                if crate::platform::file_identity(dst) != Some(expected) {
                    anyhow::bail!(
                        "Trash backend moved an object that does not match the recovery journal"
                    );
                }
                if let Err(error) = sync_dir(dst_parent).and_then(|_| {
                    if src_parent == dst_parent {
                        Ok(())
                    } else {
                        sync_dir(src_parent)
                    }
                }) {
                    let _ = crate::util::rename_no_replace(dst, src);
                    let _ = sync_dir(dst_parent);
                    let _ = sync_dir(src_parent);
                    return Err(error).context("durably commit Trash rename");
                }
                Ok(())
            }
            Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
                Err(error).context(
                    "Trash is on another filesystem; refusing an identity-changing copy so Undo remains provable",
                )
            }
            Err(error) => {
                Err(error).with_context(|| format!("move {} to trash", src.display()))
            }
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
        fn emulated_directory_relative_rename_never_clobbers() {
            let base = std::env::temp_dir()
                .join(format!("fileid-renameat-emulated-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&base).unwrap();
            let dir = std::fs::File::open(&base).unwrap();
            let fd = dir.as_raw_fd();
            let name = |text: &str| CString::new(text).unwrap();
            std::fs::write(base.join("source"), b"source").unwrap();
            std::fs::write(base.join("occupant"), b"occupant").unwrap();
            let error =
                renameat_no_replace_emulated(fd, &name("source"), fd, &name("occupant")).unwrap_err();
            assert_eq!(error.raw_os_error(), Some(libc::EEXIST));
            assert_eq!(std::fs::read(base.join("occupant")).unwrap(), b"occupant");
            renameat_no_replace_emulated(fd, &name("source"), fd, &name("moved")).unwrap();
            assert_eq!(std::fs::read(base.join("moved")).unwrap(), b"source");
            assert!(!base.join("source").exists());
            assert!(renameat_no_replace_emulated(fd, &name("missing"), fd, &name("target")).is_err());
            assert!(!base.join("target").exists());
            let _ = std::fs::remove_dir_all(&base);
        }

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
        fn staged_trash_records_and_restores_the_original_path() {
            let base =
                std::env::temp_dir().join(format!("fileid-trash-staged-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&base);
            let trash = base.join("Trash");
            let src_dir = base.join("src");
            std::fs::create_dir_all(&src_dir).unwrap();
            let original = src_dir.join("photo.jpg");
            let staged = src_dir.join(".fileid-trash-claim");
            std::fs::write(&staged, b"payload").unwrap();
            let expected = crate::platform::file_identity(&staged);

            trash_into_as(&staged, &original, &trash, expected.unwrap()).unwrap();
            assert!(trash.join("files/photo.jpg").exists());
            let info = std::fs::read_to_string(trash.join("info/photo.jpg.trashinfo")).unwrap();
            assert!(info.contains("photo.jpg"));
            assert!(!info.contains("fileid-trash-claim"));
            let root = std::fs::canonicalize(&src_dir).unwrap();
            let target = RestoreTarget::prepare(&original, &[root]).unwrap();
            let outcomes = restore_from(&[(&target, expected)], &trash);
            assert!(matches!(outcomes.as_slice(), [RestoreOutcome::Restored(_)]));
            assert_eq!(std::fs::read(&original).unwrap(), b"payload");
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
        fn trash_backend_rejects_a_mismatched_mutation_identity_before_move() {
            let base = std::env::temp_dir().join(format!(
                "fileid-trash-mismatch-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&base).unwrap();
            let source = base.join("source.bin");
            let other = base.join("other.bin");
            let destination = base.join("trashed.bin");
            std::fs::write(&source, b"source").unwrap();
            std::fs::write(&other, b"other!").unwrap();
            let wrong_identity = crate::platform::file_identity(&other).unwrap();

            let error = move_into(&source, &destination, wrong_identity).unwrap_err();

            assert!(error.to_string().contains("mutation boundary"));
            assert_eq!(std::fs::read(&source).unwrap(), b"source");
            assert!(!destination.exists());
            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        fn occupied_dangling_target_does_not_leak_trashinfo() {
            use std::os::unix::fs::symlink;

            let base = std::env::temp_dir().join(format!(
                "fileid-trash-dangling-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let trash = base.join("Trash");
            let files = trash.join("files");
            let source_dir = base.join("source");
            std::fs::create_dir_all(&files).unwrap();
            std::fs::create_dir_all(&source_dir).unwrap();
            let source = source_dir.join("blocked.bin");
            std::fs::write(&source, b"payload").unwrap();
            symlink("missing-target", files.join("blocked.bin")).unwrap();
            let expected = crate::platform::file_identity(&source).unwrap();

            assert!(trash_into_as(&source, &source, &trash, expected).is_err());

            assert_eq!(std::fs::read(&source).unwrap(), b"payload");
            assert!(!trash.join("info/blocked.bin.trashinfo").exists());
            let _ = std::fs::remove_dir_all(base);
        }

        #[test]
        fn cross_filesystem_trash_fails_without_copying() {
            use std::os::unix::fs::MetadataExt;

            let shared_memory = Path::new("/dev/shm");
            let temp = std::env::temp_dir();
            let Ok(shared_metadata) = std::fs::metadata(shared_memory) else {
                return;
            };
            let Ok(temp_metadata) = std::fs::metadata(&temp) else {
                return;
            };
            if shared_metadata.dev() == temp_metadata.dev() {
                return;
            }
            let source_dir = shared_memory.join(format!(
                "fileid-trash-exdev-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let destination_dir = temp.join(format!(
                "fileid-trash-exdev-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&source_dir).unwrap();
            std::fs::create_dir_all(&destination_dir).unwrap();
            let source = source_dir.join("source.bin");
            let destination = destination_dir.join("destination.bin");
            std::fs::write(&source, b"payload").unwrap();
            let expected = crate::platform::file_identity(&source).unwrap();

            let error = move_into(&source, &destination, expected).unwrap_err();

            assert!(error.to_string().contains("another filesystem"));
            assert_eq!(std::fs::read(&source).unwrap(), b"payload");
            assert!(!destination.exists());
            let _ = std::fs::remove_dir_all(source_dir);
            let _ = std::fs::remove_dir_all(destination_dir);
        }

        #[test]
        fn restore_selects_matching_identity_across_same_path_generations() {
            let base = std::env::temp_dir().join(format!(
                "fileid-trash-generations-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let trash = base.join("Trash");
            let src_dir = base.join("src");
            std::fs::create_dir_all(&src_dir).unwrap();
            let original = src_dir.join("dup.txt");
            std::fs::write(&original, b"older").unwrap();
            trash_into(&original, &trash).unwrap();
            std::fs::write(&original, b"newer").unwrap();
            let expected = crate::platform::file_identity(&original);
            trash_into(&original, &trash).unwrap();

            let root = std::fs::canonicalize(&src_dir).unwrap();
            let target = RestoreTarget::prepare(&original, &[root]).unwrap();
            let outcomes = restore_from(&[(&target, expected)], &trash);

            assert!(matches!(outcomes.as_slice(), [RestoreOutcome::Restored(_)]));
            assert_eq!(std::fs::read(&original).unwrap(), b"newer");
            assert_eq!(std::fs::read(trash.join("files/dup.txt")).unwrap(), b"older");
            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        fn restore_recovers_durable_quarantine_after_interruption() {
            let base = std::env::temp_dir().join(format!(
                "fileid-trash-quarantine-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let trash = base.join("Trash");
            let src_dir = base.join("src");
            std::fs::create_dir_all(&src_dir).unwrap();
            let original = src_dir.join("resume.txt");
            std::fs::write(&original, b"payload").unwrap();
            let expected = crate::platform::file_identity(&original);
            trash_into(&original, &trash).unwrap();
            let source = trash.join("files/resume.txt");
            let quarantine = trash
                .join("files/.fileid-restore-finalize-resume.txt");
            std::fs::rename(&source, &quarantine).unwrap();

            let root = std::fs::canonicalize(&src_dir).unwrap();
            let target = RestoreTarget::prepare(&original, &[root]).unwrap();
            let outcomes = restore_from(&[(&target, expected)], &trash);

            assert!(matches!(outcomes.as_slice(), [RestoreOutcome::Restored(_)]));
            assert_eq!(std::fs::read(&original).unwrap(), b"payload");
            assert!(!quarantine.exists());
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
            let expected = crate::platform::file_identity(&file);

            trash_into(&file, &trash).unwrap();
            assert!(!file.exists(), "original should be gone after trash");
            assert!(trash.join("files/restoreme.txt").exists());

            let root = std::fs::canonicalize(&src_dir).unwrap();
            let target = RestoreTarget::prepare(&file, &[root]).unwrap();
            let outcomes = restore_from(&[(&target, expected)], &trash);
            assert!(matches!(outcomes.as_slice(), [RestoreOutcome::Restored(_)]));

            assert!(file.exists(), "file should be back at its original path");
            assert_eq!(std::fs::read(&file).unwrap(), b"payload");
            assert!(
                trash.join("info/restoreme.txt.trashinfo").exists(),
                "metadata remains until the command verifies the restored path identity"
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
            let expected = crate::platform::file_identity(&file);
            trash_into(&file, &trash).unwrap();

            let root = std::fs::canonicalize(&src_dir).unwrap();
            let target = RestoreTarget::prepare(&file, &[root]).unwrap();
            std::fs::write(&file, b"new").unwrap();
            let outcomes = restore_from(&[(&target, expected)], &trash);
            assert!(matches!(outcomes.as_slice(), [RestoreOutcome::Conflict]));

            assert_eq!(
                std::fs::read(&file).unwrap(),
                b"new",
                "restore must not overwrite a file that now occupies the original path"
            );
            assert!(trash.join("files/keep.txt").exists());
            assert!(trash.join("info/keep.txt.trashinfo").exists());
            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        fn restore_rejects_substituted_trash_source() {
            let base = std::env::temp_dir().join(format!(
                "fileid-trash-restore-substitute-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let trash = base.join("Trash");
            let source_dir = base.join("src");
            std::fs::create_dir_all(&source_dir).unwrap();
            let original = source_dir.join("file.bin");
            std::fs::write(&original, b"original").unwrap();
            let expected = crate::platform::file_identity(&original);
            trash_into(&original, &trash).unwrap();
            let trash_file = trash.join("files/file.bin");
            // Substitute the trashed file with different content under a
            // GUARANTEED-distinct identity. Allocating the replacement beside the
            // still-present original and renaming it into place prevents an
            // inode-reusing filesystem (tmpfs on CI, unlike ext4 under WSL) from
            // handing the replacement the freed original's inode, which would make
            // the (dev, inode) identity spuriously match and defeat the check.
            let substitute = trash.join("files/file.bin.substitute");
            std::fs::write(&substitute, b"substitute").unwrap();
            std::fs::rename(&substitute, &trash_file).unwrap();
            let root = std::fs::canonicalize(&source_dir).unwrap();
            let target = RestoreTarget::prepare(&original, &[root]).unwrap();

            let outcomes = restore_from(&[(&target, expected)], &trash);

            assert!(matches!(outcomes.as_slice(), [RestoreOutcome::Failed(_)]));
            assert!(!original.exists());
            assert_eq!(std::fs::read(trash_file).unwrap(), b"substitute");
            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        fn restore_target_rejects_symlinked_parent() {
            use std::os::unix::fs::symlink;

            let base = std::env::temp_dir().join(format!(
                "fileid-restore-symlink-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let root = base.join("root");
            let outside = base.join("outside");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            symlink(&outside, root.join("parent")).unwrap();
            let authorized = std::fs::canonicalize(&root).unwrap();
            let original = root.join("parent/file.bin");

            assert!(RestoreTarget::prepare(&original, &[authorized]).is_err());
            assert!(!outside.join("file.bin").exists());
            let _ = std::fs::remove_dir_all(&base);
        }

        #[test]
        fn pinned_restore_parent_never_follows_replacement_symlink() {
            use std::os::unix::fs::symlink;

            let base = std::env::temp_dir().join(format!(
                "fileid-restore-pinned-{}-{}",
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let root = base.join("root");
            let parent = root.join("parent");
            let moved = base.join("moved-parent");
            let outside = base.join("outside");
            std::fs::create_dir_all(&parent).unwrap();
            std::fs::create_dir_all(&outside).unwrap();
            let original = parent.join("file.bin");
            let claim = parent.join(".fileid-trash-claim");
            std::fs::write(&claim, b"payload").unwrap();
            let expected = crate::platform::file_identity(&claim);
            let authorized = std::fs::canonicalize(&root).unwrap();
            let target = RestoreTarget::prepare(&original, &[authorized]).unwrap();

            std::fs::rename(&parent, &moved).unwrap();
            symlink(&outside, &parent).unwrap();
            target.restore_claim(&claim, expected).unwrap();

            assert!(!outside.join("file.bin").exists());
            assert_eq!(std::fs::read(moved.join("file.bin")).unwrap(), b"payload");
            let _ = std::fs::remove_dir_all(&base);
        }
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub mod trash {
    use std::path::{Path, PathBuf};

    pub const UNSUPPORTED: &str = "Moving files to the Trash is not supported by the FileID \
        engine on macOS; nothing was moved (use the FileID macOS app, or `fileid dedupe --delete` \
        for permanent removal)";

    /// Returns all-false so callers report failure rather than claim a trash.
    pub fn trash(paths: &[PathBuf]) -> Vec<bool> {
        vec![false; paths.len()]
    }

    pub(crate) fn trash_path_as(
        _source: &Path,
        _original_path: &Path,
        _expected: crate::platform::FileIdentity,
    ) -> anyhow::Result<()> {
        anyhow::bail!(UNSUPPORTED)
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn trash_refuses_with_a_user_facing_reason_and_moves_nothing() {
            let dir = std::env::temp_dir().join(format!("fileid-mac-trash-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("keep.txt");
            std::fs::write(&file, b"keep").unwrap();
            let identity = crate::platform::file_identity(&file).unwrap();
            assert_eq!(super::trash(std::slice::from_ref(&file)), vec![false]);
            let error = super::trash_path_as(&file, &file, identity).unwrap_err();
            assert_eq!(error.to_string(), super::UNSUPPORTED);
            assert_eq!(std::fs::read(&file).unwrap(), b"keep");
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// ocr  (tesseract CLI, best-effort)
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub mod ocr {
    use super::linux_util::{run_output_bounded, temp_file};
    use anyhow::Result;
    use std::io::Write;
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    /// Best-effort OCR via the `tesseract` CLI. The buffer is tightly-packed
    /// RGB8 (3 bytes/pixel), matching the Windows `recognize`. Writes a P6 PPM
    /// to a temp file, runs `tesseract <ppm> stdout`, returns the text. Never
    /// fails: a missing or erroring tesseract yields empty text.
    pub fn recognize(rgb: &[u8], width: u32, height: u32) -> Result<String> {
        const MAX_DIM: u32 = 16384;
        if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
            return Ok(String::new());
        }
        let need = (width as usize) * (height as usize) * 3;
        if rgb.len() < need {
            return Ok(String::new());
        }
        let img = temp_file("ppm");
        if write_ppm(&rgb[..need], width, height, &img).is_err() {
            let _ = std::fs::remove_file(&img);
            return Ok(String::new());
        }
        let output = run_output_bounded(
            Command::new("tesseract").arg(&img).arg("stdout"),
            16 * 1024 * 1024,
            Duration::from_secs(60),
        );
        let _ = std::fs::remove_file(&img);
        Ok(output.map(|stdout| String::from_utf8_lossy(&stdout).trim().to_string()).unwrap_or_default())
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

    /// No OCR backend ships with the Rust engine on macOS. Empty text keeps
    /// scans quiet (an error here was logged once per image) while one notice
    /// says why images have no OCR text.
    pub fn recognize(_rgb: &[u8], _width: u32, _height: u32) -> Result<String> {
        static NOTICE: std::sync::Once = std::sync::Once::new();
        NOTICE.call_once(|| {
            tracing::info!("OCR is not available in the FileID engine on macOS; images are catalogued without OCR text");
        });
        Ok(String::new())
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn recognize_returns_empty_text_instead_of_an_error_per_image() {
            assert_eq!(super::recognize(&[0; 12], 2, 2).unwrap(), "");
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// video  (ffmpeg keyframe, best-effort)
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub mod video {
    use super::linux_util::run_output_bounded;
    use anyhow::{Context, Result};
    use std::path::Path;
    use std::process::Command;
    use std::time::Duration;

    const MAX_VIDEO_EDGE: u64 = 1_280;
    const MAX_VIDEO_PIXELS: u64 = MAX_VIDEO_EDGE * MAX_VIDEO_EDGE;
    const MAX_PPM_BYTES: u64 = MAX_VIDEO_PIXELS * 3 + 64 * 1024;
    pub(crate) const VIDEO_DECODE_RESERVATION_BYTES: usize = 64 * 1024 * 1024;

    #[derive(Debug, Clone)]
    pub struct VideoFrame {
        pub width: u32,
        pub height: u32,
        /// Tightly packed RGB8.
        pub rgb: Vec<u8>,
    }

    /// Keyframe at ~25% of duration. Uses the vetted FFmpeg runtime FileID
    /// ships (`FILEID_FFMPEG_DIR`); a system `ffmpeg` on PATH is the fallback
    /// when no runtime is configured.
    pub fn keyframe_25pct(path: &Path) -> Result<VideoFrame> {
        let never = std::sync::atomic::AtomicBool::new(false);
        if let Ok(probe) = crate::commands::video_export::analysis_probe(path, &never) {
            let (width, height) = crate::commands::video_export::fitted_size(probe.width, probe.height, 1280);
            let rgb = crate::commands::video_export::frame_rgb(path, (probe.duration * 0.25).max(0.0), width, height, &never)?;
            return Ok(VideoFrame { width, height, rgb });
        }
        system_keyframe_25pct(path)
    }

    /// The PPM stream is consumed while ffmpeg runs and the child is killed if
    /// it emits more than one bounded frame.
    fn system_keyframe_25pct(path: &Path) -> Result<VideoFrame> {
        let seconds = probe_duration(path).map(|d| (d * 0.25).max(0.0)).unwrap_or(0.0);
        let bytes = run_output_bounded(
            Command::new("ffmpeg")
                .arg("-nostdin")
                .arg("-loglevel")
                .arg("error")
                .arg("-ss")
                .arg(format!("{seconds:.3}"))
                .arg("-i")
                .arg(path)
                .arg("-frames:v")
                .arg("1")
                .arg("-vf")
                .arg("scale=1280:1280:force_original_aspect_ratio=decrease")
                .arg("-f")
                .arg("image2pipe")
                .arg("-vcodec")
                .arg("ppm")
                .arg("pipe:1"),
            MAX_PPM_BYTES as usize,
            Duration::from_secs(60),
        )
        .context("ffmpeg unavailable or produced no keyframe")?;
        let (width, height, rgb) =
            parse_ppm(&bytes).context("parse PPM keyframe emitted by ffmpeg")?;
        Ok(VideoFrame { width, height, rgb })
    }

    fn probe_duration(path: &Path) -> Option<f64> {
        let output = run_output_bounded(
            Command::new("ffprobe")
                .arg("-v")
                .arg("quiet")
                .arg("-show_entries")
                .arg("format=duration")
                .arg("-of")
                .arg("csv=p=0")
                .arg(path),
            4 * 1024,
            Duration::from_secs(10),
        )
        .ok()?;
        String::from_utf8_lossy(&output)
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
        let pixels = w.checked_mul(h)?;
        if w == 0
            || h == 0
            || w > MAX_VIDEO_EDGE
            || h > MAX_VIDEO_EDGE
            || pixels > MAX_VIDEO_PIXELS
            || maxval != 255
            || !bytes
                .get(pos)
                .is_some_and(|separator| separator.is_ascii_whitespace())
        {
            return None;
        }
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

    #[cfg(test)]
    mod tests {
        use super::parse_ppm;

        #[test]
        fn ppm_parser_accepts_bounded_rgb_and_rejects_unsafe_dimensions() {
            let valid = b"P6\n2 1\n255\n\x01\x02\x03\x04\x05\x06";
            let (w, h, rgb) = parse_ppm(valid).expect("valid PPM");
            assert_eq!((w, h), (2, 1));
            assert_eq!(rgb, [1, 2, 3, 4, 5, 6]);

            assert!(parse_ppm(b"P6\n0 1\n255\n").is_none());
            assert!(parse_ppm(b"P6\n4294967296 1\n255\n").is_none());
            assert!(parse_ppm(b"P6\n64000001 1\n255\n").is_none());
            assert!(parse_ppm(b"P6\n2 1\n255\n\x01").is_none());
            assert!(parse_ppm(b"P6\n1 1\n255X\x01\x02\x03").is_none());
        }
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub mod video {
    use anyhow::Result;
    use std::path::Path;
    pub(crate) const VIDEO_DECODE_RESERVATION_BYTES: usize = 64 * 1024 * 1024;
    #[derive(Debug, Clone)]
    pub struct VideoFrame {
        pub width: u32,
        pub height: u32,
        /// Tightly packed RGB8.
        pub rgb: Vec<u8>,
    }
    /// Keyframe at ~25% of duration through FileID's vetted FFmpeg runtime
    /// (`FILEID_FFMPEG_DIR`, or `media/` beside the engine), as on Linux.
    pub fn keyframe_25pct(path: &Path) -> Result<VideoFrame> {
        use anyhow::Context;
        let never = std::sync::atomic::AtomicBool::new(false);
        let probe = crate::commands::video_export::analysis_probe(path, &never)
            .context("video frames need FileID's FFmpeg runtime")?;
        let (width, height) =
            crate::commands::video_export::fitted_size(probe.width, probe.height, 1280);
        let rgb = crate::commands::video_export::frame_rgb(
            path,
            (probe.duration * 0.25).max(0.0),
            width,
            height,
            &never,
        )?;
        Ok(VideoFrame { width, height, rgb })
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn missing_runtime_is_a_clear_error() {
            if std::env::var_os("FILEID_FFMPEG_DIR").is_some() {
                return;
            }
            let error = super::keyframe_25pct(std::path::Path::new("/nonexistent/clip.mp4"))
                .unwrap_err();
            assert!(format!("{error:#}").contains("FFmpeg runtime"), "{error:#}");
        }
    }
}

// ────────────────────────────────────────────────────────────────────
// heic  (Linux libheif CLI bridge; graceful stub on other non-Windows OSes)
// ────────────────────────────────────────────────────────────────────
#[cfg(target_os = "linux")]
pub mod heic {
    use super::linux_util::{read_bounded, temp_file, terminate_process_group};
    use anyhow::{Context, Result};
    use std::os::unix::fs::DirBuilderExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const MAX_HEIC_PIXELS: u64 = 50_000_000;
    const MAX_CONVERTED_PNG_BYTES: u64 = 256 * 1024 * 1024;
    const MAX_CONVERTED_FILES: usize = 64;

    /// Best-effort HEIC/HEIF decode through the optional libheif CLI tools.
    /// Every generated member lives in one private, aggregate-capped temp
    /// directory that is removed on all return paths.
    pub fn decode(path: &Path) -> Result<(Vec<u8>, u32, u32)> {
        let outputs = TempOutputDir::create()?;
        let out = outputs.path().join("frame.png");
        let mut produced: Option<PathBuf> = None;
        for tool in ["heif-dec", "heif-convert"] {
            clear_output_dir(outputs.path()).context("clear heic output directory")?;
            if run_converter_bounded(tool, path, &out, outputs.path()) {
                if let Some(p) = resolve_output(&out) {
                    produced = Some(p);
                    break;
                }
            }
        }
        let Some(png) = produced else {
            anyhow::bail!("heif-dec/heif-convert unavailable or produced no output");
        };

        let dimensions = image::image_dimensions(&png).ok();
        let within_limits = dimensions.is_some_and(|(w, h)| {
            w > 0
                && h > 0
                && u64::from(w) * u64::from(h) <= MAX_HEIC_PIXELS
        });
        let bytes = if within_limits {
            read_bounded(&png, MAX_CONVERTED_PNG_BYTES as usize)
                .context("read converted heic png")
        } else {
            Err(anyhow::anyhow!("converted heic exceeds decode limits"))
        };
        let bytes = bytes?;
        let (encoded_w, encoded_h) = image::ImageReader::new(std::io::Cursor::new(&bytes))
            .with_guessed_format()
            .context("guess converted heic png format")?
            .into_dimensions()
            .context("read converted heic png dimensions")?;
        anyhow::ensure!(
            encoded_w > 0
                && encoded_h > 0
                && u64::from(encoded_w) * u64::from(encoded_h) <= MAX_HEIC_PIXELS,
            "converted heic exceeds pixel limit"
        );
        let dyn_img = image::load_from_memory(&bytes).context("decode converted heic png")?;
        let rgb = dyn_img.into_rgb8();
        let (w, h) = rgb.dimensions();
        Ok((rgb.into_raw(), w, h))
    }

    struct TempOutputDir(PathBuf);

    impl TempOutputDir {
        fn create() -> Result<Self> {
            let path = temp_file("heic-output");
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .context("create heic output directory")?;
            Ok(Self(path))
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempOutputDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn run_converter_bounded(tool: &str, input: &Path, out: &Path, dir: &Path) -> bool {
        let mut command = Command::new(tool);
        command
            .arg(input)
            .arg(out)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        crate::platform::configure_child_lifetime(&mut command);
        let child = command.spawn();
        let Ok(mut child) = child else {
            return false;
        };
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if !output_dir_within_limits(dir) || Instant::now() >= deadline {
                terminate_process_group(&mut child);
                return false;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    return status.success() && output_dir_within_limits(dir);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    terminate_process_group(&mut child);
                    return false;
                }
            }
        }
    }

    fn clear_output_dir(dir: &Path) -> std::io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            std::fs::remove_file(entry?.path())?;
        }
        Ok(())
    }

    fn output_dir_within_limits(dir: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return false;
        };
        let mut files = 0usize;
        let mut bytes = 0u64;
        for entry in entries {
            let Ok(entry) = entry else {
                return false;
            };
            let Ok(metadata) = entry.metadata() else {
                return false;
            };
            if !metadata.is_file() {
                return false;
            }
            files += 1;
            let Some(total) = bytes.checked_add(metadata.len()) else {
                return false;
            };
            bytes = total;
            if files > MAX_CONVERTED_FILES || bytes > MAX_CONVERTED_PNG_BYTES {
                return false;
            }
        }
        true
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

    #[cfg(test)]
    mod tests {
        use super::{clear_output_dir, output_dir_within_limits, TempOutputDir, MAX_CONVERTED_PNG_BYTES};

        #[test]
        fn converted_output_directory_is_aggregate_capped_and_drop_cleaned() {
            let path;
            {
                let outputs = TempOutputDir::create().expect("temp output directory");
                path = outputs.path().to_path_buf();
                std::fs::write(path.join("frame-1.png"), [1]).expect("first output");
                let second = std::fs::File::create(path.join("frame-2.png"))
                    .expect("second output");
                second
                    .set_len(MAX_CONVERTED_PNG_BYTES)
                    .expect("sparse output");
                assert!(!output_dir_within_limits(&path));
                clear_output_dir(&path).expect("clear outputs");
                assert!(output_dir_within_limits(&path));
            }
            assert!(!path.exists());
        }
    }
}

#[cfg(all(not(windows), not(target_os = "linux")))]
pub mod heic {
    use anyhow::{Context, Result};
    use std::os::unix::fs::DirBuilderExt;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    const MAX_HEIC_PIXELS: u64 = 50_000_000;
    const MAX_CONVERTED_PNG_BYTES: u64 = 256 * 1024 * 1024;

    /// HEIC/HEIF decode through the system `sips` tool (ImageIO). The PNG it
    /// writes lives in a private temp directory removed on every path.
    pub fn decode(path: &Path) -> Result<(Vec<u8>, u32, u32)> {
        anyhow::ensure!(path.is_absolute(), "heic decode needs an absolute path");
        let outputs = TempOutputDir::create()?;
        let out = outputs.0.join("frame.png");
        let mut child = Command::new("/usr/bin/sips")
            .args(["-s", "format", "png"])
            .arg(path)
            .arg("--out")
            .arg(&out)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("start sips")?;
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            match child.try_wait().context("wait for sips")? {
                Some(status) => {
                    anyhow::ensure!(status.success(), "sips could not convert the image");
                    break;
                }
                None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
                None => {
                    let _ = child.kill();
                    let _ = child.wait();
                    anyhow::bail!("sips timed out converting the image");
                }
            }
        }
        let size = std::fs::metadata(&out).context("sips produced no output")?.len();
        anyhow::ensure!(size <= MAX_CONVERTED_PNG_BYTES, "converted image exceeds decode limits");
        let (width, height) =
            image::image_dimensions(&out).context("read converted image dimensions")?;
        anyhow::ensure!(
            width > 0 && height > 0 && u64::from(width) * u64::from(height) <= MAX_HEIC_PIXELS,
            "converted image exceeds the pixel limit"
        );
        let rgb = image::open(&out).context("decode converted image")?.into_rgb8();
        let (width, height) = rgb.dimensions();
        Ok((rgb.into_raw(), width, height))
    }

    struct TempOutputDir(PathBuf);

    impl TempOutputDir {
        fn create() -> Result<Self> {
            let path = std::env::temp_dir()
                .join(format!("fileid-heic-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
            std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .context("create heic output directory")?;
            Ok(Self(path))
        }
    }

    impl Drop for TempOutputDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn decodes_a_heic_written_by_sips() {
            let dir = std::env::temp_dir().join(format!("fileid-heic-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            let png = dir.join("source.png");
            let heic = dir.join("photo.heic");
            image::RgbImage::from_pixel(64, 48, image::Rgb([200, 40, 10])).save(&png).unwrap();
            let converted = std::process::Command::new("/usr/bin/sips")
                .args(["-s", "format", "heic"])
                .arg(&png)
                .arg("--out")
                .arg(&heic)
                .output()
                .is_ok_and(|output| output.status.success() && heic.exists());
            if converted {
                let (rgb, width, height) = super::decode(&heic).unwrap();
                assert_eq!((width, height), (64, 48));
                assert_eq!(rgb.len(), 64 * 48 * 3);
                assert!(rgb[0] > 150 && rgb[1] < 100, "colour survived: {:?}", &rgb[..3]);
            }
            assert!(super::decode(&dir.join("missing.heic")).is_err());
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
