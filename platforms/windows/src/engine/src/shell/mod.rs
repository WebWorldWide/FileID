// Windows shell + system integrations. Each Win32 submodule is a thin
// RAII wrapper over a Win32 / WinRT API:
//
//   reveal     → SHOpenFolderAndSelectItems
//   trash      → IFileOperation::DeleteItem (8-parallel from Cleanup tab)
//   thumbnail  → IThumbnailProvider
//   ocr        → Windows.Media.Ocr (WinRT)
//   tags       → IPropertyStore System.Keywords
//   video      → Media Foundation IMFSourceReader
//
// Sleep-prevention (SetThreadExecutionState) lives in `crate::platform`
// because it's cross-cutting, not shell-specific.
//
// On non-Windows targets thumbnail rendering supports image-rs formats.
// The remaining shell integrations return errors until native Linux
// implementations are available.

#[cfg(windows)] pub mod reveal;
#[cfg(windows)] pub mod tags;
#[cfg(windows)] pub mod thumbnail;
#[cfg(windows)] pub mod trash;
#[cfg(windows)] pub mod ocr;
#[cfg(windows)] pub mod video;
#[cfg(windows)] pub mod heic;

#[cfg(not(windows))]
pub mod reveal {
    use anyhow::Result;
    use std::path::Path;
    #[allow(dead_code)]
    pub fn reveal(_path: &Path) -> Result<()> {
        anyhow::bail!("shell::reveal::reveal not implemented on this platform")
    }
}

#[cfg(not(windows))]
pub mod tags {
    use anyhow::Result;
    use std::path::Path;
    #[allow(dead_code)]
    pub fn write_tags(_path: &Path, _tags: &[String]) -> Result<()> {
        anyhow::bail!("shell::tags::write_tags not implemented on this platform")
    }
    pub fn write_tags_full(_path: &Path, _tags: &[String]) -> Result<bool> {
        anyhow::bail!("shell::tags::write_tags_full not implemented on this platform")
    }
    #[allow(dead_code)]
    pub fn read_tags(_path: &Path) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
    pub fn move_sidecar(_old: &Path, _new: &Path) {}
}

#[cfg(not(windows))]
pub mod thumbnail {
    use anyhow::{Context, Result};
    use std::path::Path;

    pub const THUMB_DIM: i32 = 512;

    #[derive(Debug, Clone)]
    pub struct Thumbnail {
        pub width: u32,
        pub height: u32,
        pub rgba: Vec<u8>,
    }

    pub fn render(path: &Path) -> Result<Thumbnail> {
        render_at(path, THUMB_DIM)
    }

    pub fn render_at(path: &Path, dim: i32) -> Result<Thumbnail> {
        let dim = u32::try_from(dim).ok().filter(|dim| *dim > 0)
            .context("thumbnail dimension must be positive")?;
        let image = image::ImageReader::open(path)
            .context("opening thumbnail source")?
            .with_guessed_format()
            .context("detecting image format")?
            .decode()
            .context("decoding thumbnail source")?
            .thumbnail(dim, dim)
            .into_rgba8();
        Ok(Thumbnail {
            width: image.width(),
            height: image.height(),
            rgba: image.into_raw(),
        })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn linux_thumbnail_scales_generated_image_and_rejects_invalid_size() {
            let fixture = std::env::temp_dir().join(format!(
                "fileid-thumbnail-{}-{}",
                std::process::id(),
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
            ));
            std::fs::create_dir_all(&fixture).unwrap();
            let path = fixture.join("generated.png");
            image::RgbaImage::from_pixel(40, 20, image::Rgba([200, 40, 60, 255]))
                .save(&path).unwrap();
            let thumb = render_at(&path, 8).unwrap();
            assert_eq!((thumb.width, thumb.height), (8, 4));
            assert_eq!(thumb.rgba.len(), 8 * 4 * 4);
            assert_eq!(&thumb.rgba[0..4], &[200, 40, 60, 255]);
            assert!(render_at(&path, 0).is_err());
            std::fs::remove_dir_all(fixture).unwrap();
        }
    }
}

#[cfg(not(windows))]
#[path = "trash_linux.rs"]
pub mod trash;

#[cfg(not(windows))]
pub mod ocr {
    use anyhow::Result;
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct OcrLine { pub text: String }
    #[allow(dead_code)]
    pub struct OcrResult {
        pub text: String,
        pub lines: Vec<OcrLine>,
        pub locale: Option<String>,
    }
    #[allow(dead_code)]
    pub fn recognize(_rgba: &[u8], _width: u32, _height: u32) -> Result<OcrResult> {
        anyhow::bail!("shell::ocr::recognize not implemented on this platform")
    }
}

#[cfg(not(windows))]
pub mod video {
    use anyhow::Result;
    use std::path::Path;
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct VideoFrame {
        pub width: u32,
        pub height: u32,
        /// Tightly packed RGB8.
        pub rgb: Vec<u8>,
        pub time_seconds: f64,
    }
    pub fn keyframe_25pct(_path: &Path) -> Result<VideoFrame> {
        anyhow::bail!("shell::video::keyframe_25pct not implemented on this platform")
    }
}

#[cfg(not(windows))]
pub mod heic {
    use anyhow::Result;
    use std::path::Path;
    #[allow(dead_code)]
    pub fn decode(_path: &Path) -> Result<(Vec<u8>, u32, u32)> {
        anyhow::bail!("shell::heic::decode not implemented on this platform")
    }
}
