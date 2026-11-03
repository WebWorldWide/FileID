//! Largest embedded JPEG preview in a camera RAW file.
//!
//! Sony ARW, Nikon NEF, Canon CR2, Olympus ORF, Panasonic RW2, Pentax PEF and
//! DNG are TIFF containers that carry a camera-rendered JPEG (often full size);
//! Fujifilm RAF points at one from its header and Canon CR3 keeps one in an
//! ISO-BMFF `PRVW` box. Decoding that JPEG is an order of magnitude cheaper
//! than demosaicing the sensor data, and on Linux it is the only way the scan
//! can read these files at all. Mirrors the macOS scan, which takes ImageIO's
//! embedded preview when it is large enough.

const RAW_EXTENSIONS: &[&str] = &[
    "arw", "srf", "sr2", "nef", "nrw", "cr2", "cr3", "orf", "pef", "dng", "raf", "rw2", "rwl", "3fr", "erf", "mef",
    "mos", "iiq", "srw",
];

pub fn is_raw_extension(ext: &str) -> bool {
    RAW_EXTENSIONS.contains(&ext.to_ascii_lowercase().as_str())
}

/// Byte range of the largest JPEG the file embeds, or None.
pub fn largest_embedded_jpeg(bytes: &[u8]) -> Option<&[u8]> {
    let mut candidates: Vec<(usize, usize)> = Vec::new();
    if bytes.starts_with(b"FUJIFILMCCD-RAW") && bytes.len() >= 92 {
        let offset = u32::from_be_bytes(bytes[84..88].try_into().ok()?) as usize;
        let length = u32::from_be_bytes(bytes[88..92].try_into().ok()?) as usize;
        candidates.push((offset, length));
    } else if is_bmff(bytes) {
        Bmff { bytes }.walk(0, bytes.len(), 0, &mut candidates, &mut None);
    } else {
        let tiff = Tiff::new(bytes)?;
        tiff.collect(&mut candidates);
    }
    candidates
        .into_iter()
        .filter(|&(offset, length)| {
            length > 2 && offset.checked_add(length).is_some_and(|end| end <= bytes.len())
                && is_viewable_jpeg(&bytes[offset..offset + length])
        })
        .max_by_key(|&(_, length)| length)
        .map(|(offset, length)| &bytes[offset..offset + length])
}

/// EXIF orientation (1-8) the RAW container records in IFD0, or None. Reads
/// ORF and RW2 headers, which the generic EXIF reader rejects, and the CR3
/// `CMT1` TIFF block.
pub fn orientation(bytes: &[u8]) -> Option<u16> {
    let value = if is_bmff(bytes) {
        let mut cmt1 = None;
        Bmff { bytes }.walk(0, bytes.len(), 0, &mut Vec::new(), &mut cmt1);
        let (start, end) = cmt1?;
        Tiff::new(&bytes[start..end])?.ifd0_value(0x0112)?
    } else {
        Tiff::new(bytes)?.ifd0_value(0x0112)?
    };
    u16::try_from(value).ok().filter(|v| (1..=8).contains(v))
}

fn is_bmff(bytes: &[u8]) -> bool {
    bytes.get(4..8) == Some(b"ftyp")
}

struct Bmff<'a> {
    bytes: &'a [u8],
}

impl Bmff<'_> {
    fn be32(&self, at: usize) -> Option<usize> {
        Some(u32::from_be_bytes(self.bytes.get(at..at + 4)?.try_into().ok()?) as usize)
    }

    /// Boxes in `[start, end)` as (payload start, box end, fourcc).
    fn boxes(&self, start: usize, end: usize) -> Vec<(usize, usize, [u8; 4])> {
        let mut out = Vec::new();
        let mut at = start;
        while at + 8 <= end && out.len() < 256 {
            let (Some(size), Some(name)) = (self.be32(at), self.bytes.get(at + 4..at + 8)) else { break };
            let name: [u8; 4] = name.try_into().unwrap_or_default();
            let (header, size) = match size {
                0 => (8, end - at),
                1 => {
                    let Some(large) = self.bytes.get(at + 8..at + 16) else { break };
                    let large = u64::from_be_bytes(large.try_into().unwrap_or_default());
                    (16, usize::try_from(large).unwrap_or(usize::MAX))
                }
                n => (8, n),
            };
            let Some(box_end) = at.checked_add(size).filter(|e| size >= header && *e <= end) else { break };
            out.push((at + header, box_end, name));
            at = box_end;
        }
        out
    }

    fn walk(&self, start: usize, end: usize, depth: usize, out: &mut Vec<(usize, usize)>, cmt1: &mut Option<(usize, usize)>) {
        if depth > 4 {
            return;
        }
        for (payload, box_end, name) in self.boxes(start, end) {
            match &name {
                b"moov" => self.walk(payload, box_end, depth + 1, out, cmt1),
                // Canon's metadata uuid lists its children right after the
                // 16-byte uuid; the preview uuid has 8 more header bytes.
                b"uuid" => {
                    if let Some(skip) = [16, 24].into_iter().find(|skip| {
                        self.boxes(payload + skip, box_end)
                            .first()
                            .is_some_and(|(_, _, child)| child.iter().all(u8::is_ascii_alphanumeric))
                    }) {
                        self.walk(payload + skip, box_end, depth + 1, out, cmt1);
                    }
                }
                b"PRVW" | b"THMB" => {
                    let window = self.bytes.get(payload..box_end.min(payload + 64)).unwrap_or_default();
                    if let Some(soi) = window.windows(3).position(|w| w == [0xFF, 0xD8, 0xFF]) {
                        out.push((payload + soi, box_end - payload - soi));
                    }
                }
                b"CMT1" => *cmt1 = Some((payload, box_end)),
                _ => {}
            }
        }
    }
}

/// Baseline or progressive JPEG. Raw sensor data is often stored as lossless
/// JPEG (SOF3), which starts with the same marker but is not a picture an
/// ordinary decoder can show.
fn is_viewable_jpeg(jpeg: &[u8]) -> bool {
    if jpeg.get(0..2) != Some(&[0xFF, 0xD8]) {
        return false;
    }
    let mut at = 2;
    while at + 4 <= jpeg.len() {
        if jpeg[at] != 0xFF {
            return false;
        }
        let marker = jpeg[at + 1];
        match marker {
            0xC0..=0xC2 => return true,
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF | 0xDA | 0xD9 => return false,
            0xFF => {
                at += 1;
                continue;
            }
            _ => {}
        }
        let length = u16::from_be_bytes([jpeg[at + 2], jpeg[at + 3]]) as usize;
        at += 2 + length;
    }
    false
}

struct Tiff<'a> {
    bytes: &'a [u8],
    little: bool,
}

impl<'a> Tiff<'a> {
    fn new(bytes: &'a [u8]) -> Option<Self> {
        let little = match bytes.get(0..2)? {
            b"II" => true,
            b"MM" => false,
            _ => return None,
        };
        let tiff = Self { bytes, little };
        let magic = tiff.u16(2)?;
        // 42 = TIFF; ORF uses 0x4F52 / 0x5352 and RW2 0x55 with TIFF-style IFDs.
        matches!(magic, 42 | 0x4F52 | 0x5352 | 0x55).then_some(tiff)
    }

    fn u16(&self, at: usize) -> Option<u16> {
        let b: [u8; 2] = self.bytes.get(at..at + 2)?.try_into().ok()?;
        Some(if self.little { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }

    fn u32(&self, at: usize) -> Option<u32> {
        let b: [u8; 4] = self.bytes.get(at..at + 4)?.try_into().ok()?;
        Some(if self.little { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }

    fn ifd0_value(&self, wanted: u16) -> Option<u32> {
        let ifd = self.u32(4)? as usize;
        let count = self.u16(ifd)?;
        (0..count as usize).find_map(|i| {
            let entry = ifd + 2 + i * 12;
            if self.u16(entry)? != wanted {
                return None;
            }
            match (self.u16(entry + 2)?, self.u32(entry + 4)?) {
                (3, 1) => self.u16(entry + 8).map(u32::from),
                (4 | 13, 1) => self.u32(entry + 8),
                _ => None,
            }
        })
    }

    fn collect(&self, out: &mut Vec<(usize, usize)>) {
        const MAX_IFDS: usize = 64;
        let mut queue = vec![self.u32(4).unwrap_or(0) as usize];
        let mut seen = std::collections::HashSet::new();
        // Pushes are bounded too: one crafted IFD can name a million children.
        let push = |queue: &mut Vec<usize>, ifd: usize| {
            if queue.len() < MAX_IFDS {
                queue.push(ifd);
            }
        };
        while let Some(ifd) = queue.pop() {
            if seen.len() >= MAX_IFDS {
                break;
            }
            if ifd == 0 || !seen.insert(ifd) {
                continue;
            }
            let Some(count) = self.u16(ifd) else { continue };
            let mut jpeg_offset = None;
            let mut jpeg_length = None;
            let mut strip_offset = None;
            let mut strip_length = None;
            let mut compression = 0u32;
            for i in 0..count as usize {
                let entry = ifd + 2 + i * 12;
                let (Some(tag), Some(kind), Some(n)) = (self.u16(entry), self.u16(entry + 2), self.u32(entry + 4)) else {
                    break;
                };
                let value = match kind {
                    3 if n == 1 => self.u16(entry + 8).map(u32::from),
                    4 | 13 if n == 1 => self.u32(entry + 8),
                    _ => None,
                };
                match tag {
                    0x002E if kind == 7 => {
                        if let Some(offset) = self.u32(entry + 8) {
                            out.push((offset as usize, n as usize));
                        }
                    }
                    0x0201 => jpeg_offset = value,
                    0x0202 => jpeg_length = value,
                    0x0103 => compression = value.unwrap_or(0),
                    0x0111 => strip_offset = value,
                    0x0117 => strip_length = value,
                    0x014A | 0x8769 => {
                        if n == 1 {
                            if let Some(v) = value {
                                push(&mut queue, v as usize);
                            }
                        } else if kind == 4 || kind == 13 {
                            let at = self.u32(entry + 8).unwrap_or(0) as usize;
                            for k in 0..n.min(16) as usize {
                                if let Some(v) = self.u32(at.saturating_add(k * 4)) {
                                    push(&mut queue, v as usize);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            if let (Some(o), Some(l)) = (jpeg_offset, jpeg_length) {
                out.push((o as usize, l as usize));
            }
            if matches!(compression, 6 | 7 | 99) {
                if let (Some(o), Some(l)) = (strip_offset, strip_length) {
                    out.push((o as usize, l as usize));
                }
            }
            if let Some(next) = self.u32(ifd + 2 + count as usize * 12) {
                push(&mut queue, next as usize);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb([200, 30, 30]));
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Jpeg).unwrap();
        out.into_inner()
    }

    /// Little-endian TIFF: IFD0 with a small JPEG thumbnail, a SubIFD with a
    /// larger JPEG preview, like Sony/Nikon files.
    fn fake_raw(small: &[u8], large: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"II");
        b.extend_from_slice(&42u16.to_le_bytes());
        b.extend_from_slice(&8u32.to_le_bytes());
        let ifd0 = 8usize;
        let sub = ifd0 + 2 + 3 * 12 + 4;
        let small_at = sub + 2 + 2 * 12 + 4;
        let large_at = small_at + small.len();
        let entry = |b: &mut Vec<u8>, tag: u16, kind: u16, value: u32| {
            b.extend_from_slice(&tag.to_le_bytes());
            b.extend_from_slice(&kind.to_le_bytes());
            b.extend_from_slice(&1u32.to_le_bytes());
            b.extend_from_slice(&value.to_le_bytes());
        };
        b.extend_from_slice(&3u16.to_le_bytes());
        entry(&mut b, 0x014A, 4, sub as u32);
        entry(&mut b, 0x0201, 4, small_at as u32);
        entry(&mut b, 0x0202, 4, small.len() as u32);
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(&2u16.to_le_bytes());
        entry(&mut b, 0x0201, 4, large_at as u32);
        entry(&mut b, 0x0202, 4, large.len() as u32);
        b.extend_from_slice(&0u32.to_le_bytes());
        b.extend_from_slice(small);
        b.extend_from_slice(large);
        b
    }

    #[test]
    fn picks_the_largest_embedded_jpeg() {
        let small = jpeg(160, 120);
        let large = jpeg(1616, 1080);
        let raw = fake_raw(&small, &large);
        let found = largest_embedded_jpeg(&raw).expect("preview");
        assert_eq!(found.len(), large.len());
        let img = image::load_from_memory_with_format(found, image::ImageFormat::Jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (1616, 1080));
    }

    #[test]
    fn rejects_non_raw_and_bad_offsets() {
        assert!(largest_embedded_jpeg(b"not a raw file at all").is_none());
        let small = jpeg(16, 16);
        let mut raw = fake_raw(&small, &jpeg(32, 32));
        raw.truncate(raw.len() - 10);
        assert_eq!(largest_embedded_jpeg(&raw).map(<[u8]>::len), Some(small.len()), "truncated preview is skipped");
        assert!(is_raw_extension("ARW") && is_raw_extension("nef") && !is_raw_extension("jpg"));
        let lossless = [0xFF, 0xD8, 0xFF, 0xC3, 0x00, 0x0B, 8, 0, 16, 0, 16, 1, 1, 0x11, 0];
        assert!(!is_viewable_jpeg(&lossless), "lossless sensor data is not a preview");
        assert!(is_viewable_jpeg(&jpeg(8, 8)));
    }

    fn ifd_entry(b: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: u32) {
        b.extend_from_slice(&tag.to_le_bytes());
        b.extend_from_slice(&kind.to_le_bytes());
        b.extend_from_slice(&count.to_le_bytes());
        b.extend_from_slice(&value.to_le_bytes());
    }

    /// Little-endian TIFF-style header with `magic`, IFD0 holding orientation 6
    /// and, when given, a JpgFromRaw (0x002E, UNDEFINED) preview like Panasonic.
    fn tiff_with(magic: u16, jpg_from_raw: Option<&[u8]>) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(b"II");
        b.extend_from_slice(&magic.to_le_bytes());
        b.extend_from_slice(&8u32.to_le_bytes());
        let entries = 1 + u16::from(jpg_from_raw.is_some());
        let data_at = 8 + 2 + usize::from(entries) * 12 + 4;
        b.extend_from_slice(&entries.to_le_bytes());
        if let Some(jpeg) = jpg_from_raw {
            ifd_entry(&mut b, 0x002E, 7, jpeg.len() as u32, data_at as u32);
        }
        b.extend_from_slice(&0x0112u16.to_le_bytes());
        b.extend_from_slice(&3u16.to_le_bytes());
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&[6, 0, 0, 0]);
        b.extend_from_slice(&0u32.to_le_bytes());
        if let Some(jpeg) = jpg_from_raw {
            b.extend_from_slice(jpeg);
        }
        b
    }

    fn bmff_box(name: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut b = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        b.extend_from_slice(name);
        b.extend_from_slice(payload);
        b
    }

    /// Canon CR3 layout: moov → Canon uuid (CMT1 TIFF + THMB thumbnail), then
    /// the preview uuid holding PRVW, then mdat.
    fn fake_cr3(thumb: &[u8], preview: &[u8]) -> Vec<u8> {
        let mut canon = vec![0x85, 0xc0, 0xb6, 0x87, 0x82, 0x0f, 0x11, 0xe0, 0x81, 0x11, 0xf4, 0xce, 0x46, 0x2b, 0x6a, 0x48];
        canon.extend(bmff_box(b"CMT1", &tiff_with(42, None)));
        let mut thmb = vec![0u8; 16];
        thmb.extend_from_slice(thumb);
        canon.extend(bmff_box(b"THMB", &thmb));
        let mut prvw_uuid = vec![0xea, 0xf4, 0x2b, 0x5e, 0x1c, 0x98, 0x4b, 0x88, 0xb9, 0xfb, 0xb7, 0xdc, 0x40, 0x6e, 0x4d, 0x16];
        prvw_uuid.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]);
        let mut prvw = vec![0u8; 16];
        prvw.extend_from_slice(preview);
        prvw_uuid.extend(bmff_box(b"PRVW", &prvw));
        let mut out = bmff_box(b"ftyp", b"crx \0\0\0\x01crx isom");
        out.extend(bmff_box(b"moov", &bmff_box(b"uuid", &canon)));
        out.extend(bmff_box(b"uuid", &prvw_uuid));
        out.extend(bmff_box(b"mdat", &[0u8; 32]));
        out
    }

    #[test]
    fn panasonic_jpg_from_raw_is_found() {
        let preview = jpeg(1920, 1080);
        let rw2 = tiff_with(0x55, Some(&preview));
        assert_eq!(largest_embedded_jpeg(&rw2).map(<[u8]>::len), Some(preview.len()));
        assert_eq!(orientation(&rw2), Some(6));
        assert!(is_raw_extension("RW2") && is_raw_extension("rwl"));
    }

    #[test]
    fn canon_cr3_preview_and_orientation_are_found() {
        let thumb = jpeg(160, 120);
        let preview = jpeg(1620, 1080);
        let cr3 = fake_cr3(&thumb, &preview);
        let found = largest_embedded_jpeg(&cr3).expect("PRVW preview");
        let img = image::load_from_memory_with_format(found, image::ImageFormat::Jpeg).unwrap();
        assert_eq!((img.width(), img.height()), (1620, 1080));
        assert_eq!(orientation(&cr3), Some(6));
        assert!(is_raw_extension("CR3"));
    }

    #[test]
    fn olympus_header_orientation_is_read() {
        assert_eq!(orientation(&tiff_with(0x4F52, None)), Some(6));
        assert_eq!(orientation(b"FUJIFILMCCD-RAW 0201"), None);
        assert_eq!(orientation(&fake_raw(&jpeg(8, 8), &jpeg(16, 16))), None);
    }

    /// `FILEID_RAW_SAMPLE=/path/to/file.ARW cargo test raw_preview -- --ignored`
    #[test]
    #[ignore = "needs a real camera RAW file in FILEID_RAW_SAMPLE"]
    fn real_raw_sample() {
        let path = std::env::var("FILEID_RAW_SAMPLE").expect("FILEID_RAW_SAMPLE");
        let bytes = std::fs::read(&path).unwrap();
        let start = std::time::Instant::now();
        let found = largest_embedded_jpeg(&bytes).expect("preview");
        let img = image::load_from_memory_with_format(found, image::ImageFormat::Jpeg).expect("decodable preview");
        eprintln!("{}x{} ({} KB) in {:?}", img.width(), img.height(), found.len() / 1024, start.elapsed());
        assert!(img.width().max(img.height()) >= 1024);
    }
}
