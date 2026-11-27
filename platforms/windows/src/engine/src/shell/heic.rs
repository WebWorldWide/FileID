// HEIC/HEIF fallback decode through Windows.Graphics.Imaging.BitmapDecoder; image-rs has no HEIC decoder.
// Needs the HEIF Image Extensions codec; when it is missing the error says so. Output is tightly packed RGB8 plus (w, h).

use anyhow::{Context, Result};
use std::os::windows::ffi::OsStrExt;
use std::path::Path;

use windows::core::HSTRING;
use windows::Graphics::Imaging::BitmapDecoder;
use windows::Storage::{FileAccessMode, StorageFile};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Scopes a COM apartment to one `decode` call. Recycled worker threads arrive without COM initialised, which would otherwise surface as a false "HEIF codec not installed".
/// MTA, because these threads pump no message loop. `did_init` is false on RPC_E_CHANGED_MODE, where an outer caller owns the apartment and it must not be uninitialised.
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
    // Held for the whole function: the async decoder ops need the apartment live.
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
    // Same per-worker decode budget as the rest of the pipeline (50 MP is about 150 MB of RGB8); HEIC's only size guard.
    const MAX_PIXELS: u64 = 50_000_000;
    if (pw as u64) * (ph as u64) > MAX_PIXELS {
        anyhow::bail!(
            "heic decode: dimensions {pw}x{ph} exceed cap of {MAX_PIXELS} pixels"
        );
    }

    // The default pixel format is BGRA8, with EXIF orientation and the ICC profile already applied.
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
        rgb.push(chunk[2]);
        rgb.push(chunk[1]);
        rgb.push(chunk[0]);
    }
    Ok((rgb, pw, ph))
}
