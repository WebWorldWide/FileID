// HEIC / HEIF decode via Windows.Graphics.Imaging.BitmapDecoder (WinRT).
// image-rs has no HEIC decoder and libheif's licence/system dep breaks "download and run". Windows ships a HEIF codec (HEIF Image Extensions: preinstalled on Windows 11, free on the Store for Windows 10 1809+).
// Fallback only: `decode_image_sync` in tagging.rs tries image-rs first and routes .heic/.heif here on error. When the codec is absent the error says so rather than blaming the file; never panics or blocks. Output matches `decode_image_sync`: tightly packed RGB8 + (w,h).

use anyhow::{Context, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::HSTRING;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Storage::{FileAccessMode, StorageFile};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Balances a successful `CoInitializeEx` with `CoUninitialize` on drop, scoping the COM apartment to one `decode` call. The decoder-pool and Deep Analyze blocking threads are recycled and arrive with no COM init, so without this the first WinRT activation failed CO_E_NOTINITIALIZED and was misreported as "HEIF codec not installed", dropping every HEIC from tagging/faces/CLIP/thumbnails.
/// MTA because these threads pump no message loop and the blocking `.join()` waits would risk an STA deadlock. `did_init` is true only when we initialised (S_OK/S_FALSE); on RPC_E_CHANGED_MODE an outer caller owns the apartment and we must not uninit. Mirrors `shell::video::ComScope`.
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

fn heif_decoder_available() -> Result<bool> {
    let codecs = BitmapDecoder::GetDecoderInformationEnumerator()
        .context("enumerate installed bitmap decoders")?;
    for codec in codecs {
        let extensions = codec
            .FileExtensions()
            .context("read bitmap decoder extensions")?;
        if extensions.into_iter().any(|extension| {
            matches!(
                extension.to_string_lossy().to_ascii_lowercase().as_str(),
                ".heic" | ".heif" | ".heics" | ".heifs"
            )
        }) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn decoder_creation_error(error: windows::core::Error) -> anyhow::Error {
    match heif_decoder_available() {
        Ok(true) => anyhow::anyhow!(
            "HEIC/HEIF image is malformed or uses unsupported encoding features ({error})"
        ),
        Ok(false) => anyhow::anyhow!(
            "HEIF decoder unavailable — install HEIF Image Extensions from the Microsoft Store ({error})"
        ),
        Err(probe_error) => anyhow::anyhow!(
            "HEIC/HEIF decode failed ({error}); decoder availability check failed ({probe_error:#})"
        ),
    }
}

pub fn decode(path: &Path) -> Result<(Vec<u8>, u32, u32)> {
    // Held for the whole function: the decoder and pixel-data async ops need the apartment live.
    let _com = ComScope::enter();
    let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    if wide.is_empty() {
        anyhow::bail!("heic decode: empty path");
    }
    let hpath = HSTRING::from_wide(&wide);

    let file = StorageFile::GetFileFromPathAsync(&hpath)
        .context("StorageFile::GetFileFromPathAsync")?
        .join()
        .context("await StorageFile open")?;

    let stream = file
        .OpenAsync(FileAccessMode::Read)
        .context("StorageFile::OpenAsync")?
        .join()
        .context("await OpenAsync")?;

    let decoder_operation = BitmapDecoder::CreateAsync(&stream)
        .context("create HEIC/HEIF bitmap decoder operation")?;
    let decoder = decoder_operation.join().map_err(decoder_creation_error)?;

    let pw = decoder.PixelWidth().context("PixelWidth")?;
    let ph = decoder.PixelHeight().context("PixelHeight")?;
    if pw == 0 || ph == 0 {
        anyhow::bail!("heic decode: zero-dimension image");
    }
    // Same per-worker decode budget as the rest of the pipeline (50 MP ≈ 150 MB RGB8, MAX_DECODED_PIXELS in tagging.rs/deep_analyze). HEIC has no image-rs decoder, so this is its only size guard; a larger cap would bust the per-worker RAM budget and the read-ahead channel's per-frame sizing.
    const MAX_PIXELS: u64 = 50_000_000;
    if (pw as u64) * (ph as u64) > MAX_PIXELS {
        anyhow::bail!(
            "heic decode: dimensions {pw}x{ph} exceed cap of {MAX_PIXELS} pixels"
        );
    }

    // GetPixelDataAsync returns the decoder's default format (BGRA8 for HEIF Image Extensions), applying EXIF orientation and any ICC profile to sRGB, matching Explorer and Photos.
    let pixel_provider = decoder
        .GetPixelDataAsync()
        .context("GetPixelDataAsync")?
        .join()
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

    let mut rgb = Vec::with_capacity((pw as usize) * (ph as usize) * 3);
    for chunk in bgra.as_chunks::<4>().0.iter().take((pw as usize) * (ph as usize)) {
        rgb.push(chunk[2]); // R from BGRA[2]
        rgb.push(chunk[1]); // G from BGRA[1]
        rgb.push(chunk[0]); // B from BGRA[0]
    }
    Ok((rgb, pw, ph))
}
