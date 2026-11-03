//! Splits a video's low-resolution frame pool into visual scenes (CLIP cosine) and
//! activity windows (luma change). Mirror of the macOS `VideoSceneSelection.swift`
//! with the same thresholds; change both together.

use super::near_duplicate_grouping::cosine;

pub const POOL_INTERVAL: f64 = 0.5;
/// A frame starts a new scene when it and the next frame are both less similar
/// than this to the running mean of the current scene.
pub const CUT_COSINE: f32 = 0.80;
/// A slow pan or walk that drifts this far from the scene's first frame also cuts.
pub const DRIFT_COSINE: f32 = 0.70;
pub const CONFIRM_FRAMES: usize = 2;
pub const MIN_SCENE_SECONDS: f64 = 2.0;
/// Long static shots still get a fresh caption this often, for search coverage.
pub const MAX_SCENE_SECONDS: f64 = 20.0;
pub const ACTIVITY_CHANGE: f64 = 0.02;
pub const ACTIVITY_PAD_SECONDS: f64 = 0.5;
pub const MERGE_GAP_SECONDS: f64 = 1.0;
pub const MAX_WINDOW_SECONDS: f64 = 4.0;
pub const MAX_WINDOW_FRAMES: usize = 6;

#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub seconds: f64,
    pub embedding: Vec<f32>,
    /// Mean absolute luma change from the previous pool frame, 0..=1.
    pub change: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    pub start: f64,
    pub end: f64,
    pub representative: usize,
    pub frames: std::ops::Range<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Window {
    pub start: f64,
    pub end: f64,
    pub times: Vec<f64>,
}

pub fn pool_times(duration: f64, interval: f64) -> Vec<f64> {
    if !duration.is_finite() || duration <= 0.0 || interval <= 0.0 {
        return Vec::new();
    }
    let count = ((duration / interval).ceil() as usize).min(4_096);
    (0..count).map(|i| (i as f64 * interval).min(duration - 0.001)).filter(|t| *t >= 0.0).collect()
}

pub fn scenes(frames: &[Frame], duration: f64) -> Vec<Scene> {
    if frames.is_empty() || !duration.is_finite() || duration <= 0.0 {
        return Vec::new();
    }
    let mut bounds = vec![0usize];
    let mut sum = frames[0].embedding.clone();
    let mut pending = 0usize;
    for i in 1..frames.len() {
        let start = *bounds.last().unwrap_or(&0);
        let similar = frames[i].embedding.len() == sum.len()
            && cosine(&frames[i].embedding, &sum) >= CUT_COSINE
            && cosine(&frames[i].embedding, &frames[start].embedding) >= DRIFT_COSINE;
        let length = frames[i].seconds - frames[start].seconds;
        if length >= MAX_SCENE_SECONDS {
            bounds.push(i);
            sum.clone_from(&frames[i].embedding);
            pending = 0;
            continue;
        }
        if !similar && length >= MIN_SCENE_SECONDS {
            pending += 1;
            if pending >= CONFIRM_FRAMES {
                let first = i + 1 - CONFIRM_FRAMES;
                bounds.push(first);
                sum.clone_from(&frames[first].embedding);
                for frame in &frames[first + 1..=i] {
                    add(&frame.embedding, &mut sum);
                }
                pending = 0;
                continue;
            }
        } else {
            pending = 0;
        }
        add(&frames[i].embedding, &mut sum);
    }
    bounds.push(frames.len());
    bounds
        .windows(2)
        .map(|pair| {
            let (lower, upper) = (pair[0], pair[1]);
            let dims = frames[lower].embedding.len();
            let mut mean = vec![0f32; dims];
            for frame in &frames[lower..upper] {
                if frame.embedding.len() == dims {
                    for (m, v) in mean.iter_mut().zip(&frame.embedding) {
                        *m += v;
                    }
                }
            }
            let mut representative = lower;
            let mut best = f32::NEG_INFINITY;
            for i in lower..upper {
                let score = cosine(&frames[i].embedding, &mean);
                if score > best {
                    best = score;
                    representative = i;
                }
            }
            let end = if upper < frames.len() { frames[upper].seconds } else { duration };
            let start = if lower == 0 { 0.0 } else { frames[lower].seconds };
            Scene { start, end, representative, frames: lower..upper }
        })
        .collect()
}

pub fn activity_windows(frames: &[Frame], duration: f64) -> Vec<Window> {
    if frames.len() < 2 || !duration.is_finite() || duration <= 0.0 {
        return Vec::new();
    }
    let mut spans: Vec<(f64, f64)> = Vec::new();
    for i in 1..frames.len() {
        if frames[i].change < ACTIVITY_CHANGE {
            continue;
        }
        let start = (frames[i - 1].seconds - ACTIVITY_PAD_SECONDS).max(0.0);
        let end = (frames[i].seconds + ACTIVITY_PAD_SECONDS).min(duration);
        match spans.last_mut() {
            Some(last) if start - last.1 <= MERGE_GAP_SECONDS => last.1 = last.1.max(end),
            _ => spans.push((start, end)),
        }
    }
    let all: Vec<f64> = frames.iter().map(|f| f.seconds).collect();
    let mut windows = Vec::new();
    for (span_start, span_end) in spans {
        let pieces = (((span_end - span_start) / MAX_WINDOW_SECONDS).ceil() as usize).max(1);
        let length = (span_end - span_start) / pieces as f64;
        for piece in 0..pieces {
            let start = span_start + piece as f64 * length;
            let end = if piece == pieces - 1 { span_end } else { start + length };
            let mut times: Vec<f64> = all.iter().copied().filter(|t| *t >= start && *t <= end).collect();
            if times.len() > MAX_WINDOW_FRAMES {
                let step = (times.len() - 1) as f64 / (MAX_WINDOW_FRAMES - 1) as f64;
                times = (0..MAX_WINDOW_FRAMES).map(|k| times[(k as f64 * step).round() as usize]).collect();
            }
            if times.len() < 2 {
                let mut nearest = all.clone();
                nearest.sort_by(|a, b| (a - start).abs().partial_cmp(&(b - start).abs()).unwrap_or(std::cmp::Ordering::Equal));
                for t in nearest.into_iter().take(2) {
                    if !times.contains(&t) {
                        times.push(t);
                    }
                }
                times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            }
            if times.len() < 2 {
                continue;
            }
            windows.push(Window { start: start.min(times[0]), end: end.max(times[times.len() - 1]), times });
        }
    }
    windows
}

fn add(value: &[f32], sum: &mut [f32]) {
    if value.len() == sum.len() {
        for (s, v) in sum.iter_mut().zip(value) {
            *s += v;
        }
    }
}

pub fn luma_change(lhs: &[u8], rhs: &[u8]) -> f64 {
    if lhs.len() != rhs.len() || lhs.is_empty() {
        return 0.0;
    }
    let total: u64 = lhs.iter().zip(rhs).map(|(a, b)| (*a as i64 - *b as i64).unsigned_abs()).sum();
    total as f64 / (lhs.len() as f64 * 255.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seconds: f64, angle: f32, change: f64) -> Frame {
        Frame { seconds, embedding: vec![angle.cos(), angle.sin(), 0.0, 0.0], change }
    }

    #[test]
    fn cuts_scenes_on_embedding_jumps() {
        let frames: Vec<Frame> = (0..12).map(|i| frame(i as f64 * 0.5, if i < 6 { 0.0 } else { 1.5 }, 0.0)).collect();
        let found = scenes(&frames, 6.0);
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].start, found[0].end), (0.0, 3.0));
        assert_eq!((found[1].start, found[1].end), (3.0, 6.0));
        assert!(found.iter().all(|s| s.frames.contains(&s.representative)));
    }

    #[test]
    fn ignores_a_short_flash() {
        let frames = vec![frame(0.0, 0.0, 0.0), frame(0.5, 1.5, 0.0), frame(1.0, 0.0, 0.0), frame(1.5, 0.0, 0.0), frame(2.0, 0.0, 0.0)];
        assert!(scenes(&frames, 2.5).len() <= 2);
        assert!(scenes(&[], 2.0).is_empty());
    }

    #[test]
    fn activity_windows_cover_only_motion() {
        let frames: Vec<Frame> = (0..40).map(|i| frame(i as f64 * 0.5, 0.0, if (10..14).contains(&i) { 0.05 } else { 0.0 })).collect();
        let windows = activity_windows(&frames, 20.0);
        assert_eq!(windows.len(), 1);
        assert!(windows[0].start >= 4.0 && windows[0].end <= 7.5);
        assert!((2..=MAX_WINDOW_FRAMES).contains(&windows[0].times.len()));
        let still: Vec<Frame> = (0..10).map(|i| frame(i as f64, 0.0, 0.0)).collect();
        assert!(activity_windows(&still, 10.0).is_empty());
    }

    #[test]
    fn long_activity_splits_into_bounded_windows() {
        let frames: Vec<Frame> = (0..40).map(|i| frame(i as f64 * 0.5, 0.0, 0.1)).collect();
        let windows = activity_windows(&frames, 20.0);
        assert!(windows.len() >= 5);
        assert!(windows.iter().all(|w| w.end - w.start <= MAX_WINDOW_SECONDS + 1.01 && w.times.len() <= MAX_WINDOW_FRAMES));
        assert!(windows.iter().all(|w| w.times.windows(2).all(|p| p[0] < p[1])));
    }

    #[test]
    fn pool_times_stay_inside_the_video() {
        assert_eq!(pool_times(2.2, POOL_INTERVAL), vec![0.0, 0.5, 1.0, 1.5, 2.0]);
        assert!(pool_times(0.0, POOL_INTERVAL).is_empty());
        assert!((luma_change(&[0, 255], &[255, 255]) - 0.5).abs() < 1e-9);
    }
}
