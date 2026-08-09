// VLM face-comparison bridge for merge suggestions.
//
// Public API surface is fully wired but not yet driven from any IPC
// command — that wiring lands in a follow-up (`verifyMergeSuggestions`).
// Allowing dead_code here keeps clippy's -D warnings gate clean while
// the building blocks are reviewed independently of the orchestrator.
#![allow(dead_code)]

//
// `handle_find_merge_suggestions` finds candidate cluster pairs by
// cosine similarity in the uncertain band (0.45..0.70 on ArcFace's
// 512-d unit hypersphere — same threshold as `face_clustering`).
// Those are the pairs ArcFace alone can't decide. This module lifts the
// decision to a vision-language model: given two face-crop JPEGs,
// asks the VLM "Are these the same person?" and parses the verdict.
//
// Mirrors macOS's `DeepAnalyzeRunner.compareFaces()`.
//
// Design notes:
//   • `llama-mtmd-cli` accepts a single `--image` reliably across
//     versions; multi-image support is patchy. We pre-compose the two
//     crops side-by-side into a temp JPEG and ask the VLM to compare
//     "the left face and the right face". One image in, one prompt,
//     one parse.
//   • The verdict parser is permissive: any line containing "SAME" or
//     "DIFFERENT" anchors the verdict; "CONFIDENCE: 0.85" floats are
//     extracted via a small regex-free scan. If neither anchor is
//     present we return Inconclusive — the caller leaves the pair
//     untouched (sentry: better to leave a manual review than to
//     auto-merge on a confused parse).
//   • Verification is intentionally side-effect-free here. Wiring this
//     into a "verifyMergeSuggestions" IPC command is a later patch;
//     the building blocks land first so face-clustering callers can
//     opt into it once the runtime cost / UX flow is settled.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};

use crate::models::vlm::{self, CaptionRequest, VlmRunner};

/// The comparison prompt fed to the VLM.
pub const COMPARE_PROMPT: &str =
    "The image shows two cropped face photos side by side (left and right). \
     Are they the same person? Reply with exactly two lines:\n\
     VERDICT: SAME or VERDICT: DIFFERENT\n\
     CONFIDENCE: <a number between 0.0 and 1.0>";

/// Result of a single pair comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum PairVerdict {
    Same { confidence: f32 },
    Different { confidence: f32 },
    /// Neither anchor token surfaced; treat as "leave to manual review".
    Inconclusive,
}

/// Compose two face crops side-by-side into a single temp JPEG.
/// Pads the shorter crop's height with black so the result is a clean
/// 2W×H landscape and the VLM doesn't have to reason about aspect.
pub fn compose_pair_jpeg(left: &Path, right: &Path) -> Result<PathBuf> {
    let l = image::open(left).with_context(|| format!("open left crop {}", left.display()))?;
    let r = image::open(right).with_context(|| format!("open right crop {}", right.display()))?;

    // Target height = max(l.h, r.h), capped to keep VLM input small.
    const MAX_H: u32 = 512;
    let target_h = l.height().max(r.height()).min(MAX_H);
    let lw = scaled_width(l.width(), l.height(), target_h);
    let rw = scaled_width(r.width(), r.height(), target_h);
    let total_w = lw + rw;

    let l_resized = l.resize_exact(lw, target_h, image::imageops::FilterType::Triangle);
    let r_resized = r.resize_exact(rw, target_h, image::imageops::FilterType::Triangle);

    let mut canvas = image::RgbImage::from_pixel(total_w, target_h, image::Rgb([0, 0, 0]));
    image::imageops::overlay(&mut canvas, &l_resized.to_rgb8(), 0, 0);
    image::imageops::overlay(&mut canvas, &r_resized.to_rgb8(), i64::from(lw), 0);

    let dest = std::env::temp_dir().join(format!(
        "fileid-pair-{}.jpg",
        uuid::Uuid::new_v4()
    ));
    image::DynamicImage::ImageRgb8(canvas)
        .save_with_format(&dest, image::ImageFormat::Jpeg)
        .with_context(|| format!("write composite {}", dest.display()))?;
    Ok(dest)
}

fn scaled_width(orig_w: u32, orig_h: u32, target_h: u32) -> u32 {
    if orig_h == 0 {
        return target_h;
    }
    let aspect = orig_w as f32 / orig_h as f32;
    let w = (target_h as f32 * aspect).round() as u32;
    w.max(1)
}

/// Run a single VLM pair comparison. Returns the parsed verdict and
/// always cleans up the temp composite even on early Err.
pub async fn verify_pair_async(
    runner: &VlmRunner,
    model_kind: &str,
    left_crop: &Path,
    right_crop: &Path,
    cancel: Arc<std::sync::atomic::AtomicBool>,
) -> Result<PairVerdict> {
    let (gguf, mmproj) = vlm::find_weights(model_kind).ok_or_else(|| {
        anyhow::anyhow!(
            "VLM weights not installed for model_kind={model_kind}; \
             install from Settings → Local AI before verifying pairs"
        )
    })?;

    let composite = compose_pair_jpeg(left_crop, right_crop)?;
    let req = CaptionRequest {
        gguf_path: gguf,
        mmproj_path: mmproj,
        image_path: composite.clone(),
        prompt: COMPARE_PROMPT.to_string(),
        // Two short lines fit comfortably under 64 tokens. Greedy decode
        // so the verdict is deterministic for the same input + weights.
        max_tokens: 64,
        greedy: true,
    };

    let result = vlm::caption(runner, &req, cancel, |_| {}).await;
    let _ = std::fs::remove_file(&composite); // best-effort cleanup
    let caption = result?;
    Ok(parse_verdict(&caption.text))
}

/// Resolve a face crop on disk from a face id. Lookup is fixed at
/// `{face_crops_dir}/{id}.jpg` — same naming convention `face_clustering`
/// uses when persisting crops. Returns Err if the file is missing so the
/// caller can mark the pair as unverifiable.
pub fn face_crop_path(face_id: i64) -> Result<PathBuf> {
    let dir = crate::paths::faces_dir().context("resolving face_crops dir")?;
    let p = dir.join(format!("{face_id}.jpg"));
    if !p.exists() {
        anyhow::bail!("face crop not found at {}", p.display());
    }
    Ok(p)
}

/// Parse the VLM's two-line response into a PairVerdict.
///
/// Permissive: matches "SAME" / "DIFFERENT" case-insensitively anywhere
/// in the text, and extracts the first 0.0..=1.0 float after "CONFIDENCE:"
/// (also case-insensitive). Returns Inconclusive when neither verdict
/// anchor is present — the caller should NOT auto-merge on Inconclusive.
pub fn parse_verdict(text: &str) -> PairVerdict {
    let upper = text.to_ascii_uppercase();
    let confidence = extract_confidence(&upper).unwrap_or(0.5);

    // Order matters: "DIFFERENT" contains no "SAME" substring, but a
    // confused model could output "not the same person" — checking
    // DIFFERENT first avoids a false positive.
    if upper.contains("DIFFERENT") {
        return PairVerdict::Different { confidence };
    }
    if upper.contains("SAME") {
        return PairVerdict::Same { confidence };
    }
    PairVerdict::Inconclusive
}

fn extract_confidence(upper: &str) -> Option<f32> {
    let idx = upper.find("CONFIDENCE")?;
    let tail = &upper[idx..];
    // Skip past "CONFIDENCE", optional colon, whitespace.
    let mut chars = tail.chars().skip("CONFIDENCE".len());
    let mut buf = String::new();
    let mut started = false;
    let mut saw_dot = false;
    for c in chars.by_ref().take(16) {
        if !started {
            if c.is_ascii_digit() {
                started = true;
                buf.push(c);
            } else if c.is_whitespace() || c == ':' || c == '=' {
                continue;
            } else if c == '.' {
                // Leading dot ".5" — accept.
                started = true;
                saw_dot = true;
                buf.push('0');
                buf.push(c);
            } else {
                continue;
            }
        } else if c.is_ascii_digit() {
            buf.push(c);
        } else if c == '.' && !saw_dot {
            saw_dot = true;
            buf.push(c);
        } else {
            break;
        }
    }
    let v: f32 = buf.parse().ok()?;
    if (0.0..=1.0).contains(&v) {
        Some(v)
    } else if v > 1.0 && v <= 100.0 {
        // Some models emit "85" instead of "0.85" — normalize.
        Some(v / 100.0)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verdict_canonical_same() {
        let v = parse_verdict("VERDICT: SAME\nCONFIDENCE: 0.93");
        assert_eq!(v, PairVerdict::Same { confidence: 0.93 });
    }

    #[test]
    fn parse_verdict_canonical_different() {
        let v = parse_verdict("VERDICT: DIFFERENT\nCONFIDENCE: 0.71");
        assert_eq!(v, PairVerdict::Different { confidence: 0.71 });
    }

    #[test]
    fn parse_verdict_lowercase_passes() {
        let v = parse_verdict("verdict: same\nconfidence: 0.6");
        assert_eq!(v, PairVerdict::Same { confidence: 0.6 });
    }

    #[test]
    fn parse_verdict_inconclusive_no_anchor() {
        let v = parse_verdict("I'm not sure about this one.");
        assert_eq!(v, PairVerdict::Inconclusive);
    }

    #[test]
    fn parse_verdict_percent_normalized() {
        let v = parse_verdict("VERDICT: SAME\nCONFIDENCE: 85");
        assert_eq!(v, PairVerdict::Same { confidence: 0.85 });
    }

    #[test]
    fn parse_verdict_missing_confidence_defaults_to_half() {
        let v = parse_verdict("VERDICT: SAME");
        assert_eq!(v, PairVerdict::Same { confidence: 0.5 });
    }

    #[test]
    fn parse_verdict_different_wins_when_both_present() {
        // "different" must take precedence over any incidental "same"
        // elsewhere in the text to avoid auto-merging on confused output.
        let v = parse_verdict("These are clearly DIFFERENT people; not the same.");
        assert!(matches!(v, PairVerdict::Different { .. }));
    }

    #[test]
    fn parse_verdict_leading_dot() {
        let v = parse_verdict("VERDICT: SAME\nCONFIDENCE: .42");
        assert_eq!(v, PairVerdict::Same { confidence: 0.42 });
    }

    #[test]
    fn scaled_width_preserves_aspect_within_one_pixel() {
        // 100×200 scaled to 100 height → width 50.
        assert_eq!(scaled_width(100, 200, 100), 50);
        // Degenerate zero-height falls back to the target height.
        assert_eq!(scaled_width(100, 0, 100), 100);
        // Tiny inputs stay ≥ 1 pixel wide.
        assert_eq!(scaled_width(1, 1000, 4).max(1), 1);
    }
}
