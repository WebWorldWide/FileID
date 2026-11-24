//! Converts photos tagged with an RGB matrix/TRC ICC profile (Display P3, Adobe RGB, tagged sRGB: what cameras and
//! phones embed) to sRGB. Profiles built on lookup tables, or for another colour model, are refused.

const SRGB_FROM_XYZ_D50: [[f64; 3]; 3] =
    [[3.133_856_1, -1.616_866_7, -0.490_614_6], [-0.978_768_4, 1.916_141_5, 0.033_454_0], [0.071_945_3, -0.228_991_4, 1.405_242_7]];

#[derive(Debug, Clone)]
pub(crate) struct Profile {
    to_xyz: [[f64; 3]; 3],
    curves: [Curve; 3],
}

#[derive(Debug, Clone)]
enum Curve {
    Table(Vec<f64>),
    Parametric { kind: u16, p: [f64; 7] },
}

impl Curve {
    fn apply(&self, x: f64) -> f64 {
        let x = x.clamp(0.0, 1.0);
        let y = match self {
            Self::Table(values) if values.is_empty() => x,
            Self::Table(values) if values.len() == 1 => x.powf(values[0]),
            Self::Table(values) => {
                let at = x * (values.len() - 1) as f64;
                let low = at.floor() as usize;
                let high = (low + 1).min(values.len() - 1);
                values[low] + (values[high] - values[low]) * (at - low as f64)
            }
            Self::Parametric { kind, p } => {
                let [g, a, b, c, d, e, f] = *p;
                match kind {
                    0 => x.powf(g),
                    1 => if x >= -b / a { (a * x + b).max(0.0).powf(g) } else { 0.0 },
                    2 => if x >= -b / a { (a * x + b).max(0.0).powf(g) + c } else { c },
                    3 => if x >= d { (a * x + b).max(0.0).powf(g) } else { c * x },
                    _ => if x >= d { (a * x + b).max(0.0).powf(g) + e } else { c * x + f },
                }
            }
        };
        y.clamp(0.0, 1.0)
    }
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(bytes.get(at..at.checked_add(2)?)?.try_into().ok()?))
}

fn fixed_at(bytes: &[u8], at: usize) -> Option<f64> {
    Some(f64::from(u32_at(bytes, at)? as i32) / 65_536.0)
}

fn tag<'a>(bytes: &'a [u8], signature: &[u8; 4]) -> Option<&'a [u8]> {
    let count = u32_at(bytes, 128)? as usize;
    for index in 0..count.min(256) {
        let entry = 132 + index * 12;
        if bytes.get(entry..entry + 4)? == signature {
            let offset = u32_at(bytes, entry + 4)? as usize;
            let size = u32_at(bytes, entry + 8)? as usize;
            return bytes.get(offset..offset.checked_add(size)?);
        }
    }
    None
}

fn colorant(bytes: &[u8], signature: &[u8; 4]) -> Option<[f64; 3]> {
    let data = tag(bytes, signature)?;
    (data.get(..4)? == b"XYZ ").then_some(())?;
    Some([fixed_at(data, 8)?, fixed_at(data, 12)?, fixed_at(data, 16)?])
}

fn curve(bytes: &[u8], signature: &[u8; 4]) -> Option<Curve> {
    let data = tag(bytes, signature)?;
    match data.get(..4)? {
        b"curv" => {
            let count = u32_at(data, 8)? as usize;
            match count {
                0 => Some(Curve::Table(Vec::new())),
                1 => Some(Curve::Table(vec![f64::from(u16_at(data, 12)?) / 256.0])),
                2..=65_536 => (0..count).map(|index| Some(f64::from(u16_at(data, 12 + index * 2)?) / 65_535.0)).collect::<Option<Vec<_>>>().map(Curve::Table),
                _ => None,
            }
        }
        b"para" => {
            let kind = u16_at(data, 8)?;
            let count = *[1usize, 3, 4, 5, 7].get(usize::from(kind))?;
            let mut p = [0.0; 7];
            for (index, slot) in p.iter_mut().enumerate().take(count) {
                *slot = fixed_at(data, 12 + index * 4)?;
            }
            if kind >= 1 && p[1] == 0.0 {
                return None;
            }
            Some(Curve::Parametric { kind, p })
        }
        _ => None,
    }
}

impl Profile {
    /// None when the profile is not an RGB matrix/TRC profile with an XYZ connection space.
    pub(crate) fn parse(bytes: &[u8]) -> Option<Self> {
        (bytes.get(16..20)? == b"RGB " && bytes.get(20..24)? == b"XYZ ").then_some(())?;
        let (r, g, b) = (colorant(bytes, b"rXYZ")?, colorant(bytes, b"gXYZ")?, colorant(bytes, b"bXYZ")?);
        let to_xyz = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        Some(Self { to_xyz, curves: [curve(bytes, b"rTRC")?, curve(bytes, b"gTRC")?, curve(bytes, b"bTRC")?] })
    }

    fn to_srgb(&self, rgb: [f64; 3]) -> [f64; 3] {
        let linear = [self.curves[0].apply(rgb[0]), self.curves[1].apply(rgb[1]), self.curves[2].apply(rgb[2])];
        let mut xyz = [0.0; 3];
        for (row, out) in self.to_xyz.iter().zip(&mut xyz) {
            *out = row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2];
        }
        let mut out = [0.0; 3];
        for (row, slot) in SRGB_FROM_XYZ_D50.iter().zip(&mut out) {
            let value = (row[0] * xyz[0] + row[1] * xyz[1] + row[2] * xyz[2]).clamp(0.0, 1.0);
            *slot = if value <= 0.003_130_8 { value * 12.92 } else { 1.055 * value.powf(1.0 / 2.4) - 0.055 };
        }
        out
    }

    /// 8-bit lookup for the whole 3D grid would be 48 MB, so `Converter` keeps the curves as tables and multiplies.
    pub(crate) fn converter(&self) -> Converter {
        let mut worst = 0.0f64;
        for r in (0..=255).step_by(15) {
            for g in (0..=255).step_by(15) {
                for b in (0..=255).step_by(15) {
                    let input = [f64::from(r) / 255.0, f64::from(g) / 255.0, f64::from(b) / 255.0];
                    let out = self.to_srgb(input);
                    for (a, b) in input.iter().zip(out) {
                        worst = worst.max((a - b).abs() * 255.0);
                    }
                }
            }
        }
        if worst <= 1.0 {
            return Converter { identity: true, decode: Default::default(), matrix: [[0.0; 3]; 3], encode: Vec::new() };
        }
        let decode = std::array::from_fn(|channel| (0..256).map(|v| self.curves[channel].apply(v as f64 / 255.0)).collect::<Vec<_>>());
        let mut matrix = [[0.0; 3]; 3];
        for (i, row) in SRGB_FROM_XYZ_D50.iter().enumerate() {
            for j in 0..3 {
                matrix[i][j] = (0..3).map(|k| row[k] * self.to_xyz[k][j]).sum();
            }
        }
        let encode = (0..4096)
            .map(|v| {
                let value = f64::from(v) / 4095.0;
                let encoded = if value <= 0.003_130_8 { value * 12.92 } else { 1.055 * value.powf(1.0 / 2.4) - 0.055 };
                (encoded * 255.0).round() as u8
            })
            .collect();
        Converter { identity: false, decode, matrix, encode }
    }
}

pub(crate) struct Converter {
    identity: bool,
    decode: [Vec<f64>; 3],
    matrix: [[f64; 3]; 3],
    encode: Vec<u8>,
}

impl Converter {
    pub(crate) fn is_identity(&self) -> bool {
        self.identity
    }

    pub(crate) fn pixel(&self, rgb: [u8; 3]) -> [u8; 3] {
        if self.identity {
            return rgb;
        }
        let linear = [self.decode[0][usize::from(rgb[0])], self.decode[1][usize::from(rgb[1])], self.decode[2][usize::from(rgb[2])]];
        let mut out = [0u8; 3];
        for (row, slot) in self.matrix.iter().zip(&mut out) {
            let value = (row[0] * linear[0] + row[1] * linear[1] + row[2] * linear[2]).clamp(0.0, 1.0);
            *slot = self.encode[(value * 4095.0).round() as usize];
        }
        out
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn push_xyz(tags: &mut Vec<([u8; 4], Vec<u8>)>, name: &[u8; 4], xyz: [f64; 3]) {
        let mut data = b"XYZ \0\0\0\0".to_vec();
        for value in xyz {
            data.extend(((value * 65_536.0).round() as i32).to_be_bytes());
        }
        tags.push((*name, data));
    }

    /// A matrix/TRC RGB profile with the sRGB transfer curve and the given D50 colorants.
    pub(crate) fn profile(colorants: [[f64; 3]; 3]) -> Vec<u8> {
        let mut tags = Vec::new();
        for (name, xyz) in [b"rXYZ", b"gXYZ", b"bXYZ"].into_iter().zip(colorants) {
            push_xyz(&mut tags, name, xyz);
        }
        let mut para = b"para\0\0\0\0\0\x03\0\0".to_vec();
        for value in [2.4, 1.0 / 1.055, 0.055 / 1.055, 1.0 / 12.92, 0.04045] {
            para.extend(((value * 65_536.0_f64).round() as i32).to_be_bytes());
        }
        for name in [b"rTRC", b"gTRC", b"bTRC"] {
            tags.push((*name, para.clone()));
        }
        let table = 132 + tags.len() * 12;
        let mut bytes = vec![0u8; 128];
        bytes[16..20].copy_from_slice(b"RGB ");
        bytes[20..24].copy_from_slice(b"XYZ ");
        bytes.extend((tags.len() as u32).to_be_bytes());
        let mut offset = table;
        let mut body: Vec<u8> = Vec::new();
        for (name, data) in &tags {
            bytes.extend(name);
            bytes.extend((offset as u32).to_be_bytes());
            bytes.extend((data.len() as u32).to_be_bytes());
            body.extend(data);
            offset += data.len();
        }
        bytes.extend(body);
        bytes
    }

    pub(crate) const DISPLAY_P3: [[f64; 3]; 3] = [[0.5151, 0.2412, -0.0011], [0.2920, 0.6922, 0.0419], [0.1571, 0.0666, 0.7841]];
    pub(crate) const SRGB: [[f64; 3]; 3] = [[0.4360747, 0.2225045, 0.0139322], [0.3850649, 0.7168786, 0.0971045], [0.1430804, 0.0606169, 0.7141733]];

    #[test]
    fn a_tagged_srgb_profile_leaves_pixels_alone() {
        let converter = Profile::parse(&profile(SRGB)).unwrap().converter();
        assert!(converter.is_identity());
        assert_eq!(converter.pixel([12, 200, 99]), [12, 200, 99]);
    }

    #[test]
    fn display_p3_colours_are_converted_to_srgb() {
        let converter = Profile::parse(&profile(DISPLAY_P3)).unwrap().converter();
        assert!(!converter.is_identity());
        let neutral = converter.pixel([128, 128, 128]);
        assert!(neutral.iter().all(|c| (i32::from(*c) - 128).abs() <= 1), "{neutral:?}");
        let green = converter.pixel([0, 255, 0]);
        assert_eq!(green, [0, 255, 0], "out-of-gamut colours clip to the sRGB edge");
        let muted = converter.pixel([200, 100, 100]);
        assert!(muted[0] > 205 && muted[1] < 100 && muted[2] < 100, "P3 red is more saturated in sRGB: {muted:?}");
    }

    #[test]
    fn profiles_that_are_not_rgb_matrix_profiles_are_refused() {
        assert!(Profile::parse(&[0u8; 128]).is_none());
        let mut cmyk = profile(SRGB);
        cmyk[16..20].copy_from_slice(b"CMYK");
        assert!(Profile::parse(&cmyk).is_none());
        let truncated = profile(DISPLAY_P3);
        assert!(Profile::parse(&truncated[..200]).is_none());
    }
}
