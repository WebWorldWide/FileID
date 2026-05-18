// OCR — Windows.Media.Ocr (WinRT).
//
// Built into Windows since 1809 (no install, no model download).
// Multi-language (locales picked per call); falls back to English if the
// requested locale isn't installed. Privacy: runs entirely on-device.

use anyhow::{Context, Result};
use std::io::Cursor;

use windows::Graphics::Imaging::{BitmapDecoder, SoftwareBitmap};
use windows::Media::Ocr::OcrEngine;
use windows::Storage::Streams::{DataWriter, InMemoryRandomAccessStream};

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

/// Run OCR on a tightly-packed RGB buffer. Encodes to PNG in memory,
/// hands to WinRT BitmapDecoder → SoftwareBitmap → OcrEngine.
/// Returns a structured result; empty `text` means nothing matched.
pub fn recognize(rgb: &[u8], width: u32, height: u32) -> Result<OcrResult> {
    if width == 0 || height == 0 || rgb.len() < (width as usize) * (height as usize) * 3 {
        anyhow::bail!("OCR.recognize: invalid RGB buffer");
    }

    // Encode RGB → PNG so BitmapDecoder can hand back a SoftwareBitmap
    // in BGRA8 (the format OcrEngine accepts).
    let img: image::ImageBuffer<image::Rgb<u8>, _> =
        image::ImageBuffer::from_raw(width, height, rgb.to_vec())
            .context("invalid RGB ImageBuffer")?;
    let mut png_bytes = Vec::with_capacity(((width * height) as usize) / 4 + 1024);
    img.write_to(&mut Cursor::new(&mut png_bytes), image::ImageFormat::Png)
        .context("encode RGB → PNG")?;

    let engine = OcrEngine::TryCreateFromUserProfileLanguages()
        .context("OcrEngine::TryCreateFromUserProfileLanguages")?;

    let stream = InMemoryRandomAccessStream::new().context("create InMemoryRandomAccessStream")?;
    let writer = DataWriter::CreateDataWriter(&stream.GetOutputStreamAt(0)?)
        .context("create DataWriter")?;
    writer.WriteBytes(&png_bytes).context("write PNG bytes")?;
    writer
        .StoreAsync()
        .context("DataWriter::StoreAsync")?
        .get()
        .context("flush bytes")?;
    writer
        .FlushAsync()
        .context("DataWriter::FlushAsync")?
        .get()
        .context("flush op")?;
    let _ = writer.DetachStream();

    let _ = stream.Seek(0);
    let decoder = BitmapDecoder::CreateAsync(&stream)
        .context("BitmapDecoder::CreateAsync")?
        .get()
        .context("decoder build")?;
    let soft_bitmap: SoftwareBitmap = decoder
        .GetSoftwareBitmapAsync()
        .context("GetSoftwareBitmapAsync")?
        .get()
        .context("get bitmap")?;

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
