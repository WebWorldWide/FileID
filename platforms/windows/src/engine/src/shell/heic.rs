// HEIC / HEIF decode — Windows.Graphics.Imaging.BitmapDecoder (WinRT).
//
// image-rs doesn't ship a HEIC decoder (libheif is GPL/LGPL with a system
// dep that breaks our "download and run" promise). Windows ships a HEIF
// codec via the HEIF Image Extensions store app — pre-installed on
// Windows 11, available free on the Microsoft Store for Windows 10
// 1809+. When present, WinRT's BitmapDecoder transparently decodes
// HEIC/HEIF the same way it decodes PNG/JPEG/BMP/TIFF.
//
// We use this as a fallback only — `decode_image_sync` in tagging.rs
// always tries image-rs first (faster, no COM apartment cost). If image-
// rs returns an error AND the file extension is .heic / .heif, we route
// here. When the codec isn't installed (BitmapDecoder::CreateAsync
// errors), the call surfaces as an Err the caller maps to a friendly
// "install HEIF Image Extensions" message — never panics, never blocks.
//
// Output shape matches `decode_image_sync`: tightly packed RGB8 + (w,h).

use anyhow::{Context, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::HSTRING;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Storage::{FileAccessMode, StorageFile};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Balances a successful `CoInitializeEx` with `CoUninitialize` on drop so the COM
/// apartment is scoped to a single `decode` call. HEIC decode runs on the raw
/// decoder-pool OS threads (and the Deep-Analyze tokio blocking pool) — both RECYCLED
/// across tasks — and `decode_image_sync` reaches here with NO prior COM init, so the
/// first WinRT activation (`StorageFile::GetFileFromPathAsync`) was failing
/// CO_E_NOTINITIALIZED; the caller then mis-reported it as "HEIF codec not installed",
/// silently dropping every HEIC/HEIF (the default iPhone format) from tagging/faces/
/// CLIP/thumbnails even with the codec present. Every other WinRT shell module inits a
/// COM apartment; heic.rs was the lone exception. MTA because these threads pump no
/// message loop and the blocking `.get()` waits below would risk an STA deadlock;
/// `did_init` is true only when WE init'd (S_OK/S_FALSE) — on RPC_E_CHANGED_MODE an
/// outer caller owns the apartment so we must NOT uninit. Mirrors `shell::video::ComScope`.
/// (audit — HEIC COM init)
struct ComScope {
    did_init: bool,
}

impl ComScope {
    fn enter() -> Self {
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

/// Decode a HEIC / HEIF file off disk into tightly packed RGB8 + (w,h).
/// Returns `Err` when the HEIF Image Extensions codec isn't installed.
/// Caller is expected to upgrade that error message to "install HEIF
/// Image Extensions from the Microsoft Store" for the user.
pub fn decode(path: &Path) -> Result<(Vec<u8>, u32, u32)> {
    // Scoped COM apartment for the WinRT activations below — held for the whole
    // function (the decoder + pixel-data async ops all need it live). Without it every
    // HEIC failed CO_E_NOTINITIALIZED on the apartment-less decoder-pool threads.
    let _com = ComScope::enter();
    // StorageFile::GetFileFromPathAsync wants an absolute, normalized,
    // wide-char path. encode_wide handles UTF-16 conversion.
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.is_empty() {
        anyhow::bail!("heic decode: empty path");
    }
    let hpath = HSTRING::from_wide(&wide).context("HSTRING::from_wide")?;

    let file = StorageFile::GetFileFromPathAsync(&hpath)
        .context("StorageFile::GetFileFromPathAsync")?
        .get()
        .context("await StorageFile open")?;

    let stream = file
        .OpenAsync(FileAccessMode::Read)
        .context("StorageFile::OpenAsync")?
        .get()
        .context("await OpenAsync")?;

    let decoder = BitmapDecoder::CreateAsync(&stream)
        .context("BitmapDecoder::CreateAsync (HEIF codec missing?)")?
        .get()
        .context("await BitmapDecoder build")?;

    let pw = decoder.PixelWidth().context("PixelWidth")?;
    let ph = decoder.PixelHeight().context("PixelHeight")?;
    if pw == 0 || ph == 0 {
        anyhow::bail!("heic decode: zero-dimension image");
    }
    // Match the rest of the pipeline's per-worker decode budget (50 MP =
    // ~150 MB RGB8 — the same MAX_DECODED_PIXELS ceiling tagging.rs/deep_analyze
    // enforce for image-rs decodes). HEIC has no image-rs decoder, so this is
    // HEIC's only size guard; the previous 160 MP value let a 50-160 MP HEIC
    // allocate up to ~480 MB RGB through the full ML pipeline, busting the
    // per-worker RAM budget the 50 MP cap exists to hold (and the read-ahead
    // channel's per-frame sizing assumption). 50-160 MP HEIC is rare pro
    // panorama/stitched output; bounding it keeps the 50K-file RSS goal safe.
    const MAX_PIXELS: u64 = 50_000_000;
    if (pw as u64) * (ph as u64) > MAX_PIXELS {
        anyhow::bail!(
            "heic decode: dimensions {pw}x{ph} exceed cap of {MAX_PIXELS} pixels"
        );
    }

    // GetPixelDataAsync returns the decoded frame in the decoder's
    // default format (BGRA8 for HEIF Image Extensions). The default
    // applies EXIF orientation and any embedded ICC profile to sRGB —
    // matching what Explorer + Photos render.
    let pixel_provider = decoder
        .GetPixelDataAsync()
        .context("GetPixelDataAsync")?
        .get()
        .context("await pixel data")?;

    let bgra: Vec<u8> = pixel_provider
        .DetachPixelData()
        .context("DetachPixelData")?
        .to_vec();

    let expected = (pw as usize) * (ph as usize) * 4;
    if bgra.len() < expected {
        anyhow::bail!(
            "heic decode: short pixel buffer {} < expected {}",
            bgra.len(),
            expected
        );
    }

    // BGRA → RGB. Drop alpha + swap channel order in one pass.
    let mut rgb = Vec::with_capacity((pw as usize) * (ph as usize) * 3);
    for chunk in bgra.chunks_exact(4).take((pw as usize) * (ph as usize)) {
        rgb.push(chunk[2]); // R from BGRA[2]
        rgb.push(chunk[1]); // G from BGRA[1]
        rgb.push(chunk[0]); // B from BGRA[0]
    }
    Ok((rgb, pw, ph))
}
