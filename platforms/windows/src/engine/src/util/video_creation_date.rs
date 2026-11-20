use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const SECONDS_1904_TO_1970: u64 = 2_082_844_800;
const MIN_PLAUSIBLE_UNIX: f64 = 631_152_000.0;
const MAX_TOP_LEVEL_BOXES: usize = 4096;
const MAX_MOOV_CHILDREN: usize = 256;
const BOX_TYPES_AT_FILE_START: [&[u8; 4]; 8] = [b"ftyp", b"moov", b"mdat", b"wide", b"free", b"skip", b"pnot", b"junk"];

/// The movie header's creation time (`moov/mvhd`) of an MP4, MOV, M4V or 3GP file as Unix seconds: the same value
/// ffprobe reports as `creation_time`. Other containers, an unset clock (0), a pre-1990 reset or a future date yield None.
pub(crate) fn container_creation_unix(path: &Path) -> Option<f64> {
    let mut file = std::fs::File::open(crate::util::path_safety::to_extended_length(path)).ok()?;
    let length = file.metadata().ok()?.len();
    let unix = movie_header_creation(&mut file, length)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs_f64();
    (unix >= MIN_PLAUSIBLE_UNIX && unix <= now + 86_400.0).then_some(unix)
}

fn movie_header_creation<R: Read + Seek>(reader: &mut R, length: u64) -> Option<f64> {
    let mut at = 0;
    for index in 0..MAX_TOP_LEVEL_BOXES {
        let (kind, payload, end) = box_at(reader, at, length)?;
        if index == 0 && !BOX_TYPES_AT_FILE_START.contains(&&kind) {
            return None;
        }
        if &kind == b"moov" {
            return mvhd_creation(reader, payload, end);
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
        movie_header_creation(&mut Cursor::new(bytes), bytes.len() as u64)
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
