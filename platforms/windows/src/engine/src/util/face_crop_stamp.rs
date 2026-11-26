//! Marks a face crop JPEG as written by the upright alignment. Older crops can be sideways and were embedded that way;
//! the mark lives in the file, so copying a catalog or restoring a backup cannot make an old crop look current.

use std::io::Read;
use std::path::Path;

const TEXT: &[u8] = b"fileid-face-crop-upright-1";
const SOI: [u8; 2] = [0xFF, 0xD8];
const COM: [u8; 2] = [0xFF, 0xFE];

/// The JPEG with a comment segment after its start marker; bytes that are not a JPEG come back unchanged.
pub(crate) fn stamp(jpeg: &[u8]) -> Vec<u8> {
    if !jpeg.starts_with(&SOI) {
        return jpeg.to_vec();
    }
    let length = (TEXT.len() + 2) as u16;
    let mut stamped = Vec::with_capacity(jpeg.len() + TEXT.len() + 4);
    stamped.extend_from_slice(&SOI);
    stamped.extend_from_slice(&COM);
    stamped.extend_from_slice(&length.to_be_bytes());
    stamped.extend_from_slice(TEXT);
    stamped.extend_from_slice(&jpeg[2..]);
    stamped
}

pub(crate) fn is_stamped(path: &Path) -> bool {
    let mut head = [0u8; 6 + TEXT.len()];
    std::fs::File::open(path)
        .and_then(|mut file| file.read_exact(&mut head))
        .is_ok_and(|()| head[..2] == SOI && head[2..4] == COM && head[6..] == *TEXT)
}

#[cfg(test)]
mod tests {
    use super::*;

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 4, 1, 2, 0xFF, 0xD9];

    #[test]
    fn a_stamped_crop_is_recognised_and_keeps_its_image_data() {
        let directory = std::env::temp_dir().join(format!("fileid-crop-stamp-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let plain = directory.join("plain.jpg");
        let marked = directory.join("marked.jpg");
        std::fs::write(&plain, JPEG).unwrap();
        std::fs::write(&marked, stamp(JPEG)).unwrap();
        assert!(!is_stamped(&plain));
        assert!(is_stamped(&marked));
        assert!(!is_stamped(&directory.join("missing.jpg")));
        assert!(stamp(JPEG).ends_with(&JPEG[2..]));
        assert_eq!(stamp(b"not a jpeg"), b"not a jpeg");
        let _ = std::fs::remove_dir_all(&directory);
    }

    #[test]
    fn a_stamped_crop_still_decodes() {
        let mut encoded = Vec::new();
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode(&[200u8; 8 * 8 * 3], 8, 8, image::ExtendedColorType::Rgb8)
            .unwrap();
        let decoded = image::load_from_memory_with_format(&stamp(&encoded), image::ImageFormat::Jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (8, 8));
    }
}
