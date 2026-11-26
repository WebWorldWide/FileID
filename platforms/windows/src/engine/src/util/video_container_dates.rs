use std::io::{Read, Seek, SeekFrom};

const ASF_HEADER_GUID: [u8; 16] = [0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9, 0x00, 0xAA, 0x00, 0x62, 0xCE, 0x6C];
const ASF_FILE_PROPERTIES_GUID: [u8; 16] = [0xA1, 0xDC, 0xAB, 0x8C, 0x47, 0xA9, 0xCF, 0x11, 0x8E, 0xE4, 0x00, 0xC0, 0x0C, 0x20, 0x53, 0x65];
const SECONDS_1601_TO_1970: u64 = 11_644_473_600;
const SECONDS_1970_TO_2001: i64 = 978_307_200;
const MAX_ELEMENTS: usize = 256;
const MAX_TEXT_BYTES: u32 = 64;

/// The creation date in a Windows Media (ASF: wmv, wma, asf) header, as Unix seconds. The File Properties
/// object stores it as 100 ns ticks since 1601.
pub(crate) fn asf_creation<R: Read + Seek>(reader: &mut R, length: u64) -> Option<f64> {
    reader.seek(SeekFrom::Start(0)).ok()?;
    let mut head = [0u8; 30];
    reader.read_exact(&mut head).ok()?;
    if head[..16] != ASF_HEADER_GUID {
        return None;
    }
    let count = u32::from_le_bytes(head[24..28].try_into().ok()?).min(MAX_ELEMENTS as u32);
    let mut at = 30u64;
    for _ in 0..count {
        if at.checked_add(24)? > length {
            return None;
        }
        reader.seek(SeekFrom::Start(at)).ok()?;
        let mut object = [0u8; 24];
        reader.read_exact(&mut object).ok()?;
        let size = u64::from_le_bytes(object[16..].try_into().ok()?);
        if object[..16] == ASF_FILE_PROPERTIES_GUID {
            let mut properties = [0u8; 32];
            reader.read_exact(&mut properties).ok()?;
            let ticks = u64::from_le_bytes(properties[24..].try_into().ok()?);
            return (ticks / 10_000_000).checked_sub(SECONDS_1601_TO_1970).map(|unix| unix as f64);
        }
        if size < 24 {
            return None;
        }
        at = at.checked_add(size)?;
    }
    None
}

/// The recording date an AVI carries: the `IDIT` chunk (a C `ctime` string such as `Mon Mar 10 15:04:05 2008`) or
/// the `ICRD` entry of `LIST INFO` (`2008-03-10`, optionally with a time). Neither names a zone, so the clock time
/// is read in the machine's zone, as EXIF capture times without an offset are.
pub(crate) fn avi_creation<R: Read + Seek>(reader: &mut R, length: u64) -> Option<f64> {
    avi_creation_in(reader, length, &chrono::Local)
}

fn avi_creation_in<R: Read + Seek, Tz: chrono::TimeZone>(reader: &mut R, length: u64, zone: &Tz) -> Option<f64> {
    reader.seek(SeekFrom::Start(0)).ok()?;
    let mut head = [0u8; 12];
    reader.read_exact(&mut head).ok()?;
    if &head[..4] != b"RIFF" || &head[8..] != b"AVI " {
        return None;
    }
    let end = (u64::from(u32::from_le_bytes(head[4..8].try_into().ok()?)) + 8).min(length);
    let wall_clock = riff_dates(reader, 12, end, 0)?;
    Some(zone.from_local_datetime(&wall_clock).latest()?.timestamp() as f64)
}

fn riff_dates<R: Read + Seek>(reader: &mut R, start: u64, end: u64, depth: u8) -> Option<chrono::NaiveDateTime> {
    let mut at = start;
    for _ in 0..MAX_ELEMENTS {
        if at.checked_add(8)? > end {
            return None;
        }
        reader.seek(SeekFrom::Start(at)).ok()?;
        let mut header = [0u8; 8];
        reader.read_exact(&mut header).ok()?;
        let size = u32::from_le_bytes(header[4..].try_into().ok()?);
        let payload = at + 8;
        let next = payload.checked_add(u64::from(size) + u64::from(size & 1))?;
        match &header[..4] {
            b"LIST" if depth < 2 && size >= 4 => {
                let mut kind = [0u8; 4];
                reader.read_exact(&mut kind).ok()?;
                if matches!(&kind, b"hdrl" | b"INFO")
                    && let Some(found) = riff_dates(reader, payload + 4, next.min(end), depth + 1)
                {
                    return Some(found);
                }
            }
            b"IDIT" => {
                if let Some(found) = chunk_text(reader, payload, size).and_then(|text| ctime_date(&text)) {
                    return Some(found);
                }
            }
            b"ICRD" => {
                if let Some(found) = chunk_text(reader, payload, size).and_then(|text| info_date(&text)) {
                    return Some(found);
                }
            }
            _ => {}
        }
        at = next;
    }
    None
}

fn chunk_text<R: Read + Seek>(reader: &mut R, at: u64, size: u32) -> Option<String> {
    if size == 0 || size > MAX_TEXT_BYTES {
        return None;
    }
    reader.seek(SeekFrom::Start(at)).ok()?;
    let mut bytes = vec![0u8; size as usize];
    reader.read_exact(&mut bytes).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    Some(text.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string())
}

fn ctime_date(text: &str) -> Option<chrono::NaiveDateTime> {
    chrono::NaiveDateTime::parse_from_str(text, "%a %b %e %H:%M:%S %Y").ok()
}

fn info_date(text: &str) -> Option<chrono::NaiveDateTime> {
    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%dT%H:%M:%S"] {
        if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(text, format) {
            return Some(parsed);
        }
    }
    let day = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
    day.and_hms_opt(0, 0, 0)
}

/// The `DateUTC` of a Matroska or WebM segment (`Segment/Info`), which counts nanoseconds from 2001-01-01.
pub(crate) fn matroska_creation<R: Read + Seek>(reader: &mut R, length: u64) -> Option<f64> {
    const EBML: u32 = 0x1A45_DFA3;
    const SEGMENT: u32 = 0x1853_8067;
    const INFO: u32 = 0x1549_A966;
    const DATE_UTC: u32 = 0x4461;
    let (id, _, end) = element_at(reader, 0, length)?;
    if id != EBML {
        return None;
    }
    let mut at = end;
    for _ in 0..MAX_ELEMENTS {
        let (id, payload, end) = element_at(reader, at, length)?;
        if id == SEGMENT {
            return child_value(reader, payload, end, INFO, DATE_UTC);
        }
        at = end;
    }
    None
}

fn child_value<R: Read + Seek>(reader: &mut R, start: u64, end: u64, container: u32, wanted: u32) -> Option<f64> {
    let mut at = start;
    for _ in 0..MAX_ELEMENTS {
        let (id, payload, next) = element_at(reader, at, end)?;
        if id == container {
            return date_in(reader, payload, next, wanted);
        }
        at = next;
    }
    None
}

fn date_in<R: Read + Seek>(reader: &mut R, start: u64, end: u64, wanted: u32) -> Option<f64> {
    let mut at = start;
    for _ in 0..MAX_ELEMENTS {
        let (id, payload, next) = element_at(reader, at, end)?;
        if id == wanted {
            if next - payload != 8 {
                return None;
            }
            reader.seek(SeekFrom::Start(payload)).ok()?;
            let mut value = [0u8; 8];
            reader.read_exact(&mut value).ok()?;
            let nanoseconds = i64::from_be_bytes(value);
            return Some((SECONDS_1970_TO_2001 + nanoseconds.div_euclid(1_000_000_000)) as f64);
        }
        at = next;
    }
    None
}

/// The EBML element starting at `at` inside a region ending at `limit`: its ID, where its payload starts, where it
/// ends. An unknown size (all value bits set) runs to the end of the region.
pub(super) fn element_at<R: Read + Seek>(reader: &mut R, at: u64, limit: u64) -> Option<(u32, u64, u64)> {
    reader.seek(SeekFrom::Start(at)).ok()?;
    let mut first = [0u8; 1];
    reader.read_exact(&mut first).ok()?;
    let id_len = first[0].leading_zeros() as usize + 1;
    if id_len > 4 {
        return None;
    }
    let mut id = u32::from(first[0]);
    for _ in 1..id_len {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).ok()?;
        id = (id << 8) | u32::from(byte[0]);
    }
    reader.read_exact(&mut first).ok()?;
    let size_len = first[0].leading_zeros() as usize + 1;
    if size_len > 8 {
        return None;
    }
    let mut size = u64::from(first[0]) & (0xFF >> size_len);
    let mut unknown = size == (0xFF >> size_len);
    for _ in 1..size_len {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).ok()?;
        size = (size << 8) | u64::from(byte[0]);
        unknown &= byte[0] == 0xFF;
    }
    let payload = at.checked_add((id_len + size_len) as u64)?;
    let end = if unknown { limit } else { payload.checked_add(size)? };
    (end <= limit).then_some((id, payload, end))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::TimeZone;
    use std::io::Cursor;

    pub(crate) fn asf_with_creation(unix: u64) -> Vec<u8> {
        let mut properties = ASF_FILE_PROPERTIES_GUID.to_vec();
        properties.extend_from_slice(&104u64.to_le_bytes());
        properties.extend_from_slice(&[0; 16]);
        properties.extend_from_slice(&4096u64.to_le_bytes());
        properties.extend_from_slice(&((unix + SECONDS_1601_TO_1970) * 10_000_000).to_le_bytes());
        properties.extend_from_slice(&[0; 48]);
        let mut bytes = ASF_HEADER_GUID.to_vec();
        bytes.extend_from_slice(&(30 + properties.len() as u64).to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&[1, 2]);
        bytes.extend(properties);
        bytes
    }

    pub(crate) fn riff_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut out = kind.to_vec();
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
        if payload.len() % 2 == 1 {
            out.push(0);
        }
        out
    }

    pub(crate) fn avi_with(chunks: Vec<u8>) -> Vec<u8> {
        let movi = riff_chunk(b"LIST", &[b"movi".as_slice(), &[0; 64]].concat());
        let body = [b"AVI ".to_vec(), chunks, movi].concat();
        riff_chunk(b"RIFF", &body)
    }

    pub(crate) fn list(kind: &[u8; 4], children: Vec<u8>) -> Vec<u8> {
        riff_chunk(b"LIST", &[kind.as_slice(), &children].concat())
    }

    pub(crate) fn ebml_element(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.push(0x80 | payload.len() as u8);
        out.extend_from_slice(payload);
        out
    }

    fn matroska_with(nanoseconds_since_2001: i64) -> Vec<u8> {
        let header = ebml_element(&[0x1A, 0x45, 0xDF, 0xA3], &ebml_element(&[0x42, 0x82], b"matroska"));
        let date = ebml_element(&[0x44, 0x61], &nanoseconds_since_2001.to_be_bytes());
        let info = ebml_element(&[0x15, 0x49, 0xA9, 0x66], &[ebml_element(&[0xEC], &[0; 3]), date].concat());
        let segment = [vec![0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF], info].concat();
        [header, segment].concat()
    }

    fn asf(bytes: &[u8]) -> Option<f64> {
        asf_creation(&mut Cursor::new(bytes), bytes.len() as u64)
    }

    fn avi(bytes: &[u8]) -> Option<f64> {
        avi_creation(&mut Cursor::new(bytes), bytes.len() as u64)
    }

    fn avi_in(bytes: &[u8], zone: &chrono::FixedOffset) -> Option<f64> {
        avi_creation_in(&mut Cursor::new(bytes), bytes.len() as u64, zone)
    }

    fn matroska(bytes: &[u8]) -> Option<f64> {
        matroska_creation(&mut Cursor::new(bytes), bytes.len() as u64)
    }

    #[test]
    fn a_windows_media_header_gives_its_file_creation_date() {
        let bytes = asf_with_creation(1_245_000_000);
        assert_eq!(asf(&bytes), Some(1_245_000_000.0));
    }

    #[test]
    fn a_windows_media_header_cut_short_has_no_date() {
        let bytes = asf_with_creation(1_245_000_000);
        assert_eq!(asf(&bytes[..60]), None);
        assert_eq!(asf(b"not asf at all, only text here...."), None);
    }

    #[test]
    fn an_avi_gives_the_ctime_string_of_its_idit_chunk() {
        let header = list(b"hdrl", riff_chunk(b"avih", &[0; 56]));
        let bytes = avi_with([header, riff_chunk(b"IDIT", b"Mon Mar 10 15:04:05 2008\n\0")].concat());
        assert_eq!(avi_in(&bytes, &chrono::FixedOffset::east_opt(0).unwrap()), Some(1_205_161_445.0));
    }

    #[test]
    fn an_avi_clock_time_is_read_in_the_given_zone_not_as_utc() {
        let central = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        let bytes = avi_with(riff_chunk(b"IDIT", b"Mon Mar 10 15:04:05 2008\n\0"));
        assert_eq!(avi_in(&bytes, &central), Some(1_205_161_445.0 + 5.0 * 3600.0));
        let day_only = avi_with(list(b"INFO", riff_chunk(b"ICRD", b"2010-06-29\0\0")));
        assert_eq!(avi_in(&day_only, &central), Some(1_277_769_600.0 + 5.0 * 3600.0));
    }

    #[test]
    fn an_avi_gives_the_icrd_entry_of_its_info_list() {
        let info = list(b"INFO", riff_chunk(b"ICRD", b"2008-03-10\0\0"));
        assert_eq!(avi_in(&avi_with(info), &chrono::FixedOffset::east_opt(0).unwrap()), Some(1_205_107_200.0));
    }

    #[test]
    fn an_avi_without_a_readable_date_has_none() {
        let bytes = avi_with(riff_chunk(b"IDIT", b"garbage\0"));
        assert_eq!(avi(&bytes), None);
        assert_eq!(avi(&avi_with(Vec::new())), None);
    }

    #[test]
    fn a_matroska_date_counts_nanoseconds_from_2001() {
        let bytes = matroska_with(630_000_000 * 1_000_000_000);
        assert_eq!(matroska(&bytes), Some(1_608_307_200.0));
        assert_eq!(matroska(&matroska_with(-86_400 * 1_000_000_000)), Some(978_220_800.0));
    }

    #[test]
    fn a_matroska_file_without_a_date_or_cut_short_has_none() {
        let bytes = matroska_with(1);
        assert_eq!(matroska(&bytes[..bytes.len() - 6]), None);
        assert_eq!(matroska(b"RIFF....AVI "), None);
    }

    #[test]
    fn a_wmv_an_avi_and_an_mkv_file_each_give_their_container_date() {
        let directory = std::env::temp_dir().join(format!("fileid-container-dates-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        let date_of = |name: &str, bytes: Vec<u8>| {
            let path = directory.join(name);
            std::fs::write(&path, bytes).unwrap();
            crate::util::video_creation_date::container_creation_unix(&path)
        };
        let idit = avi_with(riff_chunk(b"IDIT", b"Mon Mar 10 15:04:05 2008\n\0"));
        assert_eq!(date_of("holiday.wmv", asf_with_creation(1_245_000_000)), Some(1_245_000_000.0));
        let wall_clock = chrono::NaiveDate::from_ymd_opt(2008, 3, 10).unwrap().and_hms_opt(15, 4, 5).unwrap();
        let local = chrono::Local.from_local_datetime(&wall_clock).latest().unwrap().timestamp() as f64;
        assert_eq!(date_of("holiday.avi", idit), Some(local));
        assert_eq!(date_of("holiday.mkv", matroska_with(630_000_000 * 1_000_000_000)), Some(1_608_307_200.0));
        assert_eq!(date_of("clock-reset.mkv", matroska_with(-900_000_000 * 1_000_000_000)), None, "a pre-1990 date is a reset clock");
        assert_eq!(date_of("holiday.mpg", vec![0, 0, 1, 0xBA, 0x44, 0, 4, 0, 4, 1, 0, 0, 3, 0xF8]), None);
        std::fs::remove_dir_all(directory).unwrap();
    }
}
