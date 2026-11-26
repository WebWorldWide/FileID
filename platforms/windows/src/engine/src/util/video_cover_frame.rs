//! A still image taken straight from a video container, for files the platform decoder cannot open: the cover
//! image attached to a Matroska or WebM file, and the first frame of a Motion-JPEG AVI.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use image::RgbImage;

use super::video_container_dates::element_at;

const MAX_EDGE: u32 = 1280;
const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_ELEMENTS: usize = 256;
const MAX_MOVIE_CHUNKS: usize = 256;

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const CLUSTER: u32 = 0x1F43_B675;
const ATTACHMENTS: u32 = 0x1941_A469;
const ATTACHED_FILE: u32 = 0x61A7;
const FILE_MIME_TYPE: u32 = 0x4660;
const FILE_DATA: u32 = 0x465C;

pub(crate) fn embedded_frame(path: &Path) -> Option<RgbImage> {
    let mut file = std::fs::File::open(crate::util::path_safety::to_extended_length(path)).ok()?;
    let length = file.metadata().ok()?.len();
    frame_from(&mut file, length)
}

fn frame_from<R: Read + Seek>(reader: &mut R, length: u64) -> Option<RgbImage> {
    let bytes = matroska_cover(reader, length).or_else(|| avi_first_frame(reader, length))?;
    let decoded = image::load_from_memory(&bytes).ok()?;
    let fitted = if decoded.width().max(decoded.height()) > MAX_EDGE { decoded.thumbnail(MAX_EDGE, MAX_EDGE) } else { decoded };
    Some(fitted.to_rgb8())
}

/// The first attached image of a Matroska or WebM file. The common muxers write attachments before the clusters,
/// so the search stops at the first cluster.
fn matroska_cover<R: Read + Seek>(reader: &mut R, length: u64) -> Option<Vec<u8>> {
    let (id, _, end) = element_at(reader, 0, length)?;
    if id != EBML {
        return None;
    }
    let mut at = end;
    for _ in 0..MAX_ELEMENTS {
        let (id, payload, end) = element_at(reader, at, length)?;
        if id == SEGMENT {
            return attached_image(reader, payload, end);
        }
        at = end;
    }
    None
}

fn attached_image<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<Vec<u8>> {
    let mut at = start;
    for _ in 0..MAX_ELEMENTS {
        let (id, payload, next) = element_at(reader, at, end)?;
        match id {
            CLUSTER => return None,
            ATTACHMENTS => return first_image_file(reader, payload, next),
            _ => at = next,
        }
    }
    None
}

fn first_image_file<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<Vec<u8>> {
    let mut at = start;
    for _ in 0..MAX_ELEMENTS {
        let (id, payload, next) = element_at(reader, at, end)?;
        if id == ATTACHED_FILE
            && let Some(image) = image_of_attached_file(reader, payload, next)
        {
            return Some(image);
        }
        at = next;
    }
    None
}

fn image_of_attached_file<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<Vec<u8>> {
    let mut is_image = false;
    let mut data = None;
    let mut at = start;
    for _ in 0..MAX_ELEMENTS {
        let Some((id, payload, next)) = element_at(reader, at, end) else { break };
        match id {
            FILE_MIME_TYPE => is_image = read_bytes(reader, payload, next)?.starts_with(b"image/"),
            FILE_DATA => data = Some((payload, next)),
            _ => {}
        }
        at = next;
    }
    let (payload, next) = data.filter(|_| is_image)?;
    read_bytes(reader, payload, next)
}

fn read_bytes<R: Read + Seek>(reader: &mut R, start: u64, end: u64) -> Option<Vec<u8>> {
    let size = end.checked_sub(start).filter(|size| *size <= MAX_IMAGE_BYTES)?;
    reader.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = vec![0u8; size as usize];
    reader.read_exact(&mut bytes).ok()?;
    Some(bytes)
}

/// The first video chunk of an AVI that holds a JPEG picture, which is every frame of a Motion-JPEG file.
fn avi_first_frame<R: Read + Seek>(reader: &mut R, length: u64) -> Option<Vec<u8>> {
    reader.seek(SeekFrom::Start(0)).ok()?;
    let mut head = [0u8; 12];
    reader.read_exact(&mut head).ok()?;
    if &head[..4] != b"RIFF" || &head[8..] != b"AVI " {
        return None;
    }
    let end = (u64::from(u32::from_le_bytes(head[4..8].try_into().ok()?)) + 8).min(length);
    let (mut at, movie_end) = list_contents(reader, 12, end, b"movi")?;
    for _ in 0..MAX_MOVIE_CHUNKS {
        let (kind, payload, size) = riff_chunk_at(reader, at, movie_end)?;
        if matches!(&kind[2..], b"dc" | b"db") {
            let bytes = read_bytes(reader, payload, payload + size)?;
            if bytes.starts_with(&[0xFF, 0xD8]) {
                return Some(bytes);
            }
        }
        at = payload + size + (size & 1);
    }
    None
}

fn list_contents<R: Read + Seek>(reader: &mut R, start: u64, end: u64, wanted: &[u8; 4]) -> Option<(u64, u64)> {
    let mut at = start;
    for _ in 0..MAX_ELEMENTS {
        let (kind, payload, size) = riff_chunk_at(reader, at, end)?;
        if &kind == b"LIST" && size >= 4 {
            let mut list_kind = [0u8; 4];
            reader.seek(SeekFrom::Start(payload)).ok()?;
            reader.read_exact(&mut list_kind).ok()?;
            if &list_kind == wanted {
                return Some((payload + 4, (payload + size).min(end)));
            }
        }
        at = payload + size + (size & 1);
    }
    None
}

/// The RIFF chunk at `at` inside a region ending at `limit`: its four-character code, where its payload starts and
/// its size. A chunk that runs past the region is cut to it.
fn riff_chunk_at<R: Read + Seek>(reader: &mut R, at: u64, limit: u64) -> Option<([u8; 4], u64, u64)> {
    let payload = at.checked_add(8).filter(|payload| *payload <= limit)?;
    reader.seek(SeekFrom::Start(at)).ok()?;
    let mut header = [0u8; 8];
    reader.read_exact(&mut header).ok()?;
    let size = u64::from(u32::from_le_bytes(header[4..].try_into().ok()?));
    Some((header[..4].try_into().ok()?, payload, size.min(limit - payload)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::video_container_dates::tests::{avi_with, list, riff_chunk};
    use std::io::Cursor;

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30])))
            .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        bytes
    }

    fn ebml(id: &[u8], payload: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.push(0x10 | (payload.len() >> 24) as u8);
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
        out.extend_from_slice(payload);
        out
    }

    fn matroska_with(attachments: Vec<u8>, clusters: Vec<u8>) -> Vec<u8> {
        let header = ebml(&[0x1A, 0x45, 0xDF, 0xA3], &ebml(&[0x42, 0x82], b"matroska"));
        let info = ebml(&[0x15, 0x49, 0xA9, 0x66], &ebml(&[0xEC], &[0; 3]));
        let segment = ebml(&[0x18, 0x53, 0x80, 0x67], &[info, attachments, clusters].concat());
        [header, segment].concat()
    }

    fn attached_file(mime: &[u8], data: &[u8]) -> Vec<u8> {
        ebml(&[0x19, 0x41, 0xA4, 0x69], &ebml(&[0x61, 0xA7], &[ebml(&[0x46, 0x60], mime), ebml(&[0x46, 0x5C], data)].concat()))
    }

    fn frame(bytes: &[u8]) -> Option<RgbImage> {
        frame_from(&mut Cursor::new(bytes), bytes.len() as u64)
    }

    #[test]
    fn a_matroska_cover_image_is_the_frame() {
        let bytes = matroska_with(attached_file(b"image/jpeg", &jpeg(64, 48)), Vec::new());
        let image = frame(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (64, 48));
        assert!(image.get_pixel(10, 10)[0] > 150);
    }

    #[test]
    fn a_matroska_attachment_that_is_not_an_image_gives_no_frame() {
        let bytes = matroska_with(attached_file(b"application/x-truetype-font", &jpeg(8, 8)), Vec::new());
        assert!(frame(&bytes).is_none());
    }

    #[test]
    fn attachments_after_the_first_cluster_are_not_searched() {
        let cluster = ebml(&[0x1F, 0x43, 0xB6, 0x75], &[0; 4]);
        let bytes = matroska_with(cluster, attached_file(b"image/jpeg", &jpeg(8, 8)));
        assert!(frame(&bytes).is_none());
    }

    #[test]
    fn a_large_cover_is_scaled_to_the_frame_limit() {
        let bytes = matroska_with(attached_file(b"image/jpeg", &jpeg(2560, 1280)), Vec::new());
        let image = frame(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (1280, 640));
    }

    #[test]
    fn an_mjpeg_avi_gives_its_first_jpeg_frame() {
        let header = list(b"hdrl", riff_chunk(b"avih", &[0; 56]));
        let audio = riff_chunk(b"01wb", &[1, 2, 3, 4]);
        let movi = list(b"movi", [audio, riff_chunk(b"00dc", &jpeg(32, 24))].concat());
        let bytes = avi_with([header, movi].concat());
        let image = frame(&bytes).unwrap();
        assert_eq!((image.width(), image.height()), (32, 24));
    }

    #[test]
    fn an_avi_with_other_video_data_gives_no_frame() {
        let movi = list(b"movi", riff_chunk(b"00dc", &[0, 0, 1, 0xB3, 0, 0, 0, 0]));
        assert!(frame(&avi_with(movi)).is_none());
    }

    #[test]
    fn files_that_are_neither_matroska_nor_avi_give_no_frame() {
        assert!(frame(b"\0\0\0\x18ftypmp42\0\0\0\0mp42isom").is_none());
        assert!(frame(&[]).is_none());
    }
}
