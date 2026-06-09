//! On-demand video thumbnail handler. Extracts a 25%-duration keyframe via
//! Media Foundation, resizes it to a 192px (long-side, aspect-preserved) JPEG,
//! base64-encodes it, and replies with a `thumbnailGenerated` event. On ANY
//! failure the engine emits nothing — the app keeps its placeholder — and logs
//! a redacted warning.

use std::io::Cursor;

use base64::Engine as _;

use crate::ipc::{sink::Sink, EventPayload, GenerateVideoThumbnailPayload, IpcEvent, ThumbnailGenerated, Wrap};

/// Long side (in px) of the generated thumbnail. Matches the app's cache spec.
const THUMB_LONG_SIDE: u32 = 192;

pub(crate) async fn handle_generate_video_thumbnail(sink: Sink, payload: GenerateVideoThumbnailPayload) {
    let GenerateVideoThumbnailPayload { path, modified_at } = payload;

    match build_thumbnail_b64(&path).await {
        Ok(bytes) => {
            sink.send(IpcEvent::now(EventPayload::ThumbnailGenerated(Wrap::new(
                ThumbnailGenerated { path, modified_at, bytes },
            ))))
            .await;
        }
        Err(err) => {
            tracing::warn!(
                %err,
                file = %crate::platform::redact_path_for_log(&path),
                "video thumbnail generation failed; emitting nothing"
            );
        }
    }
}

/// Decode a video keyframe → resize to 192px long-side JPEG → base64. Runs the
/// blocking keyframe extract on a `spawn_blocking` thread; never panics.
async fn build_thumbnail_b64(path: &str) -> anyhow::Result<String> {
    let p = std::path::PathBuf::from(path);
    let frame = tokio::task::spawn_blocking(move || crate::shell::video::keyframe_25pct(&p)).await??;

    let crate::shell::video::VideoFrame { width, height, rgb, .. } = frame;
    let src = image::RgbImage::from_raw(width, height, rgb)
        .ok_or_else(|| anyhow::anyhow!("video frame buffer mismatch"))?;

    let (dst_w, dst_h) = fit_long_side(width, height, THUMB_LONG_SIDE);
    let resized = image::imageops::resize(&src, dst_w, dst_h, image::imageops::FilterType::Triangle);

    let mut buf: Vec<u8> = Vec::new();
    image::DynamicImage::ImageRgb8(resized)
        .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Jpeg)?;

    Ok(base64::engine::general_purpose::STANDARD.encode(&buf))
}

/// Scale `(w, h)` so the long side equals `target`, preserving aspect ratio.
/// Never upscales beyond the source on the long side; clamps each side to >=1.
fn fit_long_side(w: u32, h: u32, target: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (1, 1);
    }
    if w >= h {
        let scale = target as f64 / w as f64;
        (target.max(1), ((h as f64 * scale).round() as u32).max(1))
    } else {
        let scale = target as f64 / h as f64;
        (((w as f64 * scale).round() as u32).max(1), target.max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::fit_long_side;

    #[test]
    fn landscape_fits_long_side_to_width() {
        assert_eq!(fit_long_side(1920, 1080, 192), (192, 108));
    }

    #[test]
    fn portrait_fits_long_side_to_height() {
        assert_eq!(fit_long_side(1080, 1920, 192), (108, 192));
    }

    #[test]
    fn square_maps_both_to_target() {
        assert_eq!(fit_long_side(500, 500, 192), (192, 192));
    }

    #[test]
    fn zero_dimensions_clamp_to_one() {
        assert_eq!(fit_long_side(0, 100, 192), (1, 1));
        assert_eq!(fit_long_side(100, 0, 192), (1, 1));
    }

    #[test]
    fn extreme_aspect_clamps_short_side_to_one() {
        let (w, h) = fit_long_side(4000, 10, 192);
        assert_eq!(w, 192);
        assert_eq!(h, 1);
    }
}
