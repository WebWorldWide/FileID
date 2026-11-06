//! On-demand video thumbnail handler. Extracts a 25%-duration keyframe via
//! Media Foundation, resizes it to a JPEG whose long side is the requested size
//! (192px by default, aspect-preserved, never upscaled),
//! base64-encodes it, and replies with a `thumbnailGenerated` event. On ANY
//! failure the engine emits nothing — the app keeps its placeholder — and logs
//! a redacted warning.

use std::io::Cursor;
use std::sync::OnceLock;

use base64::Engine as _;

use crate::ipc::{sink::Sink, EventPayload, GenerateVideoThumbnailPayload, IpcEvent, ThumbnailGenerated, Wrap};

/// Long side (in px) when the request names none. Requests are clamped to
/// `MIN_LONG_SIDE..=MAX_LONG_SIDE`.
const THUMB_LONG_SIDE: u32 = 192;
const MIN_LONG_SIDE: u32 = 64;
const MAX_LONG_SIDE: u32 = 2048;

/// Cap on concurrent Media Foundation extracts. Each one holds a full-res
/// decoded frame (up to ~192 MB at MAX_VIDEO_PIXELS) plus a D3D video-decode
/// context for 1-10 s, and a cache-cold scroll through a video folder fires
/// hundreds of these commands in seconds — unbounded, they piled up on
/// tokio's 512-thread blocking pool and OOMed the Low-memory target. Mirrors
/// the scan pipeline's GPU semaphores (vision=4 / clip=2).
const MAX_CONCURRENT_EXTRACTS: usize = 2;

fn extract_semaphore() -> &'static tokio::sync::Semaphore {
    static SEM: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
    SEM.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_EXTRACTS))
}

pub(crate) async fn handle_generate_video_thumbnail(sink: Sink, payload: GenerateVideoThumbnailPayload) {
    let GenerateVideoThumbnailPayload { path, modified_at, max_pixel_size } = payload;

    match build_thumbnail_b64(&path, requested_long_side(max_pixel_size)).await {
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

fn requested_long_side(max_pixel_size: Option<i64>) -> u32 {
    max_pixel_size.map_or(THUMB_LONG_SIDE, |size| size.clamp(MIN_LONG_SIDE as i64, MAX_LONG_SIDE as i64) as u32)
}

/// Decode a video keyframe → resize to a `long_side` JPEG → base64. Runs the
/// blocking keyframe extract on a `spawn_blocking` thread; never panics.
async fn build_thumbnail_b64(path: &str, long_side: u32) -> anyhow::Result<String> {
    let _permit = extract_semaphore().acquire().await?;
    let p = std::path::PathBuf::from(path);
    let frame = tokio::task::spawn_blocking(move || crate::shell::video::keyframe_25pct(&p)).await??;

    let crate::shell::video::VideoFrame { width, height, rgb, .. } = frame;
    let src = image::RgbImage::from_raw(width, height, rgb)
        .ok_or_else(|| anyhow::anyhow!("video frame buffer mismatch"))?;

    let (dst_w, dst_h) = fit_long_side(width, height, long_side);
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
    let target = target.min(w.max(h));
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
    use super::{fit_long_side, requested_long_side};

    #[test]
    fn requested_size_defaults_and_clamps() {
        assert_eq!(requested_long_side(None), 192);
        assert_eq!(requested_long_side(Some(1024)), 1024);
        assert_eq!(requested_long_side(Some(8)), 64);
        assert_eq!(requested_long_side(Some(-5)), 64);
        assert_eq!(requested_long_side(Some(100_000)), 2048);
    }

    #[test]
    fn larger_requests_are_honoured_but_never_upscale() {
        assert_eq!(fit_long_side(1920, 1080, 1024), (1024, 576));
        assert_eq!(fit_long_side(320, 180, 1024), (320, 180));
    }

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
