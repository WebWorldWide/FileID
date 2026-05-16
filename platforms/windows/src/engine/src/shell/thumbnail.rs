#![allow(dead_code)]
// Thumbnail — IShellItemImageFactory::GetImage.
//
// Mirror of macOS `QLThumbnailGenerator`. Uses the system's
// IThumbnailProvider chain: the same code path that Explorer uses to
// render the thumbnail in Detail/List view. This handles every kind we
// need (Office, RTF, .pages, .key, .numbers, ai, eps, psd, dwg, …)
// because the provider is whichever the user has installed.
//
// Output: tightly packed RGBA8, 512×512 by default. The Win32 path is
// HBITMAP-backed BGRA; we swap channels on extraction so callers always
// see RGBA regardless of source.

use anyhow::{Context, Result};
use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{
    DeleteObject, GetDIBits, GetObjectW, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
    DIB_RGB_COLORS, HBITMAP, HDC,
};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED,
};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF, SIIGBF_BIGGERSIZEOK,
    SIIGBF_RESIZETOFIT,
};

pub const THUMB_DIM: i32 = 512;

#[derive(Debug, Clone)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8, top-left origin.
    pub rgba: Vec<u8>,
}

/// Render the system thumbnail for `path` at the canonical 512×512 size.
/// Returns RGBA8 bytes. Initializes COM in single-threaded apartment for
/// the duration of the call (cheap; idempotent if the caller already did).
pub fn render(path: &Path) -> Result<Thumbnail> {
    render_at(path, THUMB_DIM)
}

pub fn render_at(path: &Path, dim: i32) -> Result<Thumbnail> {
    if !path.exists() {
        anyhow::bail!("thumbnail source missing: {}", path.display());
    }

    unsafe {
        // Initialize COM apartment for this call. CoUninitialize on every
        // exit path. RPC_E_CHANGED_MODE means the apartment is already set
        // (the caller initialized it MTA), which we accept silently.
        let init = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let _com_guard = ComGuard {
            initialized: init.is_ok(),
        };

        let path_str = path
            .to_str()
            .context("thumbnail path must be UTF-8")?;
        let mut wide: Vec<u16> = path_str.encode_utf16().collect();
        wide.push(0);

        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(PCWSTR::from_raw(wide.as_ptr()), None)
                .context("SHCreateItemFromParsingName")?;

        let size = SIZE { cx: dim, cy: dim };
        let flags = SIIGBF(SIIGBF_RESIZETOFIT.0 | SIIGBF_BIGGERSIZEOK.0);
        let hbm: HBITMAP = factory
            .GetImage(size, flags)
            .context("IShellItemImageFactory::GetImage")?;

        let result = hbitmap_to_rgba(hbm);
        let _ = DeleteObject(windows::Win32::Graphics::Gdi::HGDIOBJ(hbm.0));
        result
    }
}

unsafe fn hbitmap_to_rgba(hbm: HBITMAP) -> Result<Thumbnail> {
    // Pull dimensions via GetObject(BITMAP).
    let mut bitmap = BITMAP::default();
    let written = unsafe {
        GetObjectW(
            windows::Win32::Graphics::Gdi::HGDIOBJ(hbm.0),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bitmap as *mut BITMAP as *mut _),
        )
    };
    if written == 0 {
        anyhow::bail!("GetObjectW returned 0 — invalid HBITMAP");
    }

    let width = bitmap.bmWidth.max(0) as u32;
    let height = bitmap.bmHeight.max(0) as u32;
    if width == 0 || height == 0 {
        anyhow::bail!("HBITMAP has zero extent");
    }

    let row_bytes = (width as usize) * 4;
    let mut bgra = vec![0u8; row_bytes * (height as usize)];

    let mut bi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            // Negative height = top-down DIB; matches our RGBA expectation.
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let scanned = unsafe {
        GetDIBits(
            HDC::default(),
            hbm,
            0,
            height,
            Some(bgra.as_mut_ptr() as *mut _),
            &mut bi as *mut _,
            DIB_RGB_COLORS,
        )
    };
    if scanned == 0 {
        anyhow::bail!("GetDIBits returned 0");
    }

    // BGRA → RGBA swap. The HBITMAP from the shell may have premultiplied
    // alpha; Explorer renders correctly because it uses BGRA throughout.
    // We swap channels so callers see canonical RGBA.
    let mut rgba = bgra;
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }

    Ok(Thumbnail {
        width,
        height,
        rgba,
    })
}

struct ComGuard {
    initialized: bool,
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            unsafe { CoUninitialize() };
        }
    }
}
