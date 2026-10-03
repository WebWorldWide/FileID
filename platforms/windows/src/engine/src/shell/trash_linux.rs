use anyhow::{bail, Context, Result};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn data_home() -> Result<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if !path.is_absolute() {
            bail!("XDG_DATA_HOME must be absolute");
        }
        return Ok(path);
    }
    Ok(PathBuf::from(std::env::var_os("HOME").context("HOME is unset")?)
        .join(".local/share"))
}

fn safe_directory(path: &Path) -> Result<()> {
    if fs::symlink_metadata(path).is_err_and(|err| err.kind() == std::io::ErrorKind::NotFound) {
        DirBuilder::new().recursive(true).mode(0o700).create(path)?;
    }
    if !fs::symlink_metadata(path)?.file_type().is_dir() {
        bail!("Trash directory is not a directory");
    }
    Ok(())
}
fn private_trash_directory(path: &Path) -> Result<()> {
    safe_directory(path)?;
    let meta = fs::symlink_metadata(path)?;
    if meta.uid() != unsafe { libc::geteuid() } || meta.permissions().mode() & 0o077 != 0 {
        bail!("Trash directory must be owned by this user and private");
    }
    Ok(())
}


fn trash_root(path: &Path, data: &Path) -> Result<(PathBuf, PathBuf)> {
    let source = fs::symlink_metadata(path)?;
    let data_parent = data.ancestors().find(|p| p.exists()).context("No data home ancestor")?;
    if fs::metadata(data_parent)?.dev() == source.dev() {
        return Ok((data.join("Trash"), PathBuf::from("/")));
    }
    let mut mount = path.parent().context("Cannot trash filesystem root")?;
    while let Some(parent) = mount.parent() {
        if fs::metadata(parent)?.dev() != source.dev() {
            break;
        }
        mount = parent;
    }
    let uid = unsafe { libc::geteuid() };
    let shared = mount.join(".Trash");
    let root = if fs::symlink_metadata(&shared)
        .is_ok_and(|meta| meta.file_type().is_dir() && meta.permissions().mode() & 0o1000 != 0)
    {
        shared.join(uid.to_string())
    } else {
        mount.join(format!(".Trash-{uid}"))
    };
    Ok((root, mount.to_path_buf()))
}

fn escaped_path(path: &Path) -> String {
    let mut escaped = String::new();
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || b"/-_.~".contains(&byte) {
            escaped.push(byte as char);
        } else {
            escaped.push_str(&format!("%{byte:02X}"));
        }
    }
    escaped
}

fn info_content(path: &Path, mount: &Path) -> Result<String> {
    let from_mount = path.strip_prefix(mount).context("File outside trash volume")?;
    let from_mount = if mount == Path::new("/") { path } else { from_mount };
    Ok(format!(
        "[Trash Info]\nPath={}\nDeletionDate={}\n",
        escaped_path(from_mount),
        chrono::Local::now().format("%Y-%m-%dT%H:%M:%S")
    ))
}

pub fn trash_path_with_receipt(path: &Path) -> Result<PathBuf> {
    trash_path_at(path, &data_home()?)
}

fn trash_path_at(path: &Path, data: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        bail!("Only absolute file paths can be moved to Trash");
    }
    let (root, mount) = trash_root(path, data)?;
    let files = root.join("files");
    let info = root.join("info");
    private_trash_directory(&root)?;
    private_trash_directory(&files)?;
    private_trash_directory(&info)?;
    let base = path.file_name().context("Cannot trash filesystem root")?;
    let metadata = info_content(path, &mount)?;
    for n in 0..10_000 {
        let name = if n == 0 { base.to_os_string() } else {
            let mut value = base.to_os_string();
            value.push(format!(".{n}"));
            value
        };
        let target = files.join(&name);
        let mut info_name = name.clone();
        info_name.push(".trashinfo");
        let info_path = info.join(info_name);
        if target.symlink_metadata().is_ok() {
            continue;
        }
        let mut file = match OpenOptions::new().write(true).create_new(true).mode(0o600).open(&info_path) {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        };
        if let Err(err) = file.write_all(metadata.as_bytes()).and_then(|_| file.sync_all()) {
            fs::remove_file(&info_path).ok();
            return Err(err.into());
        }
        if let Err(err) = fs::rename(path, &target) {
            fs::remove_file(&info_path).ok();
            return Err(err).context("Could not move file into Trash on its filesystem");
        }
        return Ok(target);
    }
    bail!("No unique Trash filename available")
}


pub fn restore_path(path: &Path, receipt: &Path) -> Result<()> {
    restore_path_at(path, receipt, &data_home()?)
}

fn restore_path_at(path: &Path, receipt: &Path, data: &Path) -> Result<()> {
    if !path.is_absolute() || !receipt.is_absolute() {
        bail!("Trash recovery paths must be absolute");
    }
    let (root, mount) = trash_root_for_restore(path, data)?;
    if receipt.parent() != Some(root.join("files").as_path()) {
        bail!("Trash recovery receipt is outside the expected trash directory");
    }
    let name = receipt.file_name().context("Missing Trash entry name")?;
    let mut info_name = name.to_os_string();
    info_name.push(".trashinfo");
    let info_path = root.join("info").join(info_name);
    let expected = format!("Path={}\n", escaped_path(if mount == Path::new("/") { path } else {
        path.strip_prefix(&mount)?
    }));
    if !fs::read_to_string(&info_path)?.lines().any(|line| format!("{line}\n") == expected) {
        bail!("Trash metadata does not match the original file path");
    }
    if path.symlink_metadata().is_ok() {
        bail!("Restore destination is occupied");
    }
    if let Some(parent) = path.parent() {
        safe_directory(parent)?;
    }
    #[cfg(target_os = "linux")]
    {
        use std::ffi::CString;
        let src = CString::new(receipt.as_os_str().as_bytes())?;
        let dest = CString::new(path.as_os_str().as_bytes())?;
        let result = unsafe {
            libc::renameat2(libc::AT_FDCWD, src.as_ptr(), libc::AT_FDCWD, dest.as_ptr(), libc::RENAME_NOREPLACE)
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error()).context("Could not restore file without overwriting destination");
        }
    }
    #[cfg(not(target_os = "linux"))]
    bail!("Trash restore requires Linux renameat2");
    fs::remove_file(info_path).ok();
    Ok(())
}

fn trash_root_for_restore(path: &Path, data: &Path) -> Result<(PathBuf, PathBuf)> {
    // A trashed source no longer exists: use its existing parent to locate the volume.
    let parent = path.parent().context("No original file parent")?;
    let source = parent.ancestors().find(|p| p.exists()).context("No original path ancestor")?;
    trash_root(source, data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_and_restore_never_overwrite_and_preserve_bytes() {
        let root = std::env::temp_dir().join(format!("fileid-trash-{}-{}", std::process::id(), uuid::Uuid::new_v4()));
        let data = root.join("data");
        let library = root.join("library");
        fs::create_dir_all(&library).unwrap();
        let original = library.join("hello é 50%.txt");
        fs::write(&original, b"original").unwrap();
        let receipt = trash_path_at(&original, &data).unwrap();
        assert!(!original.exists());
        assert_eq!(fs::read(&receipt).unwrap(), b"original");
        fs::write(&original, b"replacement").unwrap();
        assert!(restore_path_at(&original, &receipt, &data).is_err());
        assert_eq!(fs::read(&original).unwrap(), b"replacement");
        fs::remove_file(&original).unwrap();
        restore_path_at(&original, &receipt, &data).unwrap();
        assert_eq!(fs::read(&original).unwrap(), b"original");
        assert!(!receipt.exists());
        fs::remove_dir_all(root).unwrap();
    }
}
