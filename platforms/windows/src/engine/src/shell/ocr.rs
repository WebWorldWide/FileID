// OCR via Windows.Media.Ocr (WinRT): built in, no model download, fully on-device.

use anyhow::{Context, Result};

use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Gives the calling thread a COM apartment for the WinRT activations below; blocking-pool threads have none, and the resulting failure would silently produce no OCR text.
/// MTA, not STA: these threads pump no message loop, so the blocking `RecognizeAsync(..).join()` would risk an STA deadlock. `is_ok()` is false on RPC_E_CHANGED_MODE, where another apartment owns the thread and it must be left alone.
fn with_com<R>(body: impl FnOnce() -> R) -> R {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
        let must_uninit = hr.is_ok();
        let result = body();
        if must_uninit {
            CoUninitialize();
        }
        result
    }
}

/// Runs OCR on a tightly-packed RGB buffer; empty text means nothing matched.
pub fn recognize(rgb: &[u8], width: u32, height: u32) -> Result<String> {
    // Caps each side so `width * height * 4` cannot overflow u32 and the i32 dimensions of `CreateCopyFromBuffer` stay in range.
    const MAX_DIM: u32 = 16384;
    if width == 0 || height == 0 || width > MAX_DIM || height > MAX_DIM {
        anyhow::bail!("OCR.recognize: dimensions out of range ({width}x{height})");
    }
    let pixels = (width as usize) * (height as usize);
    if rgb.len() < pixels * 3 {
        anyhow::bail!("OCR.recognize: invalid RGB buffer");
    }

    let mut bgra = Vec::with_capacity(pixels * 4);
    for chunk in rgb.as_chunks::<3>().0 {
        bgra.push(chunk[2]);
        bgra.push(chunk[1]);
        bgra.push(chunk[0]);
        bgra.push(255);
    }

    with_com(|| -> Result<String> {
    let writer = DataWriter::new()?;
    writer.WriteBytes(&bgra)?;
    let buffer = writer.DetachBuffer()?;

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
        .join()
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
