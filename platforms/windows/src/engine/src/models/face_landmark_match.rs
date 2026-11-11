//! Which detected landmark set belongs to a stored face box, as the macOS
//! engine decides it (`FaceLandmarkMatch.swift`, `FaceBBox.parseNormalized`,
//! `FaceClustering.parseBBox`). The Mac pipeline aligns a face only when one
//! Vision landmark detection overlaps its stored box clearly; a nearby,
//! displaced or ambiguous detection falls back to the padded box crop.
//!
//! Rectangles are normalized to the decoded image with a bottom-left origin,
//! like Vision's `boundingBox`.

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormRect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl NormRect {
    pub fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self { x, y, w, h }
    }

    fn mid_x(&self) -> f64 {
        self.x + self.w / 2.0
    }

    fn mid_y(&self) -> f64 {
        self.y + self.h / 2.0
    }

    fn valid(&self) -> bool {
        self.x.is_finite()
            && self.y.is_finite()
            && self.w.is_finite()
            && self.h.is_finite()
            && self.w > 0.0
            && self.h > 0.0
            && self.x >= 0.0
            && self.y >= 0.0
            && self.x + self.w <= 1.0
            && self.y + self.h <= 1.0
    }
}

/// `FaceLandmarkMatch.index`: the candidate covering `stored` with IoU ≥ 0.5 and
/// centres within 0.35 of the smaller box, unless a second such candidate comes
/// within 0.1 IoU of the best.
pub fn index(stored: &NormRect, candidates: &[NormRect]) -> Option<usize> {
    if !stored.valid() {
        return None;
    }
    let mut scores: Vec<(usize, f64)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, bounds)| {
            if !bounds.valid() {
                return None;
            }
            let left = stored.x.max(bounds.x);
            let bottom = stored.y.max(bounds.y);
            let right = (stored.x + stored.w).min(bounds.x + bounds.w);
            let top = (stored.y + stored.h).min(bounds.y + bounds.h);
            if right < left || top < bottom {
                return None;
            }
            let area = (right - left) * (top - bottom);
            let union = stored.w * stored.h + bounds.w * bounds.h - area;
            let overlap = area / union;
            let dx = (stored.mid_x() - bounds.mid_x()).abs() / stored.w.min(bounds.w);
            let dy = (stored.mid_y() - bounds.mid_y()).abs() / stored.h.min(bounds.h);
            (overlap >= 0.5 && dx <= 0.35 && dy <= 0.35).then_some((index, overlap))
        })
        .collect();
    scores.sort_by(|a, b| b.1.total_cmp(&a.1));
    let best = scores.first()?;
    if scores.len() > 1 && best.1 - scores[1].1 < 0.1 {
        return None;
    }
    Some(best.0)
}

/// `FaceBBox.parseNormalized`: a stored face box as a normalized bottom-left
/// rectangle on the upright decoded image. macOS stores Vision's CSV
/// `x,y,w,h`; the portable pipeline stores JSON pixels with a top-left origin
/// (rotated into the upright frame for EXIF orientations 2–8).
pub fn parse_normalized(text: &str, image_width: u32, image_height: u32, orientation: u8) -> Option<NormRect> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.starts_with('{') {
        return parse_json(text, image_width, image_height, orientation);
    }
    let fields: Vec<&str> = text.split(',').collect();
    if fields.len() < 4 {
        return None;
    }
    let parts: Vec<f64> = fields[..4].iter().filter_map(|field| swift_double(field)).collect();
    if parts.len() != 4 || !parts.iter().all(|value| value.is_finite()) || parts[2] <= 0.0 || parts[3] <= 0.0 {
        return None;
    }
    Some(NormRect::new(parts[0], parts[1], parts[2], parts[3]))
}

/// Swift's `Double(Substring)`: no surrounding whitespace, no leading `+`
/// quirks beyond what Rust's float grammar accepts.
fn swift_double(text: &str) -> Option<f64> {
    if text.is_empty() || text.trim() != text {
        return None;
    }
    text.parse::<f64>().ok()
}

fn parse_json(text: &str, image_width: u32, image_height: u32, orientation: u8) -> Option<NormRect> {
    if image_width == 0 || image_height == 0 {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let object = value.as_object()?;
    let number = |key: &str| object.get(key).and_then(serde_json::Value::as_f64);
    let (px, py, pw, ph) = (number("x")?, number("y")?, number("w")?, number("h")?);
    if ![px, py, pw, ph].iter().all(|value| value.is_finite()) || pw <= 0.0 || ph <= 0.0 {
        return None;
    }
    let has_source = object.contains_key("sourceWidth") || object.contains_key("sourceHeight");
    let (source_width, source_height) = if has_source {
        if object.get("coordinateSpace").and_then(serde_json::Value::as_str) != Some("pixel-top-left") {
            return None;
        }
        let (width, height) = (number("sourceWidth")?, number("sourceHeight")?);
        if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
            return None;
        }
        (width, height)
    } else {
        (f64::from(image_width), f64::from(image_height))
    };
    if !(1..=8).contains(&orientation) {
        return None;
    }
    if orientation != 1 && !object.contains_key("sourceWidth") {
        return None;
    }
    let (x, y, w, h) = (px / source_width, py / source_height, pw / source_width, ph / source_height);
    if orientation == 1 {
        return Some(NormRect::new(x, 1.0 - y - h, w, h));
    }
    let oriented = |x: f64, y: f64| -> (f64, f64) {
        match orientation {
            2 => (1.0 - x, y),
            3 => (1.0 - x, 1.0 - y),
            4 => (x, 1.0 - y),
            5 => (y, x),
            6 => (1.0 - y, x),
            7 => (1.0 - y, 1.0 - x),
            8 => (y, 1.0 - x),
            _ => (x, y),
        }
    };
    let corners = [oriented(x, y), oriented(x + w, y), oriented(x, y + h), oriented(x + w, y + h)];
    let min_x = corners.iter().map(|c| c.0).fold(f64::INFINITY, f64::min);
    let max_x = corners.iter().map(|c| c.0).fold(f64::NEG_INFINITY, f64::max);
    let min_y = corners.iter().map(|c| c.1).fold(f64::INFINITY, f64::min);
    let max_y = corners.iter().map(|c| c.1).fold(f64::NEG_INFINITY, f64::max);
    Some(NormRect::new(min_x, 1.0 - max_y, max_x - min_x, max_y - min_y))
}

/// Pixel rectangle (top-left origin, integral) of the padded crop the macOS
/// engine embeds when no landmarks match (`FaceClustering.cropFaceCGImage`):
/// 15% padding clamped to the frame, then `CGRect.integral`. `None` below 8 px.
pub fn padded_crop_rect(text: &str, image_width: u32, image_height: u32, orientation: u8) -> Option<[i64; 4]> {
    let b = parse_normalized(text, image_width, image_height, orientation)?;
    let pad = 0.15;
    let x = (b.x - b.w * pad).max(0.0);
    let y = (b.y - b.h * pad).max(0.0);
    let w = (1.0 - x).min(b.w * (1.0 + 2.0 * pad));
    let h = (1.0 - y).min(b.h * (1.0 + 2.0 * pad));
    if w <= 0.001 || h <= 0.001 {
        return None;
    }
    let (iw, ih) = (f64::from(image_width), f64::from(image_height));
    let (px, py, pw, ph) = (x * iw, (1.0 - y - h) * ih, w * iw, h * ih);
    let (left, top) = (px.floor(), py.floor());
    let (right, bottom) = ((px + pw).ceil(), (py + ph).ceil());
    let (width, height) = (right - left, bottom - top);
    if width < 8.0 || height < 8.0 {
        return None;
    }
    Some([left as i64, top as i64, width as i64, height as i64])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> NormRect {
        NormRect::new(x, y, w, h)
    }

    #[test]
    fn nearby_face_cannot_supply_landmarks() {
        let stored = rect(0.4, 0.4, 0.02, 0.02);
        let neighbor = rect(0.43, 0.4, 0.02, 0.02);
        assert_eq!(index(&stored, &[neighbor]), None);
        assert_eq!(index(&stored, &[neighbor, stored]), Some(1));
    }

    #[test]
    fn displaced_overlapping_face_is_rejected() {
        let stored = rect(0.4, 0.4, 0.1, 0.1);
        assert_eq!(index(&stored, &[rect(0.45, 0.4, 0.1, 0.1)]), None);
        assert_eq!(index(&stored, &[rect(0.401, 0.399, 0.1, 0.1)]), Some(0));
    }

    #[test]
    fn ambiguous_and_invalid_geometry_abstains() {
        let stored = rect(0.4, 0.4, 0.1, 0.1);
        assert_eq!(index(&stored, &[stored, stored]), None);
        assert_eq!(index(&rect(0.0, 0.0, 0.0, 0.0), &[stored]), None);
        assert_eq!(index(&rect(f64::NAN, 0.0, 0.1, 0.1), &[stored]), None);
        assert_eq!(index(&stored, &[rect(0.0, 0.0, 5.0, 5.0)]), None);
    }

    #[test]
    fn clear_winner_over_a_weaker_overlap() {
        let stored = rect(0.4, 0.4, 0.1, 0.1);
        let weak = rect(0.42, 0.42, 0.1, 0.1);
        assert_eq!(index(&stored, &[weak, rect(0.4005, 0.4, 0.1, 0.1)]), Some(1));
    }

    #[test]
    fn csv_boxes_parse_strictly_like_swift() {
        assert_eq!(parse_normalized(" 0.1,0.2,0.3,0.4\n", 10, 10, 1), Some(rect(0.1, 0.2, 0.3, 0.4)));
        assert_eq!(parse_normalized("0.1,0.2,0.3,0.4,extra", 10, 10, 6), Some(rect(0.1, 0.2, 0.3, 0.4)));
        assert_eq!(parse_normalized("0.1, 0.2,0.3,0.4", 10, 10, 1), None);
        assert_eq!(parse_normalized("0.1,0.2,0,0.4", 10, 10, 1), None);
        assert_eq!(parse_normalized("0.1,0.2,inf,0.4", 10, 10, 1), None);
        assert_eq!(parse_normalized("0.1,0.2,0.3", 10, 10, 1), None);
        assert_eq!(parse_normalized("", 10, 10, 1), None);
    }

    #[test]
    fn json_boxes_flip_and_rotate_like_swift() {
        let upright = parse_normalized(r#"{"x":10,"y":20,"w":30,"h":40}"#, 100, 200, 1).unwrap();
        assert_eq!(upright, rect(0.1, 1.0 - 0.1 - 0.2, 0.3, 0.2));
        assert_eq!(parse_normalized(r#"{"x":10,"y":20,"w":30,"h":40}"#, 100, 200, 6), None);
        let rotated = r#"{"x":10,"y":20,"w":30,"h":40,"sourceWidth":100,"sourceHeight":200,"coordinateSpace":"pixel-top-left"}"#;
        let r = parse_normalized(rotated, 200, 100, 6).unwrap();
        assert!((r.x - 0.7).abs() < 1e-12 && (r.y - 0.6).abs() < 1e-12 && (r.w - 0.2).abs() < 1e-12 && (r.h - 0.3).abs() < 1e-12, "{r:?}");
        let wrong_space = r#"{"x":10,"y":20,"w":30,"h":40,"sourceWidth":100,"sourceHeight":200}"#;
        assert_eq!(parse_normalized(wrong_space, 100, 200, 1), None);
    }

    #[derive(serde::Deserialize)]
    struct Padded {
        bbox: String,
        width: u32,
        height: u32,
        orientation: u8,
        expected: Option<[i64; 4]>,
    }

    fn padded_fixtures() -> Vec<Padded> {
        #[derive(serde::Deserialize)]
        struct Fixture {
            #[serde(rename = "paddedCrops")]
            padded_crops: Vec<Padded>,
        }
        let fixture: Fixture = serde_json::from_str(include_str!("../../../../../../shared/test-corpus/face-alignment.json")).unwrap();
        fixture.padded_crops
    }

    /// Same rects as the Swift engine's `cropFaceCGImage` (FaceLandmarkMatchTests).
    #[test]
    fn shared_padded_crops_match_swift() {
        let cases = padded_fixtures();
        assert!(cases.len() >= 4);
        for case in cases {
            assert_eq!(padded_crop_rect(&case.bbox, case.width, case.height, case.orientation), case.expected, "{}", case.bbox);
        }
    }

    #[test]
    #[ignore]
    fn print_padded_fixture() {
        for case in padded_fixtures() {
            println!("PAD {} {:?}", case.bbox, padded_crop_rect(&case.bbox, case.width, case.height, case.orientation));
        }
    }
}
