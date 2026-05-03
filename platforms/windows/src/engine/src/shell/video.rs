// Video keyframe extraction — Media Foundation IMFSourceReader.
//
// Mirror of macOS `AVAssetImageGenerator`. Pulls a single frame at a
// requested timestamp (we use 25% of duration with ±0.5 s tolerance,
// matching the macOS heuristic) so the Library grid has a thumbnail
// for video files and the video preview sheet has a placeholder.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Once;

use windows::core::{Interface, PCWSTR, PROPVARIANT, GUID};
use windows::Win32::Foundation::{S_OK, FALSE, TRUE};
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFStartup, MFVideoFormat_RGB32, MFMediaType_Video,
    MF_API_VERSION, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_PD_DURATION, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
    MFSTARTUP_FULL,
};

// Source-reader read sample status flags (MF_SOURCE_READERF_*).
const READF_ENDOFSTREAM: u32 = 0x00000002;
const READF_NEWSTREAM: u32 = 0x00000004;
const READF_CURRENTMEDIATYPECHANGED: u32 = 0x00000010;
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;

static MF_INIT: Once = Once::new();

fn ensure_mf_started() {
    MF_INIT.call_once(|| unsafe {
        // Best-effort startup. MF persists for the process lifetime once
        // started; we never call MFShutdown — process exit cleans up.
        let _ = MFStartup(MF_API_VERSION, MFSTARTUP_FULL);
    });
}

#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGB8.
    pub rgb: Vec<u8>,
    pub time_seconds: f64,
}

/// Extract a frame at 25% of duration. Returns the frame as RGB8.
pub fn keyframe_25pct(path: &Path) -> Result<VideoFrame> {
    ensure_mf_started();

    let path_str = path.to_str().context("video path must be UTF-8")?;
    let mut wide: Vec<u16> = path_str.encode_utf16().collect();
    wide.push(0);

    unsafe {
        // Source-reader attributes: enable hardware decoding + video
        // processing (lets MF do format conversion to RGB32).
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2).context("MFCreateAttributes")?;
        let attrs = attrs.context("attrs not initialized")?;
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .context("enable video processing")?;

        let reader: IMFSourceReader =
            MFCreateSourceReaderFromURL(PCWSTR::from_raw(wide.as_ptr()), &attrs)
                .context("MFCreateSourceReaderFromURL")?;

        // Configure video stream output to RGB32 (BGRA byte order).
        let media_type: IMFMediaType = MFCreateMediaType().context("MFCreateMediaType")?;
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .context("set major type")?;
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .context("set subtype")?;

        reader
            .SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, TRUE)
            .context("select video stream")?;
        reader
            .SetCurrentMediaType(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                None,
                &media_type,
            )
            .context("set current media type RGB32")?;

        // Pull duration from the source's presentation descriptor and
        // seek to 25%. Duration is in 100-ns units (MFTIME).
        let dur_pv = reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION);
        let duration_100ns: i64 = match dur_pv {
            Ok(pv) => {
                let v = propvariant_to_i64(&pv);
                let _ = PropVariantClear(&pv as *const _ as *mut _);
                v.unwrap_or(0)
            }
            Err(_) => 0,
        };
        let target_100ns = (duration_100ns / 4).max(0);

        if target_100ns > 0 {
            let pv: PROPVARIANT = i64_to_propvariant(target_100ns);
            let _ = reader.SetCurrentPosition(&GUID::zeroed(), &pv);
            let _ = PropVariantClear(&pv as *const _ as *mut _);
        }

        // Read until we get a non-empty video sample. Skip flags-only
        // returns (e.g. format-changed) and gap notifications.
        let mut last_dims: Option<(u32, u32)> = None;
        for _ in 0..32 {
            let mut stream_index = 0u32;
            let mut flags = 0u32;
            let mut timestamp = 0i64;
            let mut sample = None;
            reader
                .ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    Some(&mut stream_index),
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
                .context("ReadSample")?;

            if (flags & READF_ENDOFSTREAM) != 0 {
                break;
            }
            if (flags & (READF_CURRENTMEDIATYPECHANGED | READF_NEWSTREAM)) != 0 {
                // Format changed — re-pull the negotiated type to read dimensions.
                let cur = reader
                    .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                    .context("re-get media type after format change")?;
                let packed = cur.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
                let w = (packed >> 32) as u32;
                let h = (packed & 0xFFFF_FFFF) as u32;
                if w > 0 && h > 0 {
                    last_dims = Some((w, h));
                }
            }
            let Some(sample) = sample else { continue };

            let buffer = sample
                .ConvertToContiguousBuffer()
                .context("ConvertToContiguousBuffer")?;

            let (w, h) = match last_dims {
                Some(d) => d,
                None => {
                    let cur = reader
                        .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
                        .context("get media type for dims")?;
                    let packed = cur.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
                    let w = (packed >> 32) as u32;
                    let h = (packed & 0xFFFF_FFFF) as u32;
                    (w, h)
                }
            };
            if w == 0 || h == 0 {
                continue;
            }

            let mut p_data: *mut u8 = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            buffer
                .Lock(&mut p_data, Some(&mut max_len), Some(&mut cur_len))
                .context("buffer Lock")?;

            // RGB32 = BGRA. Strip alpha + reorder to RGB.
            let stride = (cur_len / h.max(1)) as usize;
            let pixel_bytes = 4usize;
            let row_pixel_bytes = (w as usize) * pixel_bytes;
            let mut rgb = vec![0u8; (w as usize) * (h as usize) * 3];
            let src = std::slice::from_raw_parts(p_data, cur_len as usize);
            for y in 0..(h as usize) {
                let s_off = y * stride;
                let d_off = y * (w as usize) * 3;
                if s_off + row_pixel_bytes > src.len() {
                    break;
                }
                for x in 0..(w as usize) {
                    let s = s_off + x * 4;
                    let d = d_off + x * 3;
                    rgb[d] = src[s + 2];     // R
                    rgb[d + 1] = src[s + 1]; // G
                    rgb[d + 2] = src[s];     // B
                }
            }
            let _ = buffer.Unlock();

            return Ok(VideoFrame {
                width: w,
                height: h,
                rgb,
                time_seconds: (timestamp as f64) / 10_000_000.0,
            });
        }

        anyhow::bail!("ReadSample produced no frame after 32 attempts")
    }
}

fn propvariant_to_i64(pv: &PROPVARIANT) -> Option<i64> {
    // PROPVARIANT impls TryFrom for the integer variants; round-trip
    // through &PROPVARIANT which the windows-rs macros convert.
    i64::try_from(pv).ok()
}

fn i64_to_propvariant(v: i64) -> PROPVARIANT {
    // PROPVARIANT impls From<i64>; type tag = VT_I8.
    PROPVARIANT::from(v)
}

// silence: S_OK / FALSE imports are exported above for downstream parity
// with shell helpers; not all are referenced here.
const _: () = {
    let _ = S_OK;
    let _ = FALSE;
};
