use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SECONDS_1904_TO_1970: u64 = 2_082_844_800;
const MIN_PLAUSIBLE_UNIX: f64 = 631_152_000.0;
const MAX_TOP_LEVEL_BOXES: usize = 4096;
const MAX_MOOV_CHILDREN: usize = 256;
const BOX_TYPES_AT_FILE_START: [&[u8; 4]; 8] = [b"ftyp", b"moov", b"mdat", b"wide", b"free", b"skip", b"pnot", b"junk"];

/// When an MP4, MOV, M4V or 3GP file was shot, as Unix seconds. Apple devices write the capture time to
/// `moov/meta` as `com.apple.quicktime.creationdate`; the movie header's creation time (`moov/mvhd`, ffprobe's
/// `creation_time`) is the export time there, so the Apple key wins and the header is the fallback. Other
/// containers, an unset clock (0), a pre-1990 reset or a future date yield None.
pub(crate) fn container_creation_unix(path: &Path) -> Option<f64> {
    let mut file = std::fs::File::open(crate::util::path_safety::to_extended_length(path)).ok()?;
    let length = file.metadata().ok()?.len();
    let (apple, header) = movie_dates(&mut file, length)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
    let plausible = |unix: &f64| *unix >= MIN_PLAUSIBLE_UNIX && *unix <= now + 86_400.0;
    apple.filter(plausible).or(header.filter(plausible))
}

const APPLE_CREATION_KEY: &[u8] = b"com.apple.quicktime.creationdate";

fn movie_dates<R: Read + Seek>(reader: &mut R, length: u64) -> Option<(Option<f64>, Option<f64>)> {
    let mut at = 0;
    for index in 0..MAX_TOP_LEVEL_BOXES {
        let (kind, payload, end) = box_at(reader, at, length)?;
        if index == 0 && !BOX_TYPES_AT_FILE_START.contains(&&kind) {
            return None;
        }
        if &kind == b"moov" {
            let header = mvhd_creation(reader, payload, end);
            let apple = apple_creation(reader, payload, end);
            return (header.is_some() || apple.is_some()).then_some((apple, header));
        }
        at = end;
    }
    None
}

fn mvhd_creation<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<f64> {
    let mut at = start;
    for _ in 0..MAX_MOOV_CHILDREN {
        let (kind, payload, next) = box_at(reader, at, end)?;
        if &kind == b"mvhd" {
            reader.seek(SeekFrom::Start(payload)).ok()?;
            let mut header = [0u8; 12];
            let version = {
                reader.read_exact(&mut header[..4]).ok()?;
                header[0]
            };
            let seconds = match version {
                0 => {
                    reader.read_exact(&mut header[..4]).ok()?;
                    u64::from(u32::from_be_bytes(header[..4].try_into().ok()?))
                }
                1 => {
                    reader.read_exact(&mut header[..8]).ok()?;
                    u64::from_be_bytes(header[..8].try_into().ok()?)
                }
                _ => return None,
            };
            return seconds.checked_sub(SECONDS_1904_TO_1970).map(|unix| unix as f64);
        }
        at = next;
    }
    None
}

fn apple_creation<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<f64> {
    let mut at = start;
    for _ in 0..MAX_MOOV_CHILDREN {
        let (kind, payload, next) = box_at(reader, at, end)?;
        if &kind == b"meta" {
            return meta_creation(reader, payload, next);
        }
        at = next;
    }
    None
}

fn meta_creation<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<f64> {
    let is_full_box = box_at(reader, start, end).is_none_or(|(kind, ..)| !matches!(&kind, b"hdlr" | b"keys" | b"ilst"));
    let mut at = if is_full_box { start.checked_add(4)? } else { start };
    let (mut keys, mut items) = (None, None);
    for _ in 0..MAX_MOOV_CHILDREN {
        let Some((kind, payload, next)) = box_at(reader, at, end) else { break };
        match &kind {
            b"keys" => keys = Some((payload, next)),
            b"ilst" => items = Some((payload, next)),
            _ => {}
        }
        at = next;
    }
    let (keys, items) = (keys?, items?);
    let wanted = key_index(reader, keys.0, keys.1)?;
    let mut at = items.0;
    for _ in 0..MAX_MOOV_CHILDREN {
        let (kind, payload, next) = box_at(reader, at, items.1)?;
        if u32::from_be_bytes(kind) == wanted {
            return item_text(reader, payload, next).and_then(|text| parse_apple_date(&text));
        }
        at = next;
    }
    None
}

fn key_index<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<u32> {
    reader.seek(SeekFrom::Start(start)).ok()?;
    let mut head = [0u8; 8];
    reader.read_exact(&mut head).ok()?;
    let count = u32::from_be_bytes(head[4..].try_into().ok()?).min(MAX_MOOV_CHILDREN as u32);
    let mut at = start + 8;
    for index in 1..=count {
        let (_, payload, next) = box_at(reader, at, end)?;
        let length = usize::try_from(next - payload).ok().filter(|len| *len <= 256)?;
        reader.seek(SeekFrom::Start(payload)).ok()?;
        let mut name = vec![0u8; length];
        reader.read_exact(&mut name).ok()?;
        if name == APPLE_CREATION_KEY {
            return Some(index);
        }
        at = next;
    }
    None
}

fn item_text<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<String> {
    let (kind, payload, next) = box_at(reader, start, end)?;
    let length = usize::try_from(next.checked_sub(payload)?.checked_sub(8)?).ok().filter(|len| *len <= 128)?;
    if &kind != b"data" {
        return None;
    }
    reader.seek(SeekFrom::Start(payload + 8)).ok()?;
    let mut text = vec![0u8; length];
    reader.read_exact(&mut text).ok()?;
    String::from_utf8(text).ok().map(|text| text.trim_end_matches('\0').to_string())
}

fn parse_apple_date(text: &str) -> Option<f64> {
    let text = text.trim();
    let parsed = chrono::DateTime::parse_from_rfc3339(text)
        .or_else(|_| chrono::DateTime::parse_from_str(text, "%Y-%m-%dT%H:%M:%S%.f%z"))
        .ok()?;
    Some(parsed.timestamp() as f64)
}

/// The box starting at `at` inside a region ending at `limit`: its type, where its payload starts, where it ends.
fn box_at<R: Read + Seek>(reader: &mut R, at: u64, limit: u64) -> Option<([u8; 4], u64, u64)> {
    if at.checked_add(8)? > limit {
        return None;
    }
    reader.seek(SeekFrom::Start(at)).ok()?;
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).ok()?;
    let size = u64::from(u32::from_be_bytes(header[..4].try_into().ok()?));
    let kind: [u8; 4] = header[4..].try_into().ok()?;
    let (header_len, size) = match size {
        0 => (8, limit - at),
        1 => {
            let mut large = [0u8; 8];
            reader.read_exact(&mut large).ok()?;
            (16, u64::from_be_bytes(large))
        }
        size => (8, size),
    };
    let end = at.checked_add(size)?;
    (size >= header_len && end <= limit).then_some((kind, at + header_len, end))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    fn boxed(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        out.extend_from_slice(kind);
        out.extend_from_slice(payload);
        out
    }

    fn mvhd_v0(seconds_since_1904: u32) -> Vec<u8> {
        let mut payload = vec![0, 0, 0, 0];
        payload.extend_from_slice(&seconds_since_1904.to_be_bytes());
        payload.extend_from_slice(&[0; 92]);
        boxed(b"mvhd", &payload)
    }

    fn mvhd_v1(seconds_since_1904: u64) -> Vec<u8> {
        let mut payload = vec![1, 0, 0, 0];
        payload.extend_from_slice(&seconds_since_1904.to_be_bytes());
        payload.extend_from_slice(&[0; 100]);
        boxed(b"mvhd", &payload)
    }

    pub(crate) fn synthetic_mp4(creation_unix: u64) -> Vec<u8> {
        mp4(true, mvhd_v1(creation_unix + SECONDS_1904_TO_1970))
    }

    fn mp4(moov_first: bool, mvhd: Vec<u8>) -> Vec<u8> {
        let ftyp = boxed(b"ftyp", b"isom\0\0\0\0isom");
        let mdat = boxed(b"mdat", &[7; 4000]);
        let moov = boxed(b"moov", &[boxed(b"free", &[0; 12]), mvhd].concat());
        if moov_first { [ftyp, moov, mdat].concat() } else { [ftyp, mdat, moov].concat() }
    }

    fn read(bytes: &[u8]) -> Option<f64> {
        let (apple, header) = movie_dates(&mut Cursor::new(bytes), bytes.len() as u64)?;
        apple.or(header)
    }

    fn apple_meta(key: &[u8], value: &str, full_box: bool) -> Vec<u8> {
        let hdlr = boxed(b"hdlr", &[0; 24]);
        let mut entries = vec![0, 0, 0, 0, 0, 0, 0, 1];
        entries.extend(boxed(b"mdta", key));
        let keys = boxed(b"keys", &entries);
        let mut data = vec![0, 0, 0, 1, 0, 0, 0, 0];
        data.extend_from_slice(value.as_bytes());
        let ilst = boxed(b"ilst", &boxed(&1u32.to_be_bytes(), &boxed(b"data", &data)));
        let mut payload = if full_box { vec![0, 0, 0, 0] } else { vec![] };
        payload.extend([hdlr, keys, ilst].concat());
        boxed(b"meta", &payload)
    }

    fn mp4_with_meta(mvhd_unix: u64, meta: Vec<u8>) -> Vec<u8> {
        let ftyp = boxed(b"ftyp", b"qt  \0\0\0\0qt  ");
        let moov = boxed(b"moov", &[mvhd_v1(mvhd_unix + SECONDS_1904_TO_1970), meta].concat());
        [ftyp, moov, boxed(b"mdat", &[7; 64])].concat()
    }

    #[test]
    fn the_apple_capture_time_beats_the_movie_header_export_time() {
        let shot = 1_560_549_623.0;
        for full_box in [true, false] {
            let bytes = mp4_with_meta(1_560_704_690, apple_meta(APPLE_CREATION_KEY, "2019-06-14T15:00:23-0700", full_box));
            assert_eq!(read(&bytes), Some(shot), "full_box={full_box}");
        }
        for text in ["2019-06-14T22:00:23+00:00", "2019-06-14T22:00:23Z", "2019-06-14T22:00:23.250+0000"] {
            let bytes = mp4_with_meta(1_560_704_690, apple_meta(APPLE_CREATION_KEY, text, true));
            assert_eq!(read(&bytes).map(f64::floor), Some(shot), "{text}");
        }
    }

    #[test]
    fn another_metadata_key_or_a_bad_date_leaves_the_movie_header() {
        for meta in [apple_meta(b"com.apple.quicktime.model", "2019-06-14T15:00:23-0700", true), apple_meta(APPLE_CREATION_KEY, "yesterday", true)] {
            assert_eq!(read(&mp4_with_meta(1_560_704_690, meta)), Some(1_560_704_690.0));
        }
    }

    #[test]
    fn reads_the_movie_header_with_moov_before_or_after_the_media_data() {
        let creation = 1_791_128_655u64 + SECONDS_1904_TO_1970;
        for moov_first in [true, false] {
            let bytes = mp4(moov_first, mvhd_v0(creation as u32));
            assert_eq!(read(&bytes), Some(1_791_128_655.0), "moov_first={moov_first}");
        }
    }

    #[test]
    fn reads_the_64_bit_creation_time_of_a_version_1_header() {
        assert_eq!(read(&mp4(true, mvhd_v1(1_600_000_000 + SECONDS_1904_TO_1970))), Some(1_600_000_000.0));
    }

    #[test]
    fn an_unset_clock_is_not_a_date() {
        assert_eq!(read(&mp4(true, mvhd_v0(0))), None);
    }

    #[test]
    fn files_that_are_not_iso_media_have_no_date() {
        let riff = [b"RIFF".as_slice(), &[0x20, 0, 0, 0], b"AVI LIST", &[0; 64]].concat();
        assert_eq!(read(&riff), None);
        assert_eq!(read(&[]), None);
        assert_eq!(read(&boxed(b"ftyp", b"isom")), None);
    }

    #[test]
    fn a_box_that_runs_past_the_file_is_refused() {
        let mut bytes = mp4(false, mvhd_v0(3_800_000_000));
        bytes.truncate(bytes.len() - 20);
        assert_eq!(read(&bytes), None);
    }

    #[test]
    fn pre_1990_and_future_dates_are_dropped_at_the_file_level() {
        let directory = std::env::temp_dir().join(format!("fileid-video-dates-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let write = |name: &str, seconds: u64| {
            let path = directory.join(name);
            std::fs::write(&path, mp4(true, mvhd_v1(seconds + SECONDS_1904_TO_1970))).unwrap();
            path
        };
        assert_eq!(container_creation_unix(&write("ok.mp4", 1_791_128_655)), Some(1_791_128_655.0));
        assert_eq!(container_creation_unix(&write("old.mp4", 86_400)), None);
        assert_eq!(container_creation_unix(&write("future.mp4", 4_000_000_000)), None);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
