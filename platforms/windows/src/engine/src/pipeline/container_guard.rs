//! Pre-flight for symphonia. Its isomp4, Matroska and CAF readers size buffers
//! from lengths declared in the file before reading them (sample tables, `data`
//! atoms, EBML strings and binaries, CAF cookies and packet tables). A crafted
//! 64-bit length makes the allocator abort the process, which `catch_unwind`
//! cannot stop. This module finds the container symphonia's probe will pick and
//! refuses one whose declared lengths don't fit inside the file.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Mirrors `symphonia_core::probe::Probe::PROBE_SEARCH_LIMIT`.
const PROBE_LIMIT: u64 = 1 << 20;
const MAX_ELEMENTS: u32 = 200_000;
const MAX_DEPTH: u32 = 16;

pub(crate) enum Source<'a> {
    Bytes(&'a [u8]),
    File(std::fs::File, u64),
}

impl Source<'_> {
    pub(crate) fn open(path: &Path) -> Option<Source<'static>> {
        let file = std::fs::File::open(crate::util::path_safety::to_extended_length(path)).ok()?;
        let len = file.metadata().ok()?.len();
        Some(Source::File(file, len))
    }

    fn len(&self) -> u64 {
        match self {
            Source::Bytes(bytes) => bytes.len() as u64,
            Source::File(_, len) => *len,
        }
    }

    /// `n` bytes at `offset`, or fewer at the end of the source.
    fn read(&mut self, offset: u64, n: usize) -> Option<Vec<u8>> {
        let len = self.len();
        if offset >= len {
            return Some(Vec::new());
        }
        let n = n.min(usize::try_from(len - offset).unwrap_or(usize::MAX));
        match self {
            Source::Bytes(bytes) => {
                let start = usize::try_from(offset).ok()?;
                Some(bytes[start..start + n].to_vec())
            }
            Source::File(file, _) => {
                file.seek(SeekFrom::Start(offset)).ok()?;
                let mut buf = vec![0; n];
                file.read_exact(&mut buf).ok()?;
                Some(buf)
            }
        }
    }

    fn exact<const N: usize>(&mut self, offset: u64) -> Option<[u8; N]> {
        self.read(offset, N)?.try_into().ok()
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Container {
    IsoMp4,
    Matroska,
    Caf,
    Other,
}

fn marker(window: &[u8]) -> Option<Container> {
    match window {
        [_, _, _, _, ..] if &window[..4] == b"ftyp" => Some(Container::IsoMp4),
        [0x1A, 0x45, 0xDF, 0xA3, ..] => Some(Container::Matroska),
        [b'c', b'a', b'f', b'f', ..] => Some(Container::Caf),
        [b'f', b'L', b'a', b'C', ..] | [b'R', b'I', b'F', b'F', ..] | [b'F', b'O', b'R', b'M', ..] | [b'O', b'g', b'g', b'S', ..] => {
            Some(Container::Other)
        }
        [0xFF, 0xE2..=0xE7 | 0xF1..=0xF7 | 0xFA..=0xFF, ..] => Some(Container::Other),
        _ => None,
    }
}

/// Where symphonia's probe would start a format reader, and which one. Like the
/// probe it needs a full 16-byte window, skips ID3v2 tags, and gives up after
/// 1 MiB without a marker.
fn locate(src: &mut Source) -> Option<(Container, u64)> {
    let mut base = 0u64;
    loop {
        let head = src.read(base, (PROBE_LIMIT + 16) as usize)?;
        let mut found = None;
        let mut pos = 0usize;
        while pos + 16 <= head.len() && (pos as u64) + 2 <= PROBE_LIMIT {
            let window = &head[pos..pos + 16];
            if window.starts_with(b"ID3") {
                found = Some((None, pos));
                break;
            }
            if let Some(container) = marker(window) {
                found = Some((Some(container), pos));
                break;
            }
            pos += 1;
        }
        let (container, pos) = found?;
        let at = base + pos as u64;
        match container {
            Some(container) => return Some((container, at)),
            None => {
                let header = src.exact::<10>(at)?;
                let size = header[6..10].iter().fold(0u64, |acc, b| (acc << 7) | u64::from(b & 0x7F));
                let footer = if header[5] & 0x10 != 0 { 10 } else { 0 };
                base = at + 10 + size + footer;
            }
        }
    }
}

/// True when symphonia can probe `src` without trusting an impossible length.
pub(crate) fn symphonia_safe(mut src: Source) -> bool {
    let Some((container, at)) = locate(&mut src) else {
        return true;
    };
    let end = src.len();
    let mut budget = MAX_ELEMENTS;
    match container {
        Container::IsoMp4 => at >= 4 && bmff(&mut src, at - 4, end, Parent::Root, 0, &mut budget),
        Container::Matroska => ebml(&mut src, at, end, 0, &mut budget),
        Container::Caf => caf(&mut src, at, end),
        Container::Other => true,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Parent {
    Root,
    Box,
    Ilst,
    IlstItem,
}

fn be_u32(bytes: &[u8]) -> u64 {
    u64::from(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn bmff(src: &mut Source, start: u64, end: u64, parent: Parent, depth: u32, budget: &mut u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let mut at = start;
    while at < end {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let Some(header) = src.exact::<8>(at) else { return false };
        let kind: [u8; 4] = [header[4], header[5], header[6], header[7]];
        let (size, header_len) = match be_u32(&header) {
            0 => (end - at, 8),
            1 => match src.exact::<8>(at + 8) {
                Some(large) => (u64::from_be_bytes(large), 16),
                None => return false,
            },
            n => (n, 8),
        };
        if size < header_len || size > end - at {
            return false;
        }
        let (payload, stop) = (at + header_len, at + size);
        let len = size - header_len;
        let ok = match (&kind, parent) {
            (_, Parent::IlstItem) => match &kind {
                b"data" => len >= 8,
                b"mean" | b"name" => len >= 4,
                _ => true,
            },
            (_, Parent::Ilst) => bmff(src, payload, stop, Parent::IlstItem, depth + 1, budget),
            (b"moov" | b"trak" | b"mdia" | b"minf" | b"stbl" | b"udta" | b"edts" | b"dinf" | b"mvex", _) => {
                bmff(src, payload, stop, Parent::Box, depth + 1, budget)
            }
            (b"meta", _) => len >= 4 && bmff(src, payload + 4, stop, Parent::Box, depth + 1, budget),
            (b"ilst", _) => bmff(src, payload, stop, Parent::Ilst, depth + 1, budget),
            (b"hdlr", _) => len >= 24,
            (b"stts" | b"co64", _) => table(src, payload, len, 8, 8),
            (b"stsc", _) => table(src, payload, len, 8, 12),
            (b"stco", _) => table(src, payload, len, 8, 4),
            (b"stsz", _) => match src.exact::<12>(payload) {
                Some(fields) if len >= 12 => be_u32(&fields[4..8]) != 0 || be_u32(&fields[8..12]) * 4 <= len - 12,
                _ => false,
            },
            (b"stsd", _) => len >= 8 && bmff(src, payload + 8, stop, Parent::Box, depth + 1, budget),
            _ => true,
        };
        if !ok {
            return false;
        }
        at = stop;
    }
    true
}

/// A full box whose u32 entry count (after version/flags) must fit its payload.
fn table(src: &mut Source, payload: u64, len: u64, fixed: u64, entry: u64) -> bool {
    match src.exact::<8>(payload) {
        Some(fields) if len >= fixed => be_u32(&fields[4..8]) * entry <= len - fixed,
        _ => false,
    }
}

/// CAF: `caff` + version + flags, then chunks of type, i64 size and body.
fn caf(src: &mut Source, start: u64, end: u64) -> bool {
    let mut at = start + 8;
    while at < end {
        let Some(header) = src.exact::<12>(at) else { return false };
        let size = i64::from_be_bytes(header[4..12].try_into().unwrap_or([0; 8]));
        if &header[..4] == b"data" && size == -1 {
            return true;
        }
        let Ok(size) = u64::try_from(size) else { return false };
        if size > end - at - 12 {
            return false;
        }
        if &header[..4] == b"pakt" {
            // Every packet has at least one byte of audio, so the count is bounded by the file.
            match src.exact::<8>(at + 12) {
                Some(count) if size >= 24 => {
                    if u64::try_from(i64::from_be_bytes(count)).is_ok_and(|count| count > end) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        at += 12 + size;
    }
    true
}

const EBML_MASTERS: &[u64] = &[
    0x1A45DFA3, 0x18538067, 0x114D9B74, 0x4DBB, 0x1549A966, 0x1F43B675, 0xA0, 0x75A1, 0xA6, 0x1654AE6B, 0xAE, 0xE0,
    0xE1, 0x6D80, 0x6240, 0x5035, 0x47E7, 0x55B0, 0x55D0, 0x1C53BB6B, 0xBB, 0xB7, 0x1043A770, 0x45B9, 0xB6, 0x80,
    0x1254C367, 0x7373, 0x63C0, 0x67C8,
];
const EBML_CLUSTER: u64 = 0x1F43B675;
const EBML_SEGMENT: u64 = 0x18538067;

/// One EBML variable-length integer: (value, encoded length, all value bits set).
fn vint(src: &mut Source, at: u64, max_len: u32, keep_marker: bool) -> Option<(u64, u64, bool)> {
    let first = src.exact::<1>(at)?[0];
    let len = first.leading_zeros() + 1;
    if first == 0 || len > max_len {
        return None;
    }
    let rest = src.read(at + 1, (len - 1) as usize)?;
    if rest.len() != (len - 1) as usize {
        return None;
    }
    let mask = if keep_marker { 0xFF } else { 0xFFu8.checked_shr(len).unwrap_or(0) };
    let value = rest.iter().fold(u64::from(first & mask), |acc, b| (acc << 8) | u64::from(*b));
    let all_ones = value == (1u64 << (7 * len)) - 1;
    Some((value, u64::from(len), all_ones))
}

fn ebml(src: &mut Source, start: u64, end: u64, depth: u32, budget: &mut u32) -> bool {
    if depth > MAX_DEPTH {
        return false;
    }
    let mut at = start;
    while at < end {
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        let Some((id, id_len, _)) = vint(src, at, 4, true) else { return false };
        let Some((size, size_len, unknown)) = vint(src, at + id_len, 8, false) else { return false };
        let data = at + id_len + size_len;
        if data > end {
            return false;
        }
        let master = EBML_MASTERS.contains(&id);
        let size = if unknown && master { end - data } else { size };
        if size > end - data {
            return false;
        }
        // Clusters hold the audio blocks, which symphonia only reads packet by
        // packet; their sizes are checked as the stream is read.
        if master && id != EBML_CLUSTER && !ebml(src, data, data + size, depth + 1, budget) {
            return false;
        }
        // symphonia reads one Segment; whatever trails it is never parsed.
        if (unknown && id == EBML_CLUSTER) || (depth == 0 && id == EBML_SEGMENT) {
            return true;
        }
        at = data + size;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut out = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        out
    }

    fn m4a(ilst: &[u8], stco_count: u32) -> Vec<u8> {
        let mut stco = vec![0u8; 4];
        stco.extend_from_slice(&stco_count.to_be_bytes());
        stco.extend_from_slice(&[0, 0, 0, 1]);
        let stbl = bx(b"stbl", &bx(b"stco", &stco));
        let trak = bx(b"trak", &bx(b"mdia", &[bx(b"hdlr", &[0u8; 25]), bx(b"minf", &stbl)].concat()));
        let meta = bx(b"meta", &[vec![0u8; 4], bx(b"ilst", ilst)].concat());
        let moov = bx(b"moov", &[trak, bx(b"udta", &meta)].concat());
        [bx(b"ftyp", b"M4A \0\0\0\0"), moov].concat()
    }

    fn title(body: &[u8]) -> Vec<u8> {
        bx(b"\xa9nam", &bx(b"data", body))
    }

    #[test]
    fn plain_files_and_well_formed_mp4_pass() {
        assert!(symphonia_safe(Source::Bytes(b"just some text, no container here at all")));
        assert!(symphonia_safe(Source::Bytes(&m4a(&title(b"\0\0\0\x01\0\0\0\0Song"), 1))));
    }

    #[test]
    fn impossible_mp4_lengths_are_refused() {
        assert!(!symphonia_safe(Source::Bytes(&m4a(&title(b"\0\0\0\x01\0\0\0\0Song"), u32::MAX))));
        assert!(!symphonia_safe(Source::Bytes(&m4a(&title(b"\0\0"), 1))));
        let mut huge_data = title(b"\0\0\0\x01\0\0\0\0Song");
        huge_data[8..12].copy_from_slice(&1u32.to_be_bytes());
        huge_data.splice(16..16, (1u64 << 40).to_be_bytes());
        assert!(!symphonia_safe(Source::Bytes(&m4a(&huge_data, 1))));
    }

    #[test]
    fn id3_prefix_is_skipped_like_the_probe() {
        let mut file = b"ID3\x04\0\0\0\0\0\x05".to_vec();
        file.extend_from_slice(b"\xff\xfb\0\0\0");
        file.extend(m4a(&[], u32::MAX));
        assert!(!symphonia_safe(Source::Bytes(&file)), "the ID3 body hides a sync marker, the MP4 follows");
        let mp3_first = [b"\xff\xfb\x90\x00".to_vec(), vec![0; 16], m4a(&[], u32::MAX)].concat();
        assert!(symphonia_safe(Source::Bytes(&mp3_first)), "an earlier MP3 frame wins the probe");
    }

    #[test]
    fn caf_and_matroska_lengths_are_checked() {
        let caf_ok = [b"caff\0\x01\0\0".to_vec(), b"kuki".to_vec(), 4i64.to_be_bytes().to_vec(), vec![1, 2, 3, 4]].concat();
        assert!(symphonia_safe(Source::Bytes(&caf_ok)));
        let caf_bad = [b"caff\0\x01\0\0".to_vec(), b"kuki".to_vec(), i64::MAX.to_be_bytes().to_vec(), vec![0; 16]].concat();
        assert!(!symphonia_safe(Source::Bytes(&caf_bad)));

        let ebml_header = [0x1A, 0x45, 0xDF, 0xA3, 0x80];
        let tag_string = [0x44, 0x87, 0x01, 0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFE, b'x'];
        let segment = [vec![0x18, 0x53, 0x80, 0x67, 0x80 | tag_string.len() as u8], tag_string.to_vec()].concat();
        let bad = [ebml_header.to_vec(), segment, vec![0; 16]].concat();
        assert!(!symphonia_safe(Source::Bytes(&bad)));
        let good = [ebml_header.to_vec(), vec![0x18, 0x53, 0x80, 0x67, 0x83, 0x44, 0x87, 0x80], vec![0; 16]].concat();
        assert!(symphonia_safe(Source::Bytes(&good)));
    }
}
