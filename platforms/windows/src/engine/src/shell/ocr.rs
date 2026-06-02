// OCR — Windows.Media.Ocr (WinRT).
//
// Built into Windows since 1809 (no install, no model download).
// Multi-language (locales picked per call); falls back to English if the
// requested locale isn't installed. Privacy: runs entirely on-device.

use anyhow::{Context, Result};

use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::DataWriter;

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
                // Union of every word's rect, not just the first word's — the
                // line bbox must span the whole line for an overlay/crop to be
                // correct (#30).
                let bbox = if let Ok(words) = line.Words() {
                    let count = words.Size().unwrap_or(0);
                    let mut min_x = f32::MAX;
                    let mut min_y = f32::MAX;
                    let mut max_x = f32::MIN;
                    let mut max_y = f32::MIN;
                    let mut any = false;
                    for w in 0..count {
                        if let Ok(word) = words.GetAt(w) {
                            if let Ok(r) = word.BoundingRect() {
                                min_x = min_x.min(r.X);
                                min_y = min_y.min(r.Y);
                                max_x = max_x.max(r.X + r.Width);
                                max_y = max_y.max(r.Y + r.Height);
                                any = true;
                            }
                        }
                    }
                    if any {
                        [min_x, min_y, (max_x - min_x).max(0.0), (max_y - min_y).max(0.0)]
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
