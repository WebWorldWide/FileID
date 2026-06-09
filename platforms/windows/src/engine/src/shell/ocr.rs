// OCR — Windows.Media.Ocr (WinRT).
//
// Built into Windows since 1809 (no install, no model download).
// Multi-language (locales picked per call); falls back to English if the
// requested locale isn't installed. Privacy: runs entirely on-device.

use std::cell::Cell;

use anyhow::{Context, Result};

use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

thread_local! {
    static COM_INIT: Cell<bool> = const { Cell::new(false) };
}

/// WinRT class activation (DataWriter / SoftwareBitmap / OcrEngine) requires
/// the calling thread's COM apartment to be initialized. `recognize()` runs
/// via `spawn_blocking`, and tokio's blocking-pool threads are never
/// COM-initialized — so the first WinRT call returned CO_E_NOTINITIALIZED,
/// which the caller swallowed to `Ok(None)`: OCR silently never ran on
/// Windows. Mirror the per-thread guard the other shell::* modules use. MTA
/// (no message pump here); RPC_E_CHANGED_MODE on a thread already init STA is
/// tolerated. No CoUninitialize — threads live for the scan, exit cleans up.
fn ensure_com_initialized() {
    COM_INIT.with(|flag| {
        if !flag.get() {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            }
            flag.set(true);
        }
    });
}

// Public API surface — current callers only consume `text`, but `lines`
// and `locale` are populated so the UI can later render per-line OCR
// overlays without a schema change. Keep until that consumer lands.
#[allow(dead_code)]
pub struct OcrResult {
    pub text: String,
    pub lines: Vec<OcrLine>,
    pub locale: Option<String>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct OcrLine {
    pub text: String,
    pub bbox: [f32; 4],
    pub confidence: f32,
}

/// Run OCR on a tightly-packed RGB buffer. Maps to BGRA8 in memory,
/// populates a WinRT IBuffer, and calls SoftwareBitmap::CreateCopyFromBuffer.
/// Returns a structured result; empty `text` means nothing matched.
pub fn recognize(rgb: &[u8], width: u32, height: u32) -> Result<OcrResult> {
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

    // Must run before the first WinRT activation below.
    ensure_com_initialized();

    let mut bgra = Vec::with_capacity(pixels * 4);
    for chunk in rgb.chunks_exact(3) {
        bgra.push(chunk[2]); // B
        bgra.push(chunk[1]); // G
        bgra.push(chunk[0]); // R
        bgra.push(255);      // A
    }

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

    let mut lines_out: Vec<OcrLine> = Vec::new();
    if let Ok(lines) = result.Lines() {
        if let Ok(size) = lines.Size() {
            for i in 0..size {
                let line = match lines.GetAt(i) {
                    Ok(l) => l,
                    Err(_) => continue,
                };
                let text = line.Text().map(|s| s.to_string_lossy()).unwrap_or_default();
                let bbox = if let Ok(words) = line.Words() {
                    if let (Ok(first), Ok(_)) = (words.GetAt(0), words.Size()) {
                        if let Ok(rect) = first.BoundingRect() {
                            [rect.X, rect.Y, rect.Width, rect.Height]
                        } else {
                            [0.0; 4]
                        }
                    } else {
                        [0.0; 4]
                    }
                } else {
                    [0.0; 4]
                };
                lines_out.push(OcrLine {
                    text,
                    bbox,
                    confidence: 1.0,
                });
            }
        }
    }

    let text = result
        .Text()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default();
    // OcrEngine doesn't expose RecognizerLanguage in this windows-rs
    // surface; we leave locale = None and let downstream tag with
    // "auto" if needed.
    let locale: Option<String> = None;

    Ok(OcrResult {
        text,
        lines: lines_out,
        locale,
    })
}

/// Best-effort list of locales the engine actually supports on this box.
/// We probe by trying to create the engine; if it succeeds, return a
/// single "auto" entry.
#[allow(dead_code)]
pub fn user_locales() -> Result<Vec<String>> {
    let _ = OcrEngine::TryCreateFromUserProfileLanguages()
        .context("TryCreateFromUserProfileLanguages")?;
    Ok(vec!["auto".into()])
}

// Suppress unused-Interface warning when feature gates close all
// downstream consumers.
const _: () = {
    let _ = std::mem::size_of::<OcrEngine>;
};

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
