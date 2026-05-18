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
// On non-Windows targets each module is replaced by a stub with matching
// public surface; stubs return Err("…not implemented on this platform")
// so call sites compile. TODO(linux): real implementations
// (gdk-pixbuf thumbnails, gio trash, tesseract OCR, ffmpeg frames,
// xdg-open reveal, xattr tags).

#[cfg(windows)] pub mod reveal;
#[cfg(windows)] pub mod tags;
#[cfg(windows)] pub mod thumbnail;
#[cfg(windows)] pub mod trash;
#[cfg(windows)] pub mod ocr;
#[cfg(windows)] pub mod video;

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
    pub fn write_tags(_path: &Path, _tags: &[String]) -> Result<()> {
        anyhow::bail!("shell::tags::write_tags not implemented on this platform")
    }
    #[allow(dead_code)]
    pub fn read_tags(_path: &Path) -> Result<Vec<String>> {
        Ok(Vec::new())
    }
}

#[cfg(not(windows))]
pub mod thumbnail {
    use anyhow::Result;
    use std::path::Path;
    #[allow(dead_code)]
    pub const THUMB_DIM: i32 = 512;
    #[derive(Debug, Clone)]
    #[allow(dead_code)]
    pub struct Thumbnail {
        pub width: u32,
        pub height: u32,
        pub rgba: Vec<u8>,
    }
    pub fn render(_path: &Path) -> Result<Thumbnail> {
        anyhow::bail!("shell::thumbnail::render not implemented on this platform")
    }
    #[allow(dead_code)]
    pub fn render_at(_path: &Path, _dim: i32) -> Result<Thumbnail> {
        anyhow::bail!("shell::thumbnail::render_at not implemented on this platform")
    }
}

#[cfg(not(windows))]
pub mod trash {
    use std::path::{Path, PathBuf};
    /// Linux stub: returns all-false so the caller logs failure cleanly
    /// rather than silently claiming a successful trash. Real Linux
    /// implementation will route through `gio trash` or the GIO C API.
    pub fn trash(paths: &[PathBuf]) -> Vec<bool> {
        vec![false; paths.len()]
    }
    #[allow(dead_code)]
    pub fn trash_path(_path: &Path) -> anyhow::Result<()> {
        anyhow::bail!("shell::trash::trash_path not implemented on this platform")
    }
}

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
