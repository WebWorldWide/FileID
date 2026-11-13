// On-disk thumbnail cache, so a grid that was decoded once opens from small
// files instead of re-reading every photo. Lives in the helper's own
// `thumbs.cache` folder (cleared by Start over), keyed by the file's path,
// size, modified time and the requested size; capped, oldest files first.
// Everything here runs on the thumbnail worker threads, never the GTK thread.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::SystemTime;

use gtk::gdk_pixbuf::{Colorspace, Pixbuf};

use crate::engine_client::DecodedImage;

/// The cache never grows past this; trimming takes it down to nine tenths.
const CAP_BYTES: u64 = 512 * 1024 * 1024;
/// A trim runs once at first use and again after this much has been written.
const TRIM_EVERY_BYTES: u64 = 32 * 1024 * 1024;
/// Grid and card sizes are cached; a full-size preview is not.
const MAX_CACHED_DIM: i32 = 1024;

static WRITTEN: AtomicU64 = AtomicU64::new(0);
static TRIMMED_ONCE: AtomicBool = AtomicBool::new(false);
static TRIMMING: AtomicBool = AtomicBool::new(false);

/// What a cached thumbnail was made from. A changed file has a new key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub path: String,
    pub size: u64,
    pub modified_nanos: u128,
    pub dim: i32,
    pub video: bool,
}

impl Source {
    pub fn of(path: &Path, dim: i32, video: bool) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let modified_nanos = meta
            .modified()
            .ok()?
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or(0);
        Some(Self { path: path.to_string_lossy().into_owned(), size: meta.len(), modified_nanos, dim, video })
    }

    /// Two independent 64-bit FNV-1a passes: a stable name across runs and
    /// versions (the standard hasher promises neither).
    fn file_stem(&self) -> String {
        let text = format!(
            "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
            self.path, self.size, self.modified_nanos, self.dim, self.video
        );
        format!("{:016x}{:016x}", fnv1a(text.as_bytes(), 0xcbf2_9ce4_8422_2325), fnv1a(text.as_bytes(), 0x8422_2325_cbf2_9ce4))
    }
}

fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
    bytes.iter().fold(seed, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

#[cfg(not(test))]
fn default_dir() -> Option<PathBuf> {
    fileid_engine::paths::thumbs_dir().ok()
}

/// Unit tests never write the owner's real cache; they pass their own folder.
#[cfg(test)]
fn default_dir() -> Option<PathBuf> {
    None
}

/// The cached thumbnail for `path` at `dim`, or `render`'s result, which is
/// then stored. Anything that goes wrong with the cache just means a render.
pub fn get_or_render(
    path: &Path,
    dim: i32,
    video: bool,
    render: impl FnOnce() -> Option<DecodedImage>,
) -> Option<DecodedImage> {
    let cached = (dim <= MAX_CACHED_DIM)
        .then(default_dir)
        .flatten()
        .and_then(|dir| Some((dir, Source::of(path, dim, video)?)));
    let Some((dir, source)) = cached else { return render() };
    if let Some(hit) = load(&dir, &source) {
        return Some(hit);
    }
    let image = render()?;
    if let Some(bytes) = store(&dir, &source, &image) {
        note_written(&dir, bytes);
    }
    Some(image)
}

fn note_written(dir: &Path, bytes: u64) {
    let total = WRITTEN.fetch_add(bytes, Ordering::Relaxed) + bytes;
    let first = !TRIMMED_ONCE.swap(true, Ordering::Relaxed);
    if !first && total < TRIM_EVERY_BYTES {
        return;
    }
    if TRIMMING.swap(true, Ordering::Acquire) {
        return;
    }
    WRITTEN.store(0, Ordering::Relaxed);
    trim(dir, CAP_BYTES);
    TRIMMING.store(false, Ordering::Release);
}

fn entry_path(dir: &Path, source: &Source, extension: &str) -> PathBuf {
    dir.join(format!("{}.{extension}", source.file_stem()))
}

pub(crate) fn load(dir: &Path, source: &Source) -> Option<DecodedImage> {
    ["jpg", "png"].iter().find_map(|extension| {
        let file = entry_path(dir, source, extension);
        if !file.exists() {
            return None;
        }
        let decoded = decode(&file);
        if decoded.is_none() {
            let _ = std::fs::remove_file(&file);
        }
        decoded
    })
}

fn decode(file: &Path) -> Option<DecodedImage> {
    let pixbuf = Pixbuf::from_file(file).ok()?;
    if pixbuf.colorspace() != Colorspace::Rgb || pixbuf.bits_per_sample() != 8 {
        return None;
    }
    let (width, height) = (usize::try_from(pixbuf.width()).ok()?, usize::try_from(pixbuf.height()).ok()?);
    let channels = usize::try_from(pixbuf.n_channels()).ok()?;
    let stride = usize::try_from(pixbuf.rowstride()).ok()?;
    if width == 0 || height == 0 || !(channels == 3 || channels == 4) {
        return None;
    }
    let pixels = pixbuf.read_pixel_bytes();
    let mut rgba = Vec::with_capacity(width * height * 4);
    for row in 0..height {
        let line = pixels.get(row * stride..row * stride + width * channels)?;
        if channels == 4 {
            rgba.extend_from_slice(line);
        } else {
            for pixel in line.as_chunks::<3>().0 {
                rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
            }
        }
    }
    Some(DecodedImage { width: width as u32, height: height as u32, rgba })
}

/// Opaque images are stored as JPEG, anything with transparency as PNG.
/// Returns the bytes written.
pub(crate) fn store(dir: &Path, source: &Source, image: &DecodedImage) -> Option<u64> {
    let (width, height) = (i32::try_from(image.width).ok()?, i32::try_from(image.height).ok()?);
    let pixels = usize::try_from(image.width).ok()?.checked_mul(usize::try_from(image.height).ok()?)?;
    if pixels == 0 || image.rgba.len() != pixels.checked_mul(4)? {
        return None;
    }
    let opaque = image.rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255);
    let (extension, encoded) = if opaque {
        let rgb: Vec<u8> = image.rgba.as_chunks::<4>().0.iter().flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect();
        let pixbuf =
            Pixbuf::from_bytes(&glib::Bytes::from_owned(rgb), Colorspace::Rgb, false, 8, width, height, width * 3);
        ("jpg", pixbuf.save_to_bufferv("jpeg", &[("quality", "86")]).ok()?)
    } else {
        let pixbuf = Pixbuf::from_bytes(
            &glib::Bytes::from(image.rgba.as_slice()),
            Colorspace::Rgb,
            true,
            8,
            width,
            height,
            width * 4,
        );
        ("png", pixbuf.save_to_bufferv("png", &[("compression", "3")]).ok()?)
    };
    std::fs::create_dir_all(dir).ok()?;
    let file = entry_path(dir, source, extension);
    let partial = dir.join(format!("{}.{}.part", source.file_stem(), std::process::id()));
    if std::fs::write(&partial, &encoded).is_err() || std::fs::rename(&partial, &file).is_err() {
        let _ = std::fs::remove_file(&partial);
        return None;
    }
    Some(encoded.len() as u64)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub name: String,
    pub bytes: u64,
    pub modified: SystemTime,
}

/// The files to delete, oldest first, to bring a cache over `cap` down to
/// nine tenths of it. Nothing when it already fits.
pub(crate) fn trim_plan(mut entries: Vec<Entry>, cap: u64) -> Vec<String> {
    let mut total: u64 = entries.iter().map(|entry| entry.bytes).sum();
    if total <= cap {
        return Vec::new();
    }
    let target = cap / 10 * 9;
    entries.sort_by(|a, b| a.modified.cmp(&b.modified).then_with(|| a.name.cmp(&b.name)));
    let mut doomed = Vec::new();
    for entry in entries {
        if total <= target {
            break;
        }
        total -= entry.bytes;
        doomed.push(entry.name);
    }
    doomed
}

pub(crate) fn trim(dir: &Path, cap: u64) {
    let Ok(listing) = std::fs::read_dir(dir) else { return };
    let mut entries = Vec::new();
    for item in listing.flatten() {
        let name = item.file_name().to_string_lossy().into_owned();
        let Ok(meta) = item.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        if name.ends_with(".part") {
            // A write that never finished (the app was closed mid-store).
            let stale = meta.modified().ok().and_then(|at| at.elapsed().ok()).is_some_and(|age| age.as_secs() > 600);
            if stale {
                let _ = std::fs::remove_file(item.path());
            }
            continue;
        }
        if !(name.ends_with(".jpg") || name.ends_with(".png")) {
            continue;
        }
        entries.push(Entry { name, bytes: meta.len(), modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH) });
    }
    for name in trim_plan(entries, cap) {
        let _ = std::fs::remove_file(dir.join(name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fileid-thumb-cache-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn source(path: &str, size: u64, modified_nanos: u128, dim: i32) -> Source {
        Source { path: path.into(), size, modified_nanos, dim, video: false }
    }

    fn entry(name: &str, bytes: u64, age_secs: u64) -> Entry {
        Entry {
            name: name.into(),
            bytes,
            modified: SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000 - age_secs),
        }
    }

    #[test]
    fn the_key_changes_with_the_path_size_time_and_requested_size() {
        let base = source("/photos/a.jpg", 10, 5, 256);
        let stems: Vec<String> = [
            base.clone(),
            source("/photos/b.jpg", 10, 5, 256),
            source("/photos/a.jpg", 11, 5, 256),
            source("/photos/a.jpg", 10, 6, 256),
            source("/photos/a.jpg", 10, 5, 512),
            Source { video: true, ..base.clone() },
        ]
        .iter()
        .map(Source::file_stem)
        .collect();
        for (index, stem) in stems.iter().enumerate() {
            assert_eq!(stem.len(), 32);
            assert!(stems[index + 1..].iter().all(|other| other != stem), "key {index} collides");
        }
        assert_eq!(base.file_stem(), source("/photos/a.jpg", 10, 5, 256).file_stem());
    }

    #[test]
    fn a_cache_within_its_cap_is_left_alone() {
        assert!(trim_plan(vec![entry("a", 40, 3), entry("b", 60, 1)], 100).is_empty());
        assert!(trim_plan(Vec::new(), 100).is_empty());
    }

    #[test]
    fn an_oversized_cache_loses_its_oldest_files_down_to_nine_tenths() {
        let entries = vec![entry("newest", 40, 1), entry("oldest", 40, 9), entry("middle", 40, 5), entry("old", 40, 7)];
        assert_eq!(trim_plan(entries.clone(), 100), ["oldest", "old"]);
        assert_eq!(trim_plan(entries, 150), ["oldest"]);
        let tie = vec![entry("b", 60, 4), entry("a", 60, 4)];
        assert_eq!(trim_plan(tie, 100), ["a"]);
    }

    #[test]
    fn trimming_deletes_only_cache_files_oldest_first() {
        let dir = scratch("trim");
        for (name, age) in [("old.jpg", 300u64), ("mid.png", 200), ("new.jpg", 100)] {
            let file = dir.join(name);
            std::fs::write(&file, vec![0u8; 1000]).unwrap();
            let at = SystemTime::now() - Duration::from_secs(age);
            std::fs::File::options().write(true).open(&file).unwrap().set_modified(at).unwrap();
        }
        std::fs::write(dir.join("notes.txt"), vec![0u8; 5000]).unwrap();
        trim(&dir, 2500);
        assert!(!dir.join("old.jpg").exists());
        assert!(dir.join("mid.png").exists());
        assert!(dir.join("new.jpg").exists());
        assert!(dir.join("notes.txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_stored_thumbnail_comes_back_with_its_size_and_alpha() {
        let dir = scratch("roundtrip");
        let opaque = DecodedImage { width: 8, height: 4, rgba: [200u8, 100, 50, 255].repeat(32) };
        let key = source("/photos/a.jpg", 10, 5, 256);
        assert!(load(&dir, &key).is_none());
        assert!(store(&dir, &key, &opaque).is_some_and(|bytes| bytes > 0));
        let back = load(&dir, &key).expect("cached thumbnail");
        assert_eq!((back.width, back.height, back.rgba.len()), (8, 4, 8 * 4 * 4));
        assert!(back.rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == 255 && pixel[0].abs_diff(200) < 12));

        let mut clear = opaque.clone();
        clear.rgba[3] = 0;
        let clear_key = source("/photos/b.png", 10, 5, 256);
        store(&dir, &clear_key, &clear).unwrap();
        assert_eq!(load(&dir, &clear_key).unwrap().rgba, clear.rgba);
        assert!(load(&dir, &source("/photos/a.jpg", 11, 5, 256)).is_none());

        let bad = DecodedImage { width: 8, height: 4, rgba: vec![0; 5] };
        assert!(store(&dir, &source("/photos/c.jpg", 1, 1, 256), &bad).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_damaged_cache_file_is_dropped_and_rendered_again() {
        let dir = scratch("damaged");
        let key = source("/photos/a.jpg", 10, 5, 256);
        std::fs::write(entry_path(&dir, &key, "jpg"), b"not a picture").unwrap();
        assert!(load(&dir, &key).is_none());
        assert!(!entry_path(&dir, &key, "jpg").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
