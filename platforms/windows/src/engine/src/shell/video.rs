// Video keyframe extraction via Media Foundation IMFSourceReader: one frame at 25 % of duration (±0.5 s) for Library thumbnails and the video preview placeholder.

use anyhow::{Context, Result};
use std::path::Path;
use std::sync::Once;

use windows::core::{PCWSTR, GUID};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, IMFMediaType, IMFSourceReader, MFCreateAttributes, MFCreateMediaType,
    MFCreateSourceReaderFromURL, MFStartup, MFVideoFormat_RGB32, MFMediaType_Video,
    MF_API_VERSION, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE, MF_MT_SUBTYPE,
    MF_PD_DURATION, MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_SOURCE_READER_MEDIASOURCE,
    MFSTARTUP_FULL,
};

const READF_ENDOFSTREAM: u32 = 0x00000002;
const READF_NEWSTREAM: u32 = 0x00000004;
const READF_CURRENTMEDIATYPECHANGED: u32 = 0x00000010;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;

static MF_INIT: Once = Once::new();

pub(crate) const VIDEO_DECODE_RESERVATION_BYTES: usize = 64 * 1024 * 1024;
const VIDEO_DECODE_BYTES_PER_PIXEL: u64 = 7;

fn video_decode_peak_bytes(width: u32, height: u32) -> Option<u64> {
    u64::from(width)
        .checked_mul(u64::from(height))?
        .checked_mul(VIDEO_DECODE_BYTES_PER_PIXEL)
}

fn video_frame_fits_reservation(width: u32, height: u32) -> bool {
    width > 0
        && height > 0
        && video_decode_peak_bytes(width, height)
            .is_some_and(|bytes| bytes <= VIDEO_DECODE_RESERVATION_BYTES as u64)
}

/// Balances a successful `CoInitializeEx` with `CoUninitialize` on drop, scoping the COM apartment to one `keyframe_25pct` call.
/// Keyframe extraction runs on recycled tokio blocking-pool and decoder-pool threads. Leaving such a thread MTA let a later shell op (trash / tags / thumbnail) hit RPC_E_CHANGED_MODE on its own STA init and run in the wrong apartment; per-call init with uninit on every exit path leaves the thread as it was.
/// `did_init` is true only when we performed the init (S_OK / S_FALSE); on RPC_E_CHANGED_MODE an outer caller owns the apartment and ref count, so we must not uninitialize.
struct ComScope {
    did_init: bool,
}

impl ComScope {
    fn enter() -> Self {
        // MTA: these worker threads don't pump a message loop. A prior STA init
        // on the thread yields RPC_E_CHANGED_MODE — MF still works against the
        // existing apartment, we just don't own (or release) it.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        ComScope { did_init: hr.is_ok() }
    }
}

impl Drop for ComScope {
    fn drop(&mut self) {
        if self.did_init {
            unsafe { CoUninitialize() };
        }
    }
}

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
    pub rgb: Vec<u8>,
}

fn scaled_video_dimensions(width: u32, height: u32, max_edge: u32) -> Option<(u32, u32)> {
    let longest = width.max(height);
    if width == 0 || height == 0 || max_edge < 2 || longest <= max_edge {
        return None;
    }
    let scale = max_edge as f64 / longest as f64;
    let even = |value: u32| value.max(2) & !1;
    Some((
        even((width as f64 * scale).round() as u32),
        even((height as f64 * scale).round() as u32),
    ))
}

pub fn keyframe_25pct(path: &Path) -> Result<VideoFrame> {
    let _com = ComScope::enter();
    ensure_mf_started();

    // Media Foundation accepts ordinary DOS paths but rejects the Win32 `\\?\`
    // namespace for common local videos. Preserve the ordinary form; long-path
    // support requires a byte-stream source rather than rewriting every URL.
    let path_str = path.to_str().context("video path must be UTF-8")?;
    let mut wide: Vec<u16> = path_str.encode_utf16().collect();
    wide.push(0);

    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 2).context("MFCreateAttributes")?;
        let attrs = attrs.context("attrs not initialized")?;
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1)
            .context("enable scaled video processing")?;

        let reader: IMFSourceReader =
            MFCreateSourceReaderFromURL(PCWSTR::from_raw(wide.as_ptr()), &attrs)
                .context("MFCreateSourceReaderFromURL")?;

        let media_type: IMFMediaType = MFCreateMediaType().context("MFCreateMediaType")?;
        media_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .context("set major type")?;
        media_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .context("set subtype")?;

        reader
            .SetStreamSelection(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, true)
            .context("select video stream")?;
        let native = reader
            .GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0)
            .context("get native video type")?;
        let native_size = native.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
        let native_width = (native_size >> 32) as u32;
        let native_height = (native_size & 0xFFFF_FFFF) as u32;
        anyhow::ensure!(
            native_width > 0 && native_height > 0,
            "native video dimensions are unavailable"
        );
        let scaled = scaled_video_dimensions(native_width, native_height, 1_280);
        if let Some((width, height)) = scaled {
            let packed = (u64::from(width) << 32) | u64::from(height);
            media_type
                .SetUINT64(&MF_MT_FRAME_SIZE, packed)
                .context("set scaled video frame size")?;
        }
        if let Err(scaled_error) = reader.SetCurrentMediaType(
            MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
            None,
            &media_type,
        ) {
            if scaled.is_none() {
                return Err(scaled_error).context("set current media type RGB32");
            }
            anyhow::ensure!(
                video_frame_fits_reservation(native_width, native_height),
                "scaled video negotiation failed and the native frame exceeds the decode memory reservation"
            );
            media_type.DeleteItem(&MF_MT_FRAME_SIZE)?;
            reader
                .SetCurrentMediaType(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    None,
                    &media_type,
                )
                .context("set unscaled fallback media type RGB32")?;
        }

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
        // Legacy MPEG program streams (the `.mpg` files common in older family
        // archives) often expose a duration but do not support a non-keyframe
        // MF seek. Reading from 25% then yields only format notifications and
        // no sample. Start at zero for those containers; the first decodable
        // frame is still a valid preview and keeps the whole archive usable.
        let target_100ns = if seek_to_quarter(path) {
            (duration_100ns / 4).max(0)
        } else {
            0
        };

        if target_100ns > 0 {
            let pv: PROPVARIANT = i64_to_propvariant(target_100ns);
            let _ = reader.SetCurrentPosition(&GUID::zeroed(), &pv);
            let _ = PropVariantClear(&pv as *const _ as *mut _);
        }

        let mut last_dims: Option<(u32, u32)> = None;
        for _ in 0..32 {
            let mut stream_index = 0u32;
            let mut flags = 0u32;
            let mut sample = None;
            reader
                .ReadSample(
                    MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                    0,
                    Some(&mut stream_index),
                    Some(&mut flags),
                    None,
                    Some(&mut sample),
                )
                .context("ReadSample")?;

            if (flags & READF_ENDOFSTREAM) != 0 {
                break;
            }
            if (flags & (READF_CURRENTMEDIATYPECHANGED | READF_NEWSTREAM)) != 0 {
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
            anyhow::ensure!(
                video_frame_fits_reservation(w, h),
                "negotiated video frame exceeds the decode memory reservation"
            );

            let buffer = sample
                .ConvertToContiguousBuffer()
                .context("ConvertToContiguousBuffer")?;
            let mut p_data: *mut u8 = std::ptr::null_mut();
            let mut max_len = 0u32;
            let mut cur_len = 0u32;
            buffer
                .Lock(&mut p_data, Some(&mut max_len), Some(&mut cur_len))
                .context("buffer Lock")?;

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
            });
        }

        anyhow::bail!("ReadSample produced no frame after 32 attempts")
    }
}

fn seek_to_quarter(path: &Path) -> bool {
    !matches!(
        path.extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.to_ascii_lowercase())
            .as_deref(),
        Some("mpg" | "mpeg" | "vob")
    )
}

fn propvariant_to_i64(pv: &PROPVARIANT) -> Option<i64> {
    // MF_PD_DURATION arrives as VT_UI8 (UINT64): try the unsigned conversion first and saturate into i64, otherwise the i64 TryFrom fails, the duration resolves to 0 and the keyframe is taken from frame 0 instead of 25%.
    if let Ok(u) = u64::try_from(pv) {
        return Some(u.min(i64::MAX as u64) as i64);
    }
    i64::try_from(pv).ok()
}

fn i64_to_propvariant(v: i64) -> PROPVARIANT {
    PROPVARIANT::from(v)
}

#[cfg(test)]
mod tests {
    use super::{
        scaled_video_dimensions, seek_to_quarter, video_frame_fits_reservation, ComScope,
        VIDEO_DECODE_RESERVATION_BYTES,
    };
    use std::path::Path;

    #[test]
    fn video_dimensions_scale_to_an_even_working_resolution() {
        assert_eq!(scaled_video_dimensions(3840, 2160, 1280), Some((1280, 720)));
        assert_eq!(scaled_video_dimensions(2160, 3840, 1280), Some((720, 1280)));
        assert_eq!(scaled_video_dimensions(1920, 1080, 1280), Some((1280, 720)));
        assert_eq!(scaled_video_dimensions(640, 480, 1280), None);
        assert_eq!(scaled_video_dimensions(0, 480, 1280), None);
    }

    #[test]
    fn unscaled_fallback_cannot_exceed_reserved_bgra_plus_rgb_memory() {
        assert!(video_frame_fits_reservation(3840, 2160));
        assert!(!video_frame_fits_reservation(7680, 4320));
        assert!(!video_frame_fits_reservation(0, 1080));
        assert!(!video_frame_fits_reservation(1920, 0));
        let boundary_pixels = VIDEO_DECODE_RESERVATION_BYTES as u64 / 7;
        assert!(video_frame_fits_reservation(boundary_pixels as u32, 1));
        assert!(!video_frame_fits_reservation(boundary_pixels as u32 + 1, 1));
    }

    #[test]
    fn legacy_mpeg_streams_start_at_a_decodable_keyframe() {
        assert!(!seek_to_quarter(Path::new("family.mpg")));
        assert!(!seek_to_quarter(Path::new("family.MPEG")));
        assert!(!seek_to_quarter(Path::new("family.vob")));
        assert!(seek_to_quarter(Path::new("family.mp4")));
    }
    use windows::Win32::System::Com::{
        CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
    };

    /// Regression for the recycled-thread apartment leak: a `ComScope` must uninitialize the MTA apartment it created so the same thread can later enter an STA (as shell trash/tags/thumbnail ops do) without RPC_E_CHANGED_MODE. Runs on a dedicated thread so the harness's apartment can't interfere.
    #[test]
    fn com_scope_uninitializes_so_thread_can_re_enter_sta() {
        let handle = std::thread::spawn(|| {
            {
                let com = ComScope::enter();
                assert!(com.did_init, "fresh thread should perform the MTA init");
            }
            unsafe {
                let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                assert!(
                    hr.is_ok(),
                    "ComScope leaked its MTA apartment: STA re-entry was refused ({hr:?})"
                );
                CoUninitialize();
            }
        });
        handle.join().expect("com scope test thread panicked");
    }
}
