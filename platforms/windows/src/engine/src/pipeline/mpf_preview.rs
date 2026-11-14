//! The large preview a camera stores inside its JPEGs as a CIPA DC-007 (MPF)
//! "large thumbnail" (Sony 1616x1080; Nikon and Fujifilm similar). Decoding it
//! instead of the 24-60 MP frame skips the entropy decode that bounds camera
//! JPEG scans; the Mac engine's `JPEGPreview.swift` carries the same rules
//! and measured the outputs against full decodes (DECISIONS 2026-11-10).

const LARGE_THUMBNAIL_TYPES: [u32; 2] = [0x0001_0001, 0x0001_0002];
pub const MAX_PREVIEW_BYTES: usize = 8 << 20;

/// The largest MPF large-thumbnail JPEG inside a JPEG file, as a slice of
/// `bytes`. Walks the marker segments before the first scan only.
pub fn largest_preview(bytes: &[u8]) -> Option<&[u8]> {
    if bytes.len() < 4 || bytes[0] != 0xFF || bytes[1] != 0xD8 {
        return None;
    }
    let mut position = 2usize;
    for _ in 0..64 {
        let header = bytes.get(position..position + 4)?;
        if header[0] != 0xFF {
            return None;
        }
        let marker = header[1];
        if marker == 0xDA || marker == 0xD9 {
            return None;
        }
        let length = usize::from(u16::from_be_bytes([header[2], header[3]]));
        if length < 2 {
            return None;
        }
        if marker == 0xE2 && length >= 16 {
            let payload = bytes.get(position + 4..position + 2 + length)?;
            if let Some(entries) = mpf_entries(payload, position + 4) {
                return entries
                    .into_iter()
                    .filter(|(offset, size)| {
                        *offset > 0 && *size > 0 && *size <= MAX_PREVIEW_BYTES && offset + size <= bytes.len()
                    })
                    .max_by_key(|(_, size)| *size)
                    .map(|(offset, size)| &bytes[offset..offset + size])
                    .filter(|jpeg| jpeg.starts_with(&[0xFF, 0xD8]));
            }
        }
        position += 2 + length;
    }
    None
}

/// `(absolute offset, size)` of each large-thumbnail entry in an APP2 payload
/// that starts with "MPF\0". MPF offsets count from its TIFF header.
fn mpf_entries(payload: &[u8], payload_offset: usize) -> Option<Vec<(usize, usize)>> {
    if payload.len() < 16 || &payload[..4] != b"MPF\0" {
        return None;
    }
    let tiff = &payload[4..];
    let little = match &tiff[..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b = tiff.get(at..at + 2)?;
        Some(if little { u16::from_le_bytes([b[0], b[1]]) } else { u16::from_be_bytes([b[0], b[1]]) })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b = tiff.get(at..at + 4)?;
        Some(if little {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        })
    };
    let ifd = u32_at(4)? as usize;
    let tag_count = usize::from(u16_at(ifd)?);
    let (mut count, mut list_offset) = (0usize, 0usize);
    for index in 0..tag_count {
        let tag = ifd + 2 + index * 12;
        let id = u16_at(tag)?;
        let value = u32_at(tag + 8)? as usize;
        match id {
            0xB001 => count = value,
            0xB002 => list_offset = value,
            _ => {}
        }
    }
    if count == 0 || count > 16 || list_offset == 0 {
        return None;
    }
    let mut entries = Vec::new();
    for index in 0..count {
        let at = list_offset + index * 16;
        let attribute = u32_at(at)?;
        let size = u32_at(at + 4)? as usize;
        let offset = u32_at(at + 8)? as usize;
        if !LARGE_THUMBNAIL_TYPES.contains(&(attribute & 0x00FF_FFFF)) || offset == 0 {
            continue;
        }
        entries.push((payload_offset + 4 + offset, size));
    }
    Some(entries)
}

/// The preview shows the whole frame: its aspect matches the full image
/// within one percent.
pub fn same_shape(width: u32, height: u32, primary_width: u32, primary_height: u32) -> bool {
    if width == 0 || height == 0 || primary_width == 0 || primary_height == 0 {
        return false;
    }
    let ratio = f64::from(width) / f64::from(height);
    let primary = f64::from(primary_width) / f64::from(primary_height);
    (ratio / primary - 1.0).abs() < 0.01
}

#[cfg(test)]
pub(crate) mod test_support {
    /// A JPEG file whose APP2 MPF index points at `preview` (appended after
    /// the primary's EOI as cameras do) and whose EXIF marks it sRGB.
    pub fn jpeg_with_mpf_preview(primary: &[u8], preview: &[u8], srgb: bool) -> Vec<u8> {
        assert!(primary.starts_with(&[0xFF, 0xD8]));
        let mut out = Vec::with_capacity(primary.len() + preview.len() + 256);
        out.extend_from_slice(&primary[..2]);
        // EXIF APP1: IFD0 -> ExifIFD -> ColorSpace.
        let mut exif = b"Exif\0\0".to_vec();
        let mut tiff: Vec<u8> = vec![0x49, 0x49, 0x2A, 0x00, 8, 0, 0, 0];
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0x8769u16.to_le_bytes());
        tiff.extend_from_slice(&4u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&26u32.to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        tiff.extend_from_slice(&1u16.to_le_bytes());
        tiff.extend_from_slice(&0xA001u16.to_le_bytes());
        tiff.extend_from_slice(&3u16.to_le_bytes());
        tiff.extend_from_slice(&1u32.to_le_bytes());
        tiff.extend_from_slice(&(if srgb { 1u32 } else { 0xFFFF }).to_le_bytes());
        tiff.extend_from_slice(&0u32.to_le_bytes());
        exif.extend_from_slice(&tiff);
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((exif.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&exif);
        // MPF APP2: version, count, entry list (primary then large thumbnail).
        let mut mpf = b"MPF\0".to_vec();
        let mut t: Vec<u8> = vec![0x49, 0x49, 0x2A, 0x00, 8, 0, 0, 0];
        t.extend_from_slice(&3u16.to_le_bytes());
        t.extend_from_slice(&0xB000u16.to_le_bytes());
        t.extend_from_slice(&7u16.to_le_bytes());
        t.extend_from_slice(&4u32.to_le_bytes());
        t.extend_from_slice(b"0100");
        t.extend_from_slice(&0xB001u16.to_le_bytes());
        t.extend_from_slice(&4u16.to_le_bytes());
        t.extend_from_slice(&1u32.to_le_bytes());
        t.extend_from_slice(&2u32.to_le_bytes());
        t.extend_from_slice(&0xB002u16.to_le_bytes());
        t.extend_from_slice(&7u16.to_le_bytes());
        t.extend_from_slice(&32u32.to_le_bytes());
        let list_offset = 8 + 2 + 3 * 12 + 4;
        t.extend_from_slice(&(list_offset as u32).to_le_bytes());
        t.extend_from_slice(&0u32.to_le_bytes());
        let segment_payload_len = mpf.len() + t.len() + 32;
        let app2_start = out.len();
        let tiff_start = app2_start + 4 + 4;
        let preview_offset = app2_start + 4 + segment_payload_len + (primary.len() - 2);
        for (attribute, size, offset) in [
            (0x0003_0000u32, primary.len() as u32, 0u32),
            (0x0001_0002u32, preview.len() as u32, (preview_offset - tiff_start) as u32),
        ] {
            t.extend_from_slice(&attribute.to_le_bytes());
            t.extend_from_slice(&size.to_le_bytes());
            t.extend_from_slice(&offset.to_le_bytes());
            t.extend_from_slice(&[0, 0, 0, 0]);
        }
        mpf.extend_from_slice(&t);
        out.extend_from_slice(&[0xFF, 0xE2]);
        out.extend_from_slice(&((mpf.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&mpf);
        out.extend_from_slice(&primary[2..]);
        assert_eq!(out.len(), preview_offset);
        out.extend_from_slice(preview);
        out
    }

    pub fn solid_jpeg(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
        let img = image::RgbImage::from_pixel(width, height, image::Rgb(rgb));
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Jpeg)
            .unwrap();
        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_large_thumbnail_behind_the_primary() {
        let primary = test_support::solid_jpeg(60, 40, [200, 30, 30]);
        let preview = test_support::solid_jpeg(30, 20, [30, 200, 30]);
        let file = test_support::jpeg_with_mpf_preview(&primary, &preview, true);
        assert_eq!(largest_preview(&file), Some(preview.as_slice()));
        assert!(largest_preview(&primary).is_none(), "a plain JPEG has no MPF index");
        let mut truncated = file.clone();
        truncated.truncate(file.len() - 10);
        assert!(largest_preview(&truncated).is_none(), "an entry past the end of the file is ignored");
    }

    #[test]
    fn shape_check_allows_sony_stretch_but_not_crops() {
        assert!(same_shape(1616, 1080, 7008, 4672));
        assert!(same_shape(1620, 1080, 6000, 4000));
        assert!(!same_shape(1080, 1080, 6000, 4000));
        assert!(!same_shape(0, 1080, 6000, 4000));
    }
}
