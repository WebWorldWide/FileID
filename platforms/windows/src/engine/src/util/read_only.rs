use std::path::{Path, PathBuf};

const READ_ONLY_ROOT: &str = "/Volumes/Adlon";

pub fn require_writable(path: &Path) -> std::io::Result<()> {
    // Resolving the root costs a realpath (statfs on a mount point) and runs twice
    // per restructure move; its resolution never changes while the engine runs.
    static RESOLVED: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    match RESOLVED.get_or_init(|| resolved(Path::new(READ_ONLY_ROOT)).ok()) {
        Some(root) => check_under(path, &[PathBuf::from(READ_ONLY_ROOT)], &[root.clone()]),
        None => require_writable_under(path, &[PathBuf::from(READ_ONLY_ROOT)]),
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
    for root in roots {
        let root = root.to_string_lossy().replace('\\', "/").to_lowercase();
        if literal == root || literal.starts_with(&(root.trim_end_matches('/').to_owned() + "/")) {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "This location is read-only example data"));
        }
    }
    let candidate = resolved(path)?.to_string_lossy().replace('\\', "/").to_lowercase();
    for root in resolved_roots {
        let root = root.to_string_lossy().replace('\\', "/").to_lowercase();
        if candidate == root || candidate.starts_with(&(root.trim_end_matches('/').to_owned() + "/")) {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "This location is read-only example data"));
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
