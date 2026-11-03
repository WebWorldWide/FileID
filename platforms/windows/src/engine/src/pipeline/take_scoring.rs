//! Model-free Best Takes scoring: subject sharpness, motion blur, exposure,
//! faces/eyes/framing and, for clips, steadiness and action. Pure functions on
//! 8-bit luma; mirror of the macOS `TakeScoring.swift` with the same
//! thresholds, weights and reasons. Change both together.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const VERSION: &str = "take-signals-v2";
pub const ANALYSIS_LONG_SIDE: u32 = 1024;
pub const VIDEO_LONG_SIDE: u32 = 384;
pub const MOTION_LONG_SIDE: usize = 96;
pub const MOTION_SEARCH: i32 = 6;

pub const MIN_FACE_PIXELS: f64 = 24.0;
pub const MIN_EYE_PIXELS: f64 = 40.0;
pub const EYES_CLOSED_BELOW: f64 = 0.35;
pub const MOTION_BLUR_BELOW: f64 = 0.75;
pub const OUT_OF_FOCUS_BELOW: f64 = 0.6;
pub const SOFT_BELOW: f64 = 0.85;
pub const MOTION_ANISOTROPY: f64 = 0.45;
pub const MOTION_ANISOTROPY_MARGIN: f64 = 0.12;
pub const EDGE_MARGIN: f64 = 0.005;
pub const SHAKY_ABOVE: f64 = 2.0;
pub const MIN_PERSON_HEIGHT: f64 = 0.15;

pub mod reason {
    pub const SHARPEST_FACE: &str = "Sharpest face";
    pub const SHARPEST: &str = "Sharpest";
    pub const EYES_OPEN: &str = "Eyes open";
    pub const ALL_FACES: &str = "Every face visible";
    pub const BEST_EXPOSURE: &str = "Best exposure";
    pub const STEADIEST: &str = "Steadiest";
    pub const MOST_ACTION: &str = "Most action";
    pub const EYES_CLOSED: &str = "Eyes closed";
    pub const MOTION_BLUR: &str = "Motion blur";
    pub const OUT_OF_FOCUS: &str = "Out of focus";
    pub const SOFT: &str = "A little soft";
    pub const HIGHLIGHTS: &str = "Bright spots blown out";
    pub const TOO_DARK: &str = "Too dark";
    pub const TOO_BRIGHT: &str = "Too bright";
    pub const FACE_CUT: &str = "Face cut off at the edge";
    pub const FACE_HIDDEN: &str = "A face is hidden";
    pub const SHAKY: &str = "Shaky";
    pub const LITTLE_ACTION: &str = "Little action";
    pub const UNREADABLE: &str = "Couldn't read this file";
    pub const FILLS_FRAME: &str = "Subject fills the frame";
    pub const EVERYONE: &str = "Everyone in the shot";
    pub const SMALL_SUBJECT: &str = "Subject small in the frame";
    pub const PERSON_CUT: &str = "Someone is cut off at the edge";
}

#[derive(Debug, Clone, PartialEq)]
pub struct Luma {
    pub width: usize,
    pub height: usize,
    pub pixels: Vec<u8>,
}

impl Luma {
    /// BT.601 integer luma, (77 R + 150 G + 29 B) >> 8, from packed RGB8.
    pub fn from_rgb(rgb: &[u8], width: usize, height: usize) -> Self {
        let pixels = rgb
            .chunks_exact(3)
            .take(width * height)
            .map(|p| ((77 * p[0] as u32 + 150 * p[1] as u32 + 29 * p[2] as u32) >> 8) as u8)
            .collect();
        Self { width, height, pixels }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Box2 {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// One detected face, top-left origin, normalized to the frame. `quality` and
/// `eyes_open` are 0…1 when the detector can judge them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Face {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eyes_open: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSignals {
    pub duration: f64,
    pub shake: f64,
    pub action: f64,
    pub peak_seconds: f64,
}

/// Per-file measurements, cached in `take_quality.signals_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signals {
    pub width: usize,
    pub height: usize,
    pub sharpness: f64,
    pub anisotropy: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Box2>,
    pub mean_luma: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub faces: Vec<Face>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_sharpness: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<VideoSignals>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub people: Option<Vec<Box2>>,
}

pub fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

pub fn round3(value: f64) -> f64 {
    (value * 1_000.0).round() / 1_000.0
}

pub fn cell_size(width: usize, height: usize) -> usize {
    8usize.max((width.max(height) as f64 / 32.0).round() as usize)
}

#[derive(Clone, Copy, Default)]
struct CellStats {
    lap: i64,
    dx: i64,
    dy: i64,
    d1: i64,
    d2: i64,
    luma: i64,
    count: i64,
}

pub fn signals(image: &Luma, faces: Vec<Face>, people: Option<Vec<Box2>>) -> Signals {
    let (w, h) = (image.width, image.height);
    let p = &image.pixels;
    let cell = cell_size(w, h);
    let columns = w.div_ceil(cell);
    let rows = h.div_ceil(cell);
    let mut cells = vec![CellStats::default(); (columns * rows).max(1)];
    let (mut luma_sum, mut bright, mut dark) = (0i64, 0i64, 0i64);
    for &value in p {
        luma_sum += value as i64;
        if value >= 250 {
            bright += 1;
        }
        if value <= 5 {
            dark += 1;
        }
    }
    if w >= 3 && h >= 3 {
        for y in 1..h - 1 {
            let row = y * w;
            let cell_row = (y / cell) * columns;
            for x in 1..w - 1 {
                let i = row + x;
                let c = p[i] as i64;
                let lap = 4 * c - p[i - 1] as i64 - p[i + 1] as i64 - p[i - w] as i64 - p[i + w] as i64;
                let dx = p[i + 1] as i64 - c;
                let dy = p[i + w] as i64 - c;
                let d1 = p[i + w + 1] as i64 - c;
                let d2 = p[i - w + 1] as i64 - c;
                let s = &mut cells[cell_row + x / cell];
                s.lap += lap * lap;
                s.dx += dx * dx;
                s.dy += dy * dy;
                s.d1 += d1 * d1;
                s.d2 += d2 * d2;
                s.luma += c;
                s.count += 1;
            }
        }
    }
    let minimum = ((cell * cell / 4).max(1)) as i64;
    let mut ranked: Vec<(usize, f64)> = cells
        .iter()
        .enumerate()
        .filter(|(_, s)| s.count >= minimum)
        .filter(|(_, s)| {
            let mean = s.luma as f64 / s.count as f64;
            (8.0..=247.0).contains(&mean)
        })
        .map(|(i, s)| (i, s.lap as f64 / s.count as f64))
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let take = 1usize.max((ranked.len() as f64 * 0.1).ceil() as usize);
    let top: Vec<(usize, f64)> = ranked.into_iter().take(take).collect();
    let (mut sharpness, mut anisotropy, mut subject) = (0.0, 0.0, None);
    if !top.is_empty() {
        sharpness = (top.iter().map(|t| t.1).sum::<f64>() / top.len() as f64).sqrt();
        let (mut sx, mut sy, mut s1, mut s2) = (0i64, 0i64, 0i64, 0i64);
        let (mut weight, mut cx, mut cy) = (0.0, 0.0, 0.0);
        let center = |index: usize| {
            (
                ((index % columns) as f64 + 0.5) * cell as f64 / w as f64,
                ((index / columns) as f64 + 0.5) * cell as f64 / h as f64,
            )
        };
        for &(index, energy) in &top {
            let s = cells[index];
            sx += s.dx;
            sy += s.dy;
            s1 += s.d1;
            s2 += s.d2;
            let (px, py) = center(index);
            weight += energy;
            cx += energy * px;
            cy += energy * py;
        }
        let directional = [sx as f64, sy as f64, s1 as f64 / 2.0, s2 as f64 / 2.0];
        let high = directional.iter().copied().fold(f64::MIN, f64::max);
        let low = directional.iter().copied().fold(f64::MAX, f64::min);
        if high > 0.0 {
            anisotropy = 1.0 - low / high;
        }
        if weight > 0.0 {
            cx /= weight;
            cy /= weight;
            let (mut vx, mut vy) = (0.0, 0.0);
            for &(index, energy) in &top {
                let (px, py) = center(index);
                vx += energy * (px - cx) * (px - cx);
                vy += energy * (py - cy) * (py - cy);
            }
            let hx = (1.5 * (vx / weight).sqrt()).clamp(0.1, 0.3);
            let hy = (1.5 * (vy / weight).sqrt()).clamp(0.1, 0.3);
            subject = Some(clamp_box(Box2 { x: cx - hx, y: cy - hy, width: 2.0 * hx, height: 2.0 * hy }));
        }
    }
    let n = p.len().max(1) as f64;
    let face_sharpness = primary_faces(&faces, w, h).first().and_then(|f| region_sharpness(image, f));
    Signals {
        width: w,
        height: h,
        sharpness: round4(sharpness),
        anisotropy: round4(anisotropy),
        subject,
        mean_luma: round4(luma_sum as f64 / n / 255.0),
        highlights: round4(bright as f64 / n),
        shadows: round4(dark as f64 / n),
        faces,
        face_sharpness: face_sharpness.map(round4),
        video: None,
        people,
    }
}

pub fn region_sharpness(image: &Luma, face: &Face) -> Option<f64> {
    let (w, h) = (image.width as i64, image.height as i64);
    let x0 = 1.max((face.x * w as f64).floor() as i64);
    let y0 = 1.max((face.y * h as f64).floor() as i64);
    let x1 = (w - 1).min(((face.x + face.width) * w as f64).ceil() as i64);
    let y1 = (h - 1).min(((face.y + face.height) * h as f64).ceil() as i64);
    if x1 - x0 < 4 || y1 - y0 < 4 {
        return None;
    }
    let p = &image.pixels;
    let w = w as usize;
    let (mut sum, mut count) = (0i64, 0i64);
    for y in y0 as usize..y1 as usize {
        for x in x0 as usize..x1 as usize {
            let i = y * w + x;
            let lap = 4 * p[i] as i64 - p[i - 1] as i64 - p[i + 1] as i64 - p[i - w] as i64 - p[i + w] as i64;
            sum += lap * lap;
            count += 1;
        }
    }
    Some((sum as f64 / count as f64).sqrt())
}

fn short_side(face: &Face, width: usize, height: usize) -> f64 {
    (face.width * width as f64).min(face.height * height as f64)
}

/// Faces big enough to judge, largest first: at least `MIN_FACE_PIXELS` on the
/// short side and a quarter of the largest face's area.
pub fn primary_faces(faces: &[Face], width: usize, height: usize) -> Vec<Face> {
    let mut sized: Vec<Face> = faces.iter().filter(|f| short_side(f, width, height) >= MIN_FACE_PIXELS).cloned().collect();
    sized.sort_by(|a, b| {
        let (aa, ba) = (a.width * a.height, b.width * b.height);
        ba.total_cmp(&aa).then(a.x.total_cmp(&b.x))
    });
    let Some(largest) = sized.first().map(|f| f.width * f.height) else { return vec![] };
    sized.into_iter().filter(|f| f.width * f.height >= 0.25 * largest).collect()
}

/// Box-averaged downsample to `long_side` on the long edge.
pub fn downsample(image: &Luma, long_side: usize) -> Luma {
    let scale = image.width.max(image.height) as f64 / long_side as f64;
    if scale <= 1.0 {
        return image.clone();
    }
    let w = 1usize.max((image.width as f64 / scale).round() as usize);
    let h = 1usize.max((image.height as f64 / scale).round() as usize);
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        let sy0 = y * image.height / h;
        let sy1 = (sy0 + 1).max((y + 1) * image.height / h);
        for x in 0..w {
            let sx0 = x * image.width / w;
            let sx1 = (sx0 + 1).max((x + 1) * image.width / w);
            let mut sum = 0usize;
            for sy in sy0..sy1 {
                for sx in sx0..sx1 {
                    sum += image.pixels[sy * image.width + sx] as usize;
                }
            }
            out[y * w + x] = (sum / ((sy1 - sy0) * (sx1 - sx0))) as u8;
        }
    }
    Luma { width: w, height: h, pixels: out }
}

/// Camera shift between two frames (integer search within ±`MOTION_SEARCH`)
/// and the mean absolute difference left after removing it, 0…1.
pub fn shift(a: &Luma, b: &Luma) -> (i32, i32, f64) {
    let r = MOTION_SEARCH;
    if a.width != b.width || a.height != b.height || a.width as i32 <= 2 * r || a.height as i32 <= 2 * r {
        return (0, 0, 0.0);
    }
    let (w, h) = (a.width as i32, a.height as i32);
    let mut best = (0i32, 0i32, i64::MAX, 1i64);
    for dy in -r..=r {
        for dx in -r..=r {
            let mut sad = 0i64;
            for y in r..h - r {
                let ra = (y * w) as usize;
                let rb = (y + dy) * w + dx;
                for x in r as usize..(w - r) as usize {
                    sad += (a.pixels[ra + x] as i64 - b.pixels[(rb + x as i32) as usize] as i64).abs();
                }
            }
            let better = sad < best.2 || (sad == best.2 && dx.abs() + dy.abs() < best.0.abs() + best.1.abs());
            if better {
                best = (dx, dy, sad, ((w - 2 * r) * (h - 2 * r)) as i64);
            }
        }
    }
    (best.0, best.1, best.2 as f64 / best.3 as f64 / 255.0)
}

pub fn anchor_times(duration: f64, count: usize, pair_gap: f64) -> Vec<f64> {
    if !duration.is_finite() || duration <= 0.0 {
        return vec![];
    }
    let usable = (duration - pair_gap).max(0.0);
    (0..count).map(|i| round4(usable * (i as f64 + 0.5) / count as f64)).collect()
}

/// Clip signals from frame pairs taken a fraction of a second apart.
pub fn video_signals(pairs: &[(f64, Luma, Luma)], duration: f64) -> Option<Signals> {
    let mut best: Option<Signals> = None;
    let (mut shakes, mut actions) = (Vec::new(), Vec::new());
    let mut peak = (pairs.first()?.0, -1.0);
    for (seconds, first, second) in pairs {
        for frame in [first, second] {
            let s = signals(frame, vec![], None);
            if best.as_ref().is_none_or(|b| s.sharpness > b.sharpness) {
                best = Some(s);
            }
        }
        let (dx, dy, residual) = shift(&downsample(first, MOTION_LONG_SIDE), &downsample(second, MOTION_LONG_SIDE));
        shakes.push(((dx * dx + dy * dy) as f64).sqrt());
        actions.push(residual);
        if residual > peak.1 {
            peak = (*seconds, residual);
        }
    }
    let mut result = best?;
    result.video = Some(VideoSignals {
        duration: round4(duration),
        shake: round4(shakes.iter().sum::<f64>() / shakes.len() as f64),
        action: round4(actions.iter().sum::<f64>() / actions.len() as f64),
        peak_seconds: round4(peak.0),
    });
    Some(result)
}

#[derive(Debug, Clone)]
pub struct Shot {
    pub primary_file_id: i64,
    pub file_ids: Vec<i64>,
    pub path: String,
    pub signals: Option<Signals>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Quality {
    pub primary_file_id: i64,
    pub file_ids: Vec<i64>,
    pub path: String,
    pub score: f64,
    pub rank: usize,
    pub recommended: bool,
    pub reasons: Vec<String>,
    pub sharpness: Option<f64>,
    pub exposure: Option<f64>,
    pub motion_blur: bool,
    pub face_count: usize,
    pub eyes_closed: bool,
    pub face_cut_off: bool,
    pub stability: Option<f64>,
    pub action: Option<f64>,
    pub subject: Option<Box2>,
    pub faces: Vec<Face>,
    pub peak_seconds: Option<f64>,
    pub framing: Option<f64>,
    pub person_cut: bool,
}

pub fn exposure_score(s: &Signals) -> f64 {
    let mut value = 1.0 - 2.0 * ((s.mean_luma - 0.45).abs() - 0.12).max(0.0);
    value -= 3.0 * (s.highlights - 0.01).max(0.0);
    value -= 2.0 * (s.shadows - 0.03).max(0.0);
    value.clamp(0.0, 1.0)
}

fn person_area(s: &Signals) -> f64 {
    s.people
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .filter(|b| b.height >= 0.1)
        .map(|b| b.width * b.height)
        .fold(0.0, f64::max)
}

fn person_count(s: &Signals) -> usize {
    s.people.as_deref().unwrap_or(&[]).iter().filter(|b| b.height >= 0.1).count()
}

fn visible(s: &Signals) -> usize {
    s.faces.iter().filter(|f| short_side(f, s.width, s.height) >= 16.0).count()
}

fn touches_edge(x: f64, y: f64, w: f64, h: f64) -> bool {
    x <= EDGE_MARGIN || y <= EDGE_MARGIN || x + w >= 1.0 - EDGE_MARGIN || y + h >= 1.0 - EDGE_MARGIN
}

pub fn score(shots: &[Shot]) -> Vec<Quality> {
    let readable: Vec<&Signals> = shots.iter().filter_map(|s| s.signals.as_ref()).collect();
    let is_video = readable.iter().any(|s| s.video.is_some());
    let max_sharp = readable.iter().map(|s| s.sharpness).fold(0.0, f64::max);
    let every_face = !readable.is_empty() && readable.iter().all(|s| s.face_sharpness.is_some());
    let max_face_sharp = if every_face { readable.iter().filter_map(|s| s.face_sharpness).fold(0.0, f64::max) } else { 0.0 };
    let max_visible = readable.iter().map(|s| visible(s)).max().unwrap_or(0);
    let group_has_faces = readable.iter().any(|s| !primary_faces(&s.faces, s.width, s.height).is_empty());
    let mut anisotropies: Vec<f64> = readable.iter().map(|s| s.anisotropy).collect();
    anisotropies.sort_by(f64::total_cmp);
    let median_anisotropy = if anisotropies.is_empty() { 0.0 } else { anisotropies[anisotropies.len() / 2] };
    let max_action = readable.iter().filter_map(|s| s.video.as_ref().map(|v| v.action)).fold(0.0, f64::max);
    let max_area = readable.iter().map(|s| person_area(s)).fold(0.0, f64::max);
    let max_people = readable.iter().map(|s| person_count(s)).max().unwrap_or(0);
    let group_has_people = !is_video
        && readable.iter().any(|s| s.people.as_deref().unwrap_or(&[]).iter().any(|b| b.height >= MIN_PERSON_HEIGHT));

    struct Draft {
        total: f64,
        sharp_rel: f64,
        quality: Quality,
    }
    let mut drafts: Vec<Draft> = Vec::with_capacity(shots.len());
    for shot in shots {
        let Some(s) = shot.signals.as_ref() else {
            drafts.push(Draft {
                total: -1.0,
                sharp_rel: 0.0,
                quality: Quality {
                    primary_file_id: shot.primary_file_id,
                    file_ids: shot.file_ids.clone(),
                    path: shot.path.clone(),
                    score: 0.0,
                    rank: 0,
                    recommended: false,
                    reasons: vec![reason::UNREADABLE.into()],
                    sharpness: None,
                    exposure: None,
                    motion_blur: false,
                    face_count: 0,
                    eyes_closed: false,
                    face_cut_off: false,
                    stability: None,
                    action: None,
                    subject: None,
                    faces: vec![],
                    peak_seconds: None,
                    framing: None,
                    person_cut: false,
                },
            });
            continue;
        };
        let peak_rel = if max_sharp > 0.0 { s.sharpness / max_sharp } else { 1.0 };
        let face_rel = (every_face && max_face_sharp > 0.0).then(|| s.face_sharpness.unwrap_or(0.0) / max_face_sharp);
        let sharp_rel = round4(face_rel.map_or(peak_rel, |f| 0.6 * f + 0.4 * peak_rel));
        let exposure = round4(exposure_score(s));
        let primary = primary_faces(&s.faces, s.width, s.height);
        let eyes_closed = primary
            .iter()
            .filter(|f| short_side(f, s.width, s.height) >= MIN_EYE_PIXELS)
            .any(|f| f.eyes_open.unwrap_or(1.0) < EYES_CLOSED_BELOW);
        let face_cut = primary.iter().any(|f| touches_edge(f.x, f.y, f.width, f.height));
        let motion_blur = !is_video
            && sharp_rel < MOTION_BLUR_BELOW
            && s.anisotropy >= MOTION_ANISOTROPY.max(median_anisotropy + MOTION_ANISOTROPY_MARGIN);
        let face_count = visible(s);
        let largest = s.people.as_deref().unwrap_or(&[]).iter().copied().reduce(|a, b| {
            if b.width * b.height > a.width * a.height {
                b
            } else {
                a
            }
        });
        let person_cut = group_has_people
            && !face_cut
            && largest.is_some_and(|b| b.x <= EDGE_MARGIN || b.x + b.width >= 1.0 - EDGE_MARGIN);
        let framing = group_has_people.then(|| {
            round4(
                0.6 * if max_area > 0.0 { person_area(s) / max_area } else { 0.0 }
                    + 0.4 * if max_people > 0 { person_count(s) as f64 / max_people as f64 } else { 0.0 },
            )
        });
        let (mut stability, mut action) = (None, None);
        let mut total;
        if let Some(video) = &s.video {
            let steady = round4((1.0 - video.shake / 4.0).clamp(0.0, 1.0));
            let moving = round4(if max_action > 0.0 { video.action / max_action } else { 0.0 });
            stability = Some(steady);
            action = Some(moving);
            total = 0.35 * steady + 0.30 * moving + 0.25 * sharp_rel + 0.10 * exposure;
        } else if group_has_faces {
            let qualities: Vec<f64> = primary.iter().map(|f| f.quality.unwrap_or(0.5)).collect();
            let avg_quality = if qualities.is_empty() { 0.0 } else { qualities.iter().sum::<f64>() / qualities.len() as f64 };
            let face_score =
                (if max_visible > 0 { 0.5 * face_count as f64 / max_visible as f64 } else { 0.0 }) + 0.5 * avg_quality;
            total = match framing {
                Some(f) => 0.35 * sharp_rel + 0.15 * exposure + 0.25 * face_score + 0.25 * f,
                None => 0.45 * sharp_rel + 0.20 * exposure + 0.35 * face_score,
            };
            if person_cut {
                total -= 0.05;
            }
            if eyes_closed {
                total -= 0.25;
            }
            if face_cut {
                total -= 0.10;
            }
            if motion_blur {
                total -= 0.10;
            }
        } else if let Some(f) = framing {
            total = 0.45 * sharp_rel + 0.20 * exposure + 0.35 * f;
            if person_cut {
                total -= 0.05;
            }
            if motion_blur {
                total -= 0.10;
            }
        } else {
            total = 0.65 * sharp_rel + 0.35 * exposure;
            if motion_blur {
                total -= 0.10;
            }
        }
        let total = round3(total.clamp(0.0, 1.0));
        let mut negatives: Vec<String> = Vec::new();
        if eyes_closed {
            negatives.push(reason::EYES_CLOSED.into());
        }
        if motion_blur {
            negatives.push(reason::MOTION_BLUR.into());
        } else if !is_video && sharp_rel < OUT_OF_FOCUS_BELOW {
            negatives.push(reason::OUT_OF_FOCUS.into());
        }
        if face_cut {
            negatives.push(reason::FACE_CUT.into());
        }
        if person_cut {
            negatives.push(reason::PERSON_CUT.into());
        }
        if group_has_faces && face_count < max_visible {
            negatives.push(reason::FACE_HIDDEN.into());
        }
        if group_has_people && max_area > 0.0 && person_area(s) < 0.5 * max_area {
            negatives.push(reason::SMALL_SUBJECT.into());
        }
        if s.highlights > 0.05 {
            negatives.push(reason::HIGHLIGHTS.into());
        }
        if s.mean_luma < 0.20 || s.shadows > 0.25 {
            negatives.push(reason::TOO_DARK.into());
        } else if s.mean_luma > 0.78 {
            negatives.push(reason::TOO_BRIGHT.into());
        }
        if stability.is_some_and(|v| v < 1.0 - SHAKY_ABOVE / 4.0) {
            negatives.push(reason::SHAKY.into());
        }
        if action.is_some_and(|v| max_action > 0.0 && v < 0.4) {
            negatives.push(reason::LITTLE_ACTION.into());
        }
        if negatives.is_empty() && !is_video && sharp_rel < SOFT_BELOW {
            negatives.push(reason::SOFT.into());
        }
        let subject = primary
            .first()
            .map(|f| Box2 { x: f.x, y: f.y, width: f.width, height: f.height })
            .or(s.subject);
        drafts.push(Draft {
            total,
            sharp_rel,
            quality: Quality {
                primary_file_id: shot.primary_file_id,
                file_ids: shot.file_ids.clone(),
                path: shot.path.clone(),
                score: total,
                rank: 0,
                recommended: false,
                reasons: negatives,
                sharpness: Some(sharp_rel),
                exposure: Some(exposure),
                motion_blur,
                face_count,
                eyes_closed,
                face_cut_off: face_cut,
                stability,
                action,
                subject,
                faces: s.faces.clone(),
                peak_seconds: s.video.as_ref().map(|v| v.peak_seconds),
                framing,
                person_cut,
            },
        });
    }
    let mut order: Vec<usize> = (0..drafts.len()).collect();
    order.sort_by(|&a, &b| {
        let (l, r) = (&drafts[a], &drafts[b]);
        r.total
            .total_cmp(&l.total)
            .then(r.sharp_rel.total_cmp(&l.sharp_rel))
            .then(l.quality.primary_file_id.cmp(&r.quality.primary_file_id))
    });
    let mut rank = vec![0usize; drafts.len()];
    for (position, &index) in order.iter().enumerate() {
        rank[index] = position + 1;
    }
    let winner = order.first().copied().filter(|&i| drafts[i].total >= 0.0);
    let readable_drafts: Vec<&Draft> = drafts.iter().filter(|d| d.total >= 0.0).collect();
    let shared: HashSet<String> = match readable_drafts.first() {
        Some(first) if readable_drafts.len() > 1 => first
            .quality
            .reasons
            .iter()
            .filter(|r| readable_drafts.iter().all(|d| d.quality.reasons.contains(r)))
            .cloned()
            .collect(),
        _ => HashSet::new(),
    };
    let any_eyes_closed = drafts.iter().any(|d| d.quality.eyes_closed);
    let mut exposures: Vec<f64> = drafts.iter().filter_map(|d| d.quality.exposure).collect();
    exposures.sort_by(|a, b| b.total_cmp(a));
    let max_exposure = exposures.first().copied().unwrap_or(0.0);
    let second_exposure = exposures.get(1).copied().unwrap_or(max_exposure);
    let max_stability = drafts.iter().filter_map(|d| d.quality.stability).fold(0.0, f64::max);
    let mut out = Vec::with_capacity(drafts.len());
    for (index, draft) in drafts.iter().enumerate() {
        let q = &draft.quality;
        let mut reasons: Vec<String> = q.reasons.iter().filter(|r| !shared.contains(*r)).cloned().collect();
        let is_winner = winner == Some(index) && shots.len() > 1;
        if is_winner {
            let mut positives: Vec<String> = Vec::new();
            if is_video {
                if q.stability.is_some_and(|s| s >= max_stability) {
                    positives.push(reason::STEADIEST.into());
                }
                if q.action.is_some_and(|a| a >= 1.0) {
                    positives.push(reason::MOST_ACTION.into());
                }
                if q.sharpness == Some(1.0) && positives.is_empty() {
                    positives.push(reason::SHARPEST.into());
                }
            } else {
                if q.sharpness == Some(1.0) {
                    positives.push(if every_face { reason::SHARPEST_FACE } else { reason::SHARPEST }.into());
                }
                if any_eyes_closed && !q.eyes_closed {
                    positives.push(reason::EYES_OPEN.into());
                }
                if group_has_faces
                    && q.face_count == max_visible
                    && drafts.iter().any(|d| d.quality.face_count < max_visible && d.total >= 0.0)
                {
                    positives.push(reason::ALL_FACES.into());
                }
                if let Some(s) = shots[index].signals.as_ref() {
                    if group_has_people
                        && max_area > 0.0
                        && person_area(s) >= max_area
                        && readable.iter().any(|o| person_area(o) < 0.7 * max_area)
                    {
                        positives.push(reason::FILLS_FRAME.into());
                    }
                    if group_has_people
                        && max_people >= 2
                        && person_count(s) == max_people
                        && readable.iter().any(|o| person_count(o) < max_people)
                    {
                        positives.push(reason::EVERYONE.into());
                    }
                }
                if q.exposure == Some(max_exposure) && max_exposure - second_exposure >= 0.1 {
                    positives.push(reason::BEST_EXPOSURE.into());
                }
            }
            reasons.retain(|r| r != reason::SOFT);
            positives.extend(reasons);
            reasons = positives;
        }
        reasons.truncate(3);
        out.push(Quality { rank: rank[index], recommended: is_winner, reasons, ..q.clone() });
    }
    out
}

pub fn clamp_box(b: Box2) -> Box2 {
    let x = b.x.clamp(0.0, 1.0);
    let y = b.y.clamp(0.0, 1.0);
    Box2 { x: round4(x), y: round4(y), width: round4(b.width.min(1.0 - x)), height: round4(b.height.min(1.0 - y)) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(width: usize, height: usize, blur: usize, offset: u8) -> Luma {
        let mut pixels = vec![0u8; width * height];
        for y in 0..height {
            for x in 0..width {
                pixels[y * width + x] = if ((x / 4) + (y / 4)) % 2 == 0 { 40 + offset } else { 200 };
            }
        }
        let mut image = Luma { width, height, pixels };
        for _ in 0..blur {
            let src = image.pixels.clone();
            for y in 1..height - 1 {
                for x in 1..width - 1 {
                    let i = y * width + x;
                    let sum = src[i] as u32 * 4 + src[i - 1] as u32 + src[i + 1] as u32 + src[i - width] as u32 + src[i + width] as u32;
                    image.pixels[i] = (sum / 8) as u8;
                }
            }
        }
        image
    }

    fn shot(id: i64, s: Signals) -> Shot {
        Shot { primary_file_id: id, file_ids: vec![id], path: format!("/t/{id}.png"), signals: Some(s) }
    }

    #[test]
    fn the_sharp_try_wins_and_says_why() {
        let sharp = signals(&pattern(96, 64, 0, 0), vec![], None);
        let soft = signals(&pattern(96, 64, 3, 0), vec![], None);
        assert!(sharp.sharpness > soft.sharpness * 1.5);
        let scored = score(&[shot(1, soft), shot(2, sharp)]);
        assert!(scored[1].recommended && scored[1].rank == 1);
        assert_eq!(scored[1].reasons, vec![reason::SHARPEST.to_string()]);
        assert!(scored[0].reasons.contains(&reason::OUT_OF_FOCUS.to_string()));
    }

    #[test]
    fn shared_problems_are_not_reasons_and_closed_eyes_lose() {
        let mut open = signals(&pattern(400, 300, 0, 0), vec![], None);
        open.faces = vec![Face { x: 0.3, y: 0.2, width: 0.2, height: 0.25, quality: Some(0.6), eyes_open: Some(0.9) }];
        open.face_sharpness = Some(30.0);
        let mut closed = open.clone();
        closed.faces[0].eyes_open = Some(0.1);
        closed.face_sharpness = Some(31.0);
        let scored = score(&[shot(1, closed), shot(2, open)]);
        assert!(scored[1].recommended);
        assert!(scored[1].reasons.contains(&reason::EYES_OPEN.to_string()));
        assert_eq!(scored[0].reasons.first().map(String::as_str), Some(reason::EYES_CLOSED));
    }

    #[test]
    fn bigger_subject_and_everyone_in_shot_win_when_sharpness_ties() {
        let base = signals(&pattern(96, 64, 0, 0), vec![], None);
        let mut wide = base.clone();
        wide.people = Some(vec![Box2 { x: 0.4, y: 0.4, width: 0.1, height: 0.3 }]);
        let mut tight = base;
        tight.people = Some(vec![Box2 { x: 0.3, y: 0.1, width: 0.3, height: 0.8 }, Box2 { x: 0.7, y: 0.3, width: 0.1, height: 0.3 }]);
        let scored = score(&[shot(1, wide), shot(2, tight)]);
        assert!(scored[1].recommended);
        assert!(scored[1].reasons.contains(&reason::FILLS_FRAME.to_string()));
        assert!(scored[0].reasons.contains(&reason::SMALL_SUBJECT.to_string()));
    }

    #[test]
    fn camera_shift_is_found_and_residual_is_small() {
        let a = pattern(96, 64, 1, 0);
        let mut b = a.clone();
        for y in 0..64 {
            for x in 0..96 {
                b.pixels[y * 96 + x] = a.pixels[y * 96 + (x + 93) % 96];
            }
        }
        let (dx, dy, residual) = shift(&a, &b);
        assert_eq!((dx, dy), (3, 0));
        assert!(residual < 0.01);
    }

    #[test]
    fn signals_round_trip_through_the_cache_json() {
        let s = signals(&pattern(64, 48, 1, 0), vec![], None);
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"meanLuma\""));
        assert_eq!(serde_json::from_str::<Signals>(&json).unwrap(), s);
    }
}
