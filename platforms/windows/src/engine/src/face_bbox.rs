#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizedFaceBBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl NormalizedFaceBBox {
    pub fn parse(bbox: &str, image_width: u32, image_height: u32, orientation: u8) -> Option<Self> {
        if image_width == 0 || image_height == 0 || !(1..=8).contains(&orientation) {
            return None;
        }
        let bbox = bbox.trim();
        if !bbox.starts_with('{') {
            let values = bbox.split(',').take(5).map(|value| value.trim().parse::<f64>().ok())
                .collect::<Option<Vec<_>>>()?;
            if values.len() != 4 {
                return None;
            }
            return Self::validated(values[0], 1.0 - values[1] - values[3], values[2], values[3]);
        }
        let value: serde_json::Value = serde_json::from_str(bbox).ok()?;
        let number = |key: &str| value.get(key)?.as_f64();
        let (source_width, source_height) = if value.get("sourceWidth").is_some() || value.get("sourceHeight").is_some() {
            if value.get("coordinateSpace")?.as_str()? != "pixel-top-left" {
                return None;
            }
            (number("sourceWidth")?, number("sourceHeight")?)
        } else {
            if orientation != 1 {
                return None;
            }
            (f64::from(image_width), f64::from(image_height))
        };
        if !source_width.is_finite() || !source_height.is_finite() || source_width <= 0.0 || source_height <= 0.0 {
            return None;
        }
        let bounds = Self::validated(number("x")? / source_width, number("y")? / source_height,
            number("w")? / source_width, number("h")? / source_height)?;
        bounds.oriented(orientation)
    }

    pub fn source_pixels_from_upright(bbox: [f32;4], source_width: u32, source_height: u32, orientation: u8) -> Option<[f32;4]> {
        if source_width == 0 || source_height == 0 || !(1..=8).contains(&orientation) { return None; }
        let (width,height) = if orientation >= 5 { (source_height,source_width) } else { (source_width,source_height) };
        let upright = Self::validated(f64::from(bbox[0])/f64::from(width), f64::from(bbox[1])/f64::from(height),
            f64::from(bbox[2])/f64::from(width), f64::from(bbox[3])/f64::from(height))?;
        let inverse = match orientation { 6 => 8, 8 => 6, value => value };
        let raw = upright.oriented(inverse)?;
        Some([(raw.x*f64::from(source_width)) as f32,(raw.y*f64::from(source_height)) as f32,
            (raw.width*f64::from(source_width)) as f32,(raw.height*f64::from(source_height)) as f32])
    }

    fn oriented(self, orientation: u8) -> Option<Self> {
        let bounds = self;
        let orient = |x: f64, y: f64| match orientation {
            2 => (1.0 - x, y),
            3 => (1.0 - x, 1.0 - y),
            4 => (x, 1.0 - y),
            5 => (y, x),
            6 => (1.0 - y, x),
            7 => (1.0 - y, 1.0 - x),
            8 => (y, 1.0 - x),
            _ => (x, y),
        };
        let corners = [orient(bounds.x, bounds.y), orient(bounds.x + bounds.width, bounds.y),
            orient(bounds.x, bounds.y + bounds.height), orient(bounds.x + bounds.width, bounds.y + bounds.height)];
        let min_x = corners.iter().map(|point| point.0).fold(f64::INFINITY, f64::min);
        let min_y = corners.iter().map(|point| point.1).fold(f64::INFINITY, f64::min);
        let max_x = corners.iter().map(|point| point.0).fold(f64::NEG_INFINITY, f64::max);
        let max_y = corners.iter().map(|point| point.1).fold(f64::NEG_INFINITY, f64::max);
        Self::validated(min_x, min_y, max_x - min_x, max_y - min_y)
    }

    fn validated(x: f64, y: f64, width: f64, height: f64) -> Option<Self> {
        if [x, y, width, height].iter().all(|value| value.is_finite()) && width > 0.0 && height > 0.0 {
            Some(Self { x, y, width, height })
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_and_upright_formats_describe_the_same_rotated_subject() {
        let raw = r#"{"x":8,"y":8,"w":16,"h":16,"coordinateSpace":"pixel-top-left","sourceWidth":84,"sourceHeight":64}"#;
        let a = NormalizedFaceBBox::parse(raw, 64, 84, 6).unwrap();
        let b = NormalizedFaceBBox::parse("0.625,0.7142857142857143,0.25,0.19047619047619047", 64, 84, 6).unwrap();
        assert!((a.x - b.x).abs() < 1e-12);
        assert!((a.y - b.y).abs() < 1e-12);
        assert!((a.width - b.width).abs() < 1e-12);
        assert!((a.height - b.height).abs() < 1e-12);
        assert!(NormalizedFaceBBox::parse(r#"{"x":8,"y":8,"w":16,"h":16}"#, 64, 84, 6).is_none());
        assert!(NormalizedFaceBBox::parse(r#"{"x":8,"y":8,"w":16,"h":16}"#, 84, 64, 1).is_some());
    }

    #[test]
    fn invalid_bounds_and_provenance_are_rejected() {
        for bbox in ["", "0,0,NaN,1", "0,0,1,inf", "0,0,-1,1", "0,0,1", "0,0,1,1,2", "{}",
            r#"{"x":0,"y":0,"w":1,"h":1,"sourceWidth":0,"sourceHeight":64,"coordinateSpace":"pixel-top-left"}"#,
            r#"{"x":0,"y":0,"w":1,"h":1,"sourceWidth":84,"sourceHeight":64,"coordinateSpace":"unknown"}"#] {
            assert!(NormalizedFaceBBox::parse(bbox, 84, 64, 1).is_none(), "{bbox}");
        }
        assert!(NormalizedFaceBBox::parse("0,0,1,1", 84, 64, 9).is_none());
    }
}
