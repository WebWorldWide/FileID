//! The names of the files inside a zip, so a search for a file finds the
//! archive that holds it. Reads only the zip's central directory (its table of
//! contents at the end of the file); nothing is extracted or decompressed.
//! Mirrors the Swift engine's `ArchiveNames`.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_NAMES_BYTES: usize = 256 * 1024;
const MAX_DIRECTORY_BYTES: u64 = 32 * 1024 * 1024;

fn le16(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn le64(b: &[u8], i: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(i..i + 8)?.try_into().ok()?))
}

fn le32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

pub(crate) fn list(path: &Path) -> Option<String> {
    if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip")) {
        return None;
    }
    let mut file = std::fs::File::open(crate::util::path_safety::to_extended_length(path)).ok()?;
    let size = file.seek(SeekFrom::End(0)).ok()?;
    if size < 22 {
        return None;
    }
    // The end-of-central-directory record sits in the last 22 bytes plus an optional comment (<= 64 KiB).
    let tail_len = size.min(22 + 65_535);
    file.seek(SeekFrom::Start(size - tail_len)).ok()?;
    let mut tail = vec![0u8; tail_len as usize];
    file.read_exact(&mut tail).ok()?;
    let eocd = (0..=tail.len() - 22).rev().find(|&i| tail[i..i + 4] == [0x50, 0x4B, 0x05, 0x06])?;
    let mut entries = le16(&tail, eocd + 10) as usize;
    let mut length = u64::from(le32(&tail, eocd + 12));
    let mut offset = u64::from(le32(&tail, eocd + 16));
    let eocd_offset = size - tail_len + eocd as u64;
    if entries == 0xFFFF || length == 0xFFFF_FFFF || offset == 0xFFFF_FFFF {
        // ZIP64: a locator just before the end record points at the 64-bit end record.
        if eocd < 20 || tail[eocd - 20..eocd - 16] != [0x50, 0x4B, 0x06, 0x07] {
            return None;
        }
        let record = le64(&tail, eocd - 12).filter(|&r| r < eocd_offset)?;
        file.seek(SeekFrom::Start(record)).ok()?;
        let mut r = [0u8; 56];
        file.read_exact(&mut r).ok()?;
        if r[0..4] != [0x50, 0x4B, 0x06, 0x06] {
            return None;
        }
        entries = usize::try_from(le64(&r, 32)?).ok()?;
        length = le64(&r, 40)?;
        offset = le64(&r, 48)?;
    }
    if length == 0 || length > MAX_DIRECTORY_BYTES || offset.checked_add(length)? > eocd_offset {
        return None;
    }
    file.seek(SeekFrom::Start(offset)).ok()?;
    let mut dir = vec![0u8; length as usize];
    file.read_exact(&mut dir).ok()?;
    let mut names: Vec<String> = Vec::new();
    let (mut total, mut at) = (0usize, 0usize);
    for _ in 0..entries {
        if at + 46 > dir.len() || dir[at..at + 4] != [0x50, 0x4B, 0x01, 0x02] {
            break;
        }
        let name_len = le16(&dir, at + 28) as usize;
        let extra = le16(&dir, at + 30) as usize;
        let comment = le16(&dir, at + 32) as usize;
        if at + 46 + name_len > dir.len() {
            break;
        }
        let name = String::from_utf8_lossy(&dir[at + 46..at + 46 + name_len]).into_owned();
        at += 46 + name_len + extra + comment;
        if name.ends_with('/') || name.starts_with("__MACOSX/") || name.starts_with('.') || name.contains("/.") {
            continue;
        }
        total += name.len() + 1;
        if total > MAX_NAMES_BYTES {
            break;
        }
        names.push(name);
    }
    (!names.is_empty()).then(|| names.join("\n"))
}

// The test builds its fixture with the `zip` command-line tool, which Unix runners have.
#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::*;

    #[cfg(unix)]
    fn zip_with(dir: &Path, files: &[&str]) -> std::path::PathBuf {
        zip_with_flags(dir, files, &[])
    }

    #[cfg(unix)]
    fn zip_with_flags(dir: &Path, files: &[&str], flags: &[&str]) -> std::path::PathBuf {
        let src = dir.join("src");
        for name in files {
            let p = src.join(name);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, name.as_bytes()).unwrap();
        }
        let zip = dir.join("bundle.zip");
        let status = std::process::Command::new("zip").arg("-qr").args(flags).arg(&zip).arg(".").current_dir(&src).status();
        assert!(status.is_ok_and(|s| s.success()), "zip tool needed for this test");
        zip
    }

    #[cfg(unix)]
    #[test]
    fn lists_zip64_archives() {
        let dir = std::env::temp_dir().join(format!("fileid-zip64-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip = zip_with_flags(&dir, &["big/report.rpt", "notes.txt"], &["-fz"]);
        let raw = std::fs::read(&zip).unwrap();
        assert!(raw.windows(4).any(|w| w == [0x50, 0x4B, 0x06, 0x06]), "fixture must carry a ZIP64 end record");
        let listed = list(&zip).unwrap();
        let names: std::collections::HashSet<&str> = listed.lines().collect();
        assert_eq!(names, ["big/report.rpt", "notes.txt"].into_iter().collect());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn lists_names_and_survives_corruption() {
        let dir = std::env::temp_dir().join(format!("fileid-zip-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let zip = zip_with(&dir, &["notes/jeff-rule-changes.txt", "photo.jpg", ".DS_Store"]);
        let listed = list(&zip).unwrap();
        let names: std::collections::HashSet<&str> = listed.lines().collect();
        assert!(names.contains("notes/jeff-rule-changes.txt") && names.contains("photo.jpg"));
        assert!(!names.iter().any(|n| n.ends_with('/') || n.contains("DS_Store")));
        let original = std::fs::read(&zip).unwrap();
        let broken = dir.join("broken.zip");
        let mut state: u64 = 0x2545_F491_4F6C_DD1D;
        for round in 0..300 {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            let mut bytes = original.clone();
            if round % 3 == 0 {
                bytes.truncate((state >> 33) as usize % bytes.len());
            } else {
                for k in 0..4 {
                    let i = ((state >> (8 * k)) % bytes.len() as u64) as usize;
                    bytes[i] = (state >> 40) as u8;
                }
            }
            std::fs::write(&broken, &bytes).unwrap();
            let _ = list(&broken);
        }
        std::fs::write(&broken, b"not a zip").unwrap();
        assert!(list(&broken).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
