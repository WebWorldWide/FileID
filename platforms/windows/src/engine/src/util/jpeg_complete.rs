//! Tells a JPEG that ends before its picture does from one that is whole. ImageIO paints the missing part grey and
//! reports success, so a cut-off download or copy would otherwise be indexed as a normal photo.
//! Only the Mac-hosted scan asks.
#![cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]

/// True only for a JPEG (it starts with the SOI marker) whose data stops before the end-of-image marker.
/// Anything else, and a JPEG with extra data after its picture (a motion-photo video, multi-picture frames), is false.
pub fn is_cut_off(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || bytes[..2] != [0xFF, 0xD8] {
        return false;
    }
    let end = bytes.iter().rposition(|&byte| byte != 0).map_or(0, |last| last + 1);
    if end >= 2 && bytes[end - 2..end] == [0xFF, 0xD9] {
        return false;
    }
    !reaches_end_of_image(bytes)
}

fn reaches_end_of_image(bytes: &[u8]) -> bool {
    let mut at = 2;
    loop {
        while bytes.get(at) == Some(&0xFF) && bytes.get(at + 1) == Some(&0xFF) {
            at += 1;
        }
        let (Some(0xFF), Some(&marker)) = (bytes.get(at), bytes.get(at + 1)) else { return false };
        match marker {
            0xD9 => return true,
            0x01 | 0xD0..=0xD8 => at += 2,
            _ => {
                let (Some(&high), Some(&low)) = (bytes.get(at + 2), bytes.get(at + 3)) else { return false };
                let length = usize::from(u16::from_be_bytes([high, low]));
                if length < 2 || at + 2 + length > bytes.len() {
                    return false;
                }
                at += 2 + length;
                if marker == 0xDA {
                    let Some(next) = next_marker(bytes, at) else { return false };
                    at = next;
                }
            }
        }
    }
}

/// The offset of the next real marker after entropy-coded data: not a stuffed 0xFF00, a restart or fill byte.
fn next_marker(bytes: &[u8], from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(offset) = bytes[at..].iter().position(|&byte| byte == 0xFF) {
        at += offset;
        match bytes.get(at + 1)? {
            0x00 | 0xD0..=0xD7 | 0xFF => at += if bytes[at + 1] == 0xFF { 1 } else { 2 },
            _ => return Some(at),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jpeg(entropy: &[u8], ending: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE1, 0x00, 0x06, 1, 2, 3, 4, 0xFF, 0xDA, 0x00, 0x04, 9, 9];
        bytes.extend_from_slice(entropy);
        bytes.extend_from_slice(ending);
        bytes
    }

    #[test]
    fn a_picture_that_ends_with_its_end_marker_is_whole() {
        assert!(!is_cut_off(&jpeg(&[0x12, 0xFF, 0x00, 0x34], &[0xFF, 0xD9])));
        assert!(!is_cut_off(&jpeg(&[0x12, 0xFF, 0xD3, 0x34], &[0xFF, 0xD9, 0, 0, 0, 0])), "padding after the end marker");
    }

    #[test]
    fn extra_data_after_the_end_marker_is_not_a_cut() {
        assert!(!is_cut_off(&jpeg(&[0x12, 0x34], &[0xFF, 0xD9, b'm', b'p', b'4', 0xFF, 0x01])));
    }

    #[test]
    fn a_picture_that_stops_inside_its_data_or_a_segment_is_cut_off() {
        let whole = jpeg(&[0x12, 0xFF, 0x00, 0x34, 0x56], &[0xFF, 0xD9]);
        for length in 4..whole.len() - 1 {
            assert!(is_cut_off(&whole[..length]), "cut at {length}");
        }
        assert!(is_cut_off(&jpeg(&[0x12, 0x34, 0xFF], &[])));
    }

    #[test]
    fn other_files_are_never_called_cut_off() {
        assert!(!is_cut_off(b"\x89PNG\r\n\x1a\n"));
        assert!(!is_cut_off(&[]));
        assert!(!is_cut_off(&[0xFF, 0xD8]));
    }
}
