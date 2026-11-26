use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The owner's photo drive stays protected by name even where the volume flag cannot be read (owner policy).
const READ_ONLY_ROOT: &str = "/Volumes/Adlon";
const VOLUME_TTL: Duration = Duration::from_secs(3);
const CACHE_LIMIT: usize = 4096;

/// The refusal carried inside the `io::Error` so message builders can name the location without parsing text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocationReadOnly {
    pub name: String,
}

impl std::fmt::Display for LocationReadOnly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", location_sentence(&self.name))
    }
}

impl std::error::Error for LocationReadOnly {}

pub fn location_sentence(name: &str) -> String {
    format!("\u{201c}{name}\u{201d} is a read-only drive. FileID can show and search these files but can\u{2019}t change them.")
}

fn refusal(name: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::ReadOnlyFilesystem, LocationReadOnly { name })
}

/// The read-only location named inside an error (walks anyhow context layers), if it came from this guard.
pub fn location_in(error: &anyhow::Error) -> Option<&LocationReadOnly> {
    error.chain().find_map(|layer| layer.downcast_ref::<std::io::Error>()?.get_ref()?.downcast_ref::<LocationReadOnly>())
}

pub fn location_in_io(error: &std::io::Error) -> Option<&LocationReadOnly> {
    error.get_ref()?.downcast_ref::<LocationReadOnly>()
}

#[derive(Clone, Debug)]
pub(crate) struct Volume {
    pub name: String,
    pub read_only: bool,
}

type Probe = dyn Fn(&Path) -> Option<(String, Volume)> + Send + Sync;

type VolumeCache = Mutex<HashMap<String, (Instant, Volume)>>;

fn cache() -> &'static VolumeCache {
    static CACHE: std::sync::OnceLock<Mutex<HashMap<String, (Instant, Volume)>>> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Forget cached volume state so a remount is noticed now (scan start, an explicit listRoots refresh).
pub fn refresh() {
    forget(cache());
}

fn forget(cache: &VolumeCache) {
    if let Ok(mut cache) = cache.lock() {
        cache.clear();
    }
}

#[cfg(test)]
fn faked() -> &'static Mutex<Vec<(PathBuf, String)>> {
    static FAKED: std::sync::OnceLock<Mutex<Vec<(PathBuf, String)>>> = std::sync::OnceLock::new();
    FAKED.get_or_init(Default::default)
}

/// Marks a directory as a read-only volume for tests; a real read-only mount cannot be made in CI.
#[cfg(test)]
pub(crate) fn mark_read_only_for_tests(root: &Path, name: &str) {
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    faked().lock().unwrap().push((root, name.to_owned()));
}

#[cfg(test)]
fn faked_volume(candidate: &Path) -> Option<String> {
    let candidate = candidate.canonicalize().unwrap_or_else(|_| candidate.to_path_buf());
    faked().lock().unwrap().iter().find(|(root, _)| candidate.starts_with(root)).map(|(_, name)| name.clone())
}

fn system_probe(path: &Path) -> Option<(String, Volume)> {
    crate::platform::volume_of(path).map(|(id, name, read_only)| (id, Volume { name, read_only }))
}

fn volume_read_only_with(path: &Path, probe: &Probe) -> Option<String> {
    volume_read_only_in(cache(), path, probe)
}

fn volume_read_only_in(cache: &VolumeCache, path: &Path, probe: &Probe) -> Option<String> {
    #[cfg(test)]
    if let Some(name) = faked_volume(path) {
        return Some(name);
    }
    let mut existing = path;
    let metadata = loop {
        match std::fs::metadata(existing) {
            Ok(metadata) => break metadata,
            Err(_) => existing = existing.parent()?,
        }
    };
    let directory = if metadata.is_dir() { existing } else { existing.parent()? };
    let key = directory.to_string_lossy().into_owned();
    let now = Instant::now();
    if let Some((at, cached)) = cache.lock().ok()?.get(&key)
        && now.duration_since(*at) < VOLUME_TTL
    {
        return cached.read_only.then(|| cached.name.clone());
    }
    let (_, volume) = probe(directory)?;
    let mut cache = cache.lock().ok()?;
    if cache.len() >= CACHE_LIMIT {
        cache.clear();
    }
    cache.insert(key, (now, volume.clone()));
    volume.read_only.then_some(volume.name)
}

/// The name of the read-only drive `path` is on (by volume flag, or the owner's Adlon name guard), if any.
pub fn read_only_location(path: &Path) -> Option<String> {
    let belt = Path::new(READ_ONLY_ROOT);
    let literal = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let root = READ_ONLY_ROOT.to_lowercase();
    if literal == root || literal.starts_with(&format!("{root}/")) {
        return Some(root_name(belt));
    }
    let resolved = resolved(path).ok()?;
    let candidate = resolved.to_string_lossy().replace('\\', "/").to_lowercase();
    static ADLON: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let adlon = ADLON.get_or_init(|| resolved_root(belt)).clone().unwrap_or_else(|| root.clone());
    if candidate == adlon || candidate.starts_with(&format!("{adlon}/")) {
        return Some(root_name(belt));
    }
    volume_read_only_with(&resolved, &system_probe)
}

fn resolved_root(root: &Path) -> Option<String> {
    resolved(root).ok().map(|r| r.to_string_lossy().replace('\\', "/").to_lowercase())
}

fn root_name(root: &Path) -> String {
    root.file_name().map_or_else(|| "this drive".to_owned(), |n| n.to_string_lossy().into_owned())
}

pub fn require_writable(path: &Path) -> std::io::Result<()> {
    match read_only_location(path) {
        Some(name) => Err(refusal(name)),
        None => Ok(()),
    }
}

/// A batch refused whole: how many targets sit on read-only drives, out of how many, and the first drive's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchRefusal {
    pub name: String,
    pub read_only: usize,
    pub total: usize,
}

impl BatchRefusal {
    pub fn sentence(&self) -> String {
        let base = location_sentence(&self.name);
        if self.total <= 1 {
            return base;
        }
        format!("{base} {} of {} files are on it, so nothing was changed.", self.read_only, self.total)
    }
}

/// Checks every target before any side effect. One read-only target refuses the whole batch.
pub fn require_all_writable<'a>(paths: impl IntoIterator<Item = &'a Path>) -> Result<(), BatchRefusal> {
    let (mut total, mut read_only, mut first) = (0usize, 0usize, None);
    for path in paths {
        total += 1;
        if let Some(name) = read_only_location(path) {
            read_only += 1;
            first.get_or_insert(name);
        }
    }
    match first {
        Some(name) => Err(BatchRefusal { name, read_only, total }),
        None => Ok(()),
    }
}

pub fn require_source_mutation(path: &Path) -> std::io::Result<()> {
    require_writable(path)?;
    let path = resolved(path)?;
    let bundles = [".fcpbundle", ".photoslibrary", ".photolibrary", ".aplibrary", ".imovielibrary", ".logicx", ".band"];
    for component in path.components() {
        let component = component.as_os_str().to_string_lossy().to_lowercase();
        if bundles.iter().any(|suffix| component.ends_with(suffix)) || ["finalcutoriginalmedia", "finalcutproxymedia"].contains(&component.split_whitespace().collect::<String>().as_str()) {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "This file belongs to a managed media library; export a new version instead"));
        }
    }
    Ok(())
}

fn resolved(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() { path.to_path_buf() } else { std::env::current_dir()?.join(path) };
    let mut ancestor = absolute.as_path();
    let mut tail = Vec::new();
    while !ancestor.exists() {
        if std::fs::symlink_metadata(ancestor).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "A symbolic link has a missing target; select an existing folder"));
        }
        match (ancestor.file_name(), ancestor.parent()) {
            (Some(name), Some(parent)) => { tail.push(name.to_os_string()); ancestor = parent; }
            _ => break,
        }
    }
    let mut result = ancestor.canonicalize()?;
    for component in tail.into_iter().rev() {
        if component == ".." { result.pop(); } else if component != "." { result.push(component); }
    }
    Ok(result)
}

pub fn require_writable_under(path: &Path, roots: &[PathBuf]) -> std::io::Result<()> {
    let resolved_roots = roots.iter().map(|root| resolved(root)).collect::<std::io::Result<Vec<_>>>()?;
    check_under(path, roots, &resolved_roots)
}

fn check_under(path: &Path, roots: &[PathBuf], resolved_roots: &[PathBuf]) -> std::io::Result<()> {
    let literal = path.to_string_lossy().replace('\\', "/").to_lowercase();
    let candidate = resolved(path)?.to_string_lossy().replace('\\', "/").to_lowercase();
    for (root, resolved_root) in roots.iter().zip(resolved_roots) {
        for (text, subject) in [(root, &literal), (resolved_root, &candidate)] {
            let text = text.to_string_lossy().replace('\\', "/").to_lowercase();
            if *subject == text || subject.starts_with(&(text.trim_end_matches('/').to_owned() + "/")) {
                return Err(refusal(root_name(root)));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_existing_and_future_paths_without_blocking_siblings() {
        let base = std::env::temp_dir().join(format!("fileid-read-only-{}", std::process::id()));
        let protected = base.join("example");
        std::fs::create_dir_all(&protected).unwrap();
        let roots = [protected];
        assert!(require_writable_under(&roots[0], &roots).is_err());
        assert!(require_writable_under(&roots[0].join("missing/export.mp4"), &roots).is_err());
        assert!(require_writable_under(&base.join("example-other/out.mp4"), &roots).is_ok());
        #[cfg(unix)] {
            let alias = base.join("alias");
            std::os::unix::fs::symlink(&roots[0], &alias).unwrap();
            assert!(require_writable_under(&alias.join("future/cache.sqlite"), &roots).is_err());
            let file_alias = base.join("output.png");
            let directory_alias = base.join("output-directory");
            std::os::unix::fs::symlink(roots[0].join("new.png"), &file_alias).unwrap();
            std::os::unix::fs::symlink(roots[0].join("new-directory"), &directory_alias).unwrap();
            assert!(require_writable_under(&file_alias, &roots).is_err());
            assert!(require_writable_under(&directory_alias.join("new.png"), &roots).is_err());
            assert!(!roots[0].join("new.png").exists());
        }
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[cfg(test)]
mod volume_tests {
    use super::*;

    #[test]
    fn a_probe_reporting_a_read_only_volume_marks_every_path_on_it() {
        let dir = std::env::temp_dir().join(format!("fileid-ro-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = VolumeCache::default();
        let probe = |_: &Path| Some(("probe-volume".to_owned(), Volume { name: "Backup".into(), read_only: true }));
        assert_eq!(volume_read_only_in(&cache, &dir.join("not/yet/there.jpg"), &probe).as_deref(), Some("Backup"));
        let writable = |_: &Path| Some(("probe-volume".to_owned(), Volume { name: "Backup".into(), read_only: false }));
        forget(&cache);
        assert_eq!(volume_read_only_in(&cache, &dir, &writable), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_batch_in_one_folder_asks_the_system_about_the_volume_once() {
        let dir = std::env::temp_dir().join(format!("fileid-ro-calls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = VolumeCache::default();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = calls.clone();
        let probe = move |_: &Path| {
            counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Some(("calls-volume".to_owned(), Volume { name: "Backup".into(), read_only: false }))
        };
        for index in 0..50 {
            assert_eq!(volume_read_only_in(&cache, &dir.join(format!("{index}.jpg")), &probe), None);
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_remount_is_noticed_after_the_ttl_or_a_refresh() {
        let dir = std::env::temp_dir().join(format!("fileid-ro-remount-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = VolumeCache::default();
        let ro = |_: &Path| Some(("remount-volume".to_owned(), Volume { name: "Backup".into(), read_only: true }));
        let rw = |_: &Path| Some(("remount-volume".to_owned(), Volume { name: "Backup".into(), read_only: false }));
        assert!(volume_read_only_in(&cache, &dir, &ro).is_some());
        assert!(volume_read_only_in(&cache, &dir, &rw).is_some(), "cached inside the TTL");
        forget(&cache);
        assert!(volume_read_only_in(&cache, &dir, &rw).is_none(), "refresh forgets the cache");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_adlon_name_guard_still_applies_without_a_volume_flag() {
        let error = require_writable(Path::new("/Volumes/Adlon/Photos/a.jpg")).unwrap_err();
        assert_eq!(location_in_io(&error).map(|l| l.name.as_str()), Some("Adlon"));
        assert!(error.to_string().contains("read-only drive"));
    }

    #[test]
    fn a_batch_refusal_counts_the_read_only_targets() {
        let dir = std::env::temp_dir().join(format!("fileid-ro-batch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        mark_read_only_for_tests(&dir, "Vault");
        let paths = [dir.join("a"), std::env::temp_dir().join("elsewhere-fileid.jpg"), dir.join("b")];
        let refusal = require_all_writable(paths.iter().map(PathBuf::as_path)).unwrap_err();
        assert_eq!((refusal.read_only, refusal.total, refusal.name.as_str()), (2, 3, "Vault"));
        assert!(refusal.sentence().contains("2 of 3"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
