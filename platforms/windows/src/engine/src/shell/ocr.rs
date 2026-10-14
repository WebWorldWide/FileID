// OCR — Windows.Media.Ocr (WinRT).
//
// Built into Windows since 1809 (no install, no model download).
// Multi-language (locales picked per call); falls back to English if the
// requested locale isn't installed. Privacy: runs entirely on-device.

use anyhow::{Context, Result};

use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED};

/// Ensure the calling thread has a COM apartment for the WinRT activations
/// below. Every other shell module that touches COM/WinRT does this; OCR runs
/// on tokio blocking-pool threads that start with NO apartment, so the first
/// WinRT activation (DataWriter::new) was failing CO_E_NOTINITIALIZED and the
/// error was silently swallowed by the caller — OCR produced nothing. Mirrors
/// `shell::tags::with_com`. `is_ok()` is true for S_OK/S_FALSE (uninit to
/// balance), false for RPC_E_CHANGED_MODE (thread already in another apartment;
/// don't touch it — WinRT activation works on MTA too).
fn with_com<R>(body: impl FnOnce() -> R) -> R {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let must_uninit = hr.is_ok();
        let result = body();
        if must_uninit {
            CoUninitialize();
        }
        result
    }
}

/// Run OCR on a tightly-packed RGB buffer. Maps to BGRA8 in memory,
/// populates a WinRT IBuffer, and calls SoftwareBitmap::CreateCopyFromBuffer.
/// Returns the recognized text; empty means nothing matched.
pub fn recognize(rgb: &[u8], width: u32, height: u32) -> Result<String> {
    // Cap each side at 16384 so `width * height * 4` cannot overflow u32 and
    // `SoftwareBitmap::CreateCopyFromBuffer` (which takes i32 dims) stays in
    // range. Windows.Media.Ocr tops out well below this in practice.
    const MAX_DIM: u32 = 16384;
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        anyhow::bail!("OCR.recognize: dimensions out of range ({width}x{height})");
    }
    let pixels = (width as usize) * (height as usize);
    if rgb.len() < pixels * 3 {
        anyhow::bail!("OCR.recognize: invalid RGB buffer");
    }

    let mut bgra = Vec::with_capacity(pixels * 4);
    for chunk in rgb.chunks_exact(3) {
        bgra.push(chunk[2]); // B
        bgra.push(chunk[1]); // G
        bgra.push(chunk[0]); // R
        bgra.push(255);      // A
    }

    with_com(|| -> Result<String> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(&bgra)?;
    let buffer = writer.DetachBuffer()?;

    // Create SoftwareBitmap directly from the BGRA8 buffer.
    let soft_bitmap = SoftwareBitmap::CreateCopyFromBuffer(
        &buffer,
        BitmapPixelFormat::Bgra8,
        width as i32,
        height as i32,
    )?;

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .context("OcrEngine::TryCreateFromUserProfileLanguages")?;

    let result = engine
        .RecognizeAsync(&soft_bitmap)
        .context("RecognizeAsync")?
        .get()
        .context("recognize result")?;

    Ok(result.Text().map(|s| s.to_string_lossy()).unwrap_or_default())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognize_rejects_zero_dim() {
        assert!(recognize(&[], 0, 10).is_err());
        assert!(recognize(&[], 10, 0).is_err());
    }

    #[test]
    fn recognize_rejects_oversize_dim() {
        // 200_000 > MAX_DIM (16384). Bails before any Windows API call.
        match recognize(&[], 200_000, 200_000) {
            Ok(_) => panic!("oversize must bail"),
            Err(e) => assert!(
                format!("{e}").contains("out of range"),
                "unexpected error: {e}"
            ),
        }
    }

    #[test]
    fn recognize_rejects_short_buffer() {
        // 100x100 RGB needs 30_000 bytes; pass 100 to force the length check.
        let buf = vec![0u8; 100];
        assert!(recognize(&buf, 100, 100).is_err());
    }
}
