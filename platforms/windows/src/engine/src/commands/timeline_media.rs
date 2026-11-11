//! The timeline's video decoder: the vetted FFmpeg runtime on Windows and
//! Linux, AVFoundation worker processes in the Mac app's engine
//! (`crate::apple::media`).

use super::video_export::AnalysisProbe;
use anyhow::Result;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// Whether this engine can decode video for timeline analysis.
pub(crate) fn available() -> bool {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    return true;
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    super::video_export::available()
}

/// Duration, displayed size and whether there is audio.
pub(crate) fn probe(source: &Path, cancel: &AtomicBool) -> Result<AnalysisProbe> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        let _ = cancel;
        let probe = crate::apple::media::probe(source, None, false)?;
        anyhow::ensure!(probe.video_count > 0, "The file has no video stream");
        Ok(AnalysisProbe {
            duration: probe.duration,
            width: probe.width.round() as u32,
            height: probe.height.round() as u32,
            has_audio: probe.audio_count > 0,
        })
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    super::video_export::analysis_probe(source, cancel)
}

/// One frame at `seconds`, `size` pixels, written to `output` as a JPEG.
pub(crate) fn frame_jpeg(source: &Path, seconds: f64, size: (u32, u32), output: &Path, cancel: &AtomicBool) -> Result<()> {
    #[cfg(all(target_os = "macos", feature = "apple-adapters"))]
    {
        crate::apple::media::sample_frame(source, seconds, size, output, cancel)?;
        Ok(())
    }
    #[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
    {
        let rgb = super::video_export::frame_rgb(source, seconds, size.0, size.1, cancel)?;
        let image = image::RgbImage::from_raw(size.0, size.1, rgb).ok_or_else(|| anyhow::anyhow!("Invalid decoded frame"))?;
        image.save_with_format(output, image::ImageFormat::Jpeg)?;
        Ok(())
    }
}

/// The Mac engine samples a whole video once into a frame pool, as the Swift
/// engine does: half-second signatures for activity windows and small JPEGs
/// for the moment windows, from one sequential hardware decode.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
pub(crate) const FRAME_POOL: bool = true;
#[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
pub(crate) const FRAME_POOL: bool = false;

/// The pool's frames: `(seconds, signature, jpeg)`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Pool {
    pub(crate) frames: Vec<(f64, Vec<u8>, std::path::PathBuf)>,
}

impl Pool {
    /// The pool frame closest to `seconds`.
    pub(crate) fn nearest(&self, seconds: f64) -> Option<&std::path::Path> {
        self.frames
            .iter()
            .min_by(|a, b| (a.0 - seconds).abs().total_cmp(&(b.0 - seconds).abs()))
            .map(|frame| frame.2.as_path())
    }
}

/// Samples the frame pool into `directory`.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
pub(crate) fn pool(source: &Path, duration: f64, directory: &Path, cancel: &AtomicBool) -> Result<Pool> {
    let manifest = crate::apple::media::sample_pool(
        source,
        crate::pipeline::video_scene_selection::POOL_INTERVAL,
        POOL_MAX_SIDE,
        directory,
        duration,
        cancel,
    )?;
    Ok(Pool { frames: manifest.frames.into_iter().map(|frame| (frame.seconds, frame.signature, directory.join(frame.file))).collect() })
}

#[cfg(not(all(target_os = "macos", feature = "apple-adapters")))]
pub(crate) fn pool(_: &Path, _: f64, _: &Path, _: &AtomicBool) -> Result<Pool> {
    anyhow::bail!("This engine samples video signals in chunks")
}

/// The Swift engine's `VideoFramePool.maxPixelSize`.
#[cfg(all(target_os = "macos", feature = "apple-adapters"))]
pub(crate) const POOL_MAX_SIDE: u32 = 448;

/// The Swift engine's `TimelineAnalysis.poolFrames`: a CLIP vector for every
/// pool frame when the image encoder is installed, else the frame's luma
/// signature minus its mean; `change` is the luma change from the previous
/// pool frame. Scenes cut on these vectors; activity windows read `change`.
#[cfg_attr(not(all(target_os = "macos", feature = "apple-adapters")), allow(dead_code))]
pub(crate) fn scene_frames(pool: &Pool) -> Vec<crate::pipeline::video_scene_selection::Frame> {
    use crate::pipeline::video_scene_selection::{luma_change, Frame};
    let mut clip = crate::models::mobileclip::default_weights_path()
        .ok()
        .filter(|path| path.is_file())
        .and_then(|path| crate::models::mobileclip::MobileClipImage::load(path).ok());
    let mut previous: Option<&[u8]> = None;
    pool.frames
        .iter()
        .map(|(seconds, signature, path)| {
            let embedding = clip
                .as_mut()
                .and_then(|clip| {
                    let image = image::open(path).ok()?.resize_exact(224, 224, image::imageops::FilterType::Triangle).to_rgb8();
                    clip.embed(image.as_raw()).ok()
                })
                .unwrap_or_else(|| {
                    let mean = signature.iter().map(|&value| f32::from(value)).sum::<f32>() / signature.len().max(1) as f32;
                    signature.iter().map(|&value| f32::from(value) - mean).collect()
                });
            let change = previous.map_or(0.0, |before| luma_change(before, signature));
            previous = Some(signature);
            Frame { seconds: *seconds, embedding, change }
        })
        .collect()
}

/// The pool frames nearest each whole second: the 1 fps signal sequence the
/// chunked decoder produces.
pub(crate) fn whole_seconds(pool: &Pool) -> Vec<(f64, Vec<u8>)> {
    let mut picked: Vec<(f64, Vec<u8>)> = Vec::new();
    for (seconds, signature, _) in &pool.frames {
        let second = seconds.round();
        if (seconds - second).abs() > 0.25 {
            continue;
        }
        match picked.last() {
            Some((last, _)) if last.round() as i64 == second as i64 => {}
            _ => picked.push((*seconds, signature.clone())),
        }
    }
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_seconds_keep_one_frame_per_second_of_the_half_second_pool() {
        let pool = Pool {
            frames: [0.0, 0.5, 1.02, 1.5, 2.0, 2.5, 3.4]
                .iter()
                .map(|&s| (s, vec![s as u8], std::path::PathBuf::from(format!("{s}.jpg"))))
                .collect(),
        };
        let picked: Vec<f64> = whole_seconds(&pool).into_iter().map(|(s, _)| s).collect();
        assert_eq!(picked, [0.0, 1.02, 2.0]);
        assert_eq!(pool.nearest(1.3), Some(std::path::Path::new("1.5.jpg")));
        assert_eq!(pool.nearest(0.2), Some(std::path::Path::new("0.jpg")));
    }

    // Without the CLIP image encoder (tests install none), scene vectors are
    // the luma signatures minus their mean, as in the Swift engine.
    #[test]
    fn scene_frames_fall_back_to_centred_luma_with_luma_change() {
        let missing = std::path::PathBuf::from("/nonexistent/frame.jpg");
        let pool = Pool { frames: vec![(0.0, vec![10, 30], missing.clone()), (0.5, vec![10, 30], missing.clone()), (1.0, vec![255, 0], missing)] };
        let frames = scene_frames(&pool);
        assert_eq!(frames.iter().map(|f| f.seconds).collect::<Vec<_>>(), [0.0, 0.5, 1.0]);
        assert_eq!(frames[0].embedding, [-10.0, 10.0]);
        assert_eq!(frames[2].embedding, [127.5, -127.5]);
        assert!(frames[0].change.abs() < 1e-12 && frames[1].change.abs() < 1e-12);
        assert!((frames[2].change - (245.0 + 30.0) / (2.0 * 255.0)).abs() < 1e-12);
    }
}
