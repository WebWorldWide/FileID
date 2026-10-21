use std::{fs, io, path::Path};

pub(crate) fn move_no_clobber(
    source: &Path,
    target: &Path,
    on_published: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    let source = super::path_safety::to_extended_length(source);
    let target = super::path_safety::to_extended_length(target);
    move_with_io(&source, &target, &FileSystem, on_published)
}

pub(crate) trait MoveFileIo {
    fn link_source(&self, source: &Path, target: &Path) -> io::Result<()> {
        fs::hard_link(source, target)
    }
    fn copy(&self, source: &mut fs::File, target: &mut fs::File) -> io::Result<u64> {
        io::copy(source, target)
    }
    fn publish_stage(&self, stage: &Path, target: &Path) -> io::Result<()> {
        fs::hard_link(stage, target)
    }
    fn remove_source(&self, source: &Path) -> io::Result<()> {
        fs::remove_file(source)
    }
    fn remove_stage(&self, stage: &Path) -> io::Result<()> {
        fs::remove_file(stage)
    }
    fn rename_no_replace(&self, source: &Path, target: &Path) -> io::Result<()> {
        super::path_safety::rename_no_replace(source, target)
    }
}

/// The volume has no hard links (FAT, exFAT, many network shares), or the
/// source is at its link limit. A no-replace rename gives the same no-clobber
/// guarantee there, without the moment where both names exist.
fn links_unavailable(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::Unsupported {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(error.raw_os_error(), Some(code) if code == libc::EPERM || code == libc::EMLINK)
    }
    #[cfg(windows)]
    {
        const ERROR_INVALID_FUNCTION: i32 = 1;
        const ERROR_TOO_MANY_LINKS: i32 = 1142;
        matches!(error.raw_os_error(), Some(ERROR_INVALID_FUNCTION | ERROR_TOO_MANY_LINKS))
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}
struct FileSystem;
impl MoveFileIo for FileSystem {}

pub(crate) fn move_with_io(
    source: &Path,
    target: &Path,
    io: &impl MoveFileIo,
    mut on_published: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    if !fs::symlink_metadata(source)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Move source must be a regular file",
        ));
    }
    match io.link_source(source, target) {
        Ok(()) => {
            on_published()?;
            return io.remove_source(source);
        }
        Err(error) if error.kind() == io::ErrorKind::CrossesDevices => {}
        Err(error) if links_unavailable(&error) => {
            io.rename_no_replace(source, target)?;
            return on_published();
        }
        Err(error) => return Err(error),
    }
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Move destination has no parent",
        )
    })?;
    let stage = parent.join(format!(".fileid-move-{}", uuid::Uuid::new_v4()));
    let mut output = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stage)?;
    let result = (|| {
        let mut input = fs::File::open(source)?;
        io.copy(&mut input, &mut output)?;
        output.set_permissions(input.metadata()?.permissions())?;
        output.sync_all()?;
        drop(output);
        let stage_kept = match io.publish_stage(&stage, target) {
            Ok(()) => true,
            Err(error) if links_unavailable(&error) => {
                io.rename_no_replace(&stage, target)?;
                false
            }
            Err(error) => return Err(error),
        };
        on_published()?;
        // Cleanup failure must leave the source present, not hide a committed move from its journal.
        if stage_kept {
            io.remove_stage(&stage)?;
        }
        io.remove_source(source)
    })();
    if result.is_err() {
        let _ = io.remove_stage(&stage);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    struct CrossDevice {
        copy_error: bool,
        cleanup_error: bool,
        source_error: bool,
        race: bool,
    }
    impl MoveFileIo for CrossDevice {
        fn link_source(&self, _: &Path, _: &Path) -> io::Result<()> {
            Err(io::ErrorKind::CrossesDevices.into())
        }
        fn copy(&self, from: &mut fs::File, to: &mut fs::File) -> io::Result<u64> {
            if self.copy_error {
                Err(io::ErrorKind::StorageFull.into())
            } else {
                io::copy(from, to)
            }
        }
        fn publish_stage(&self, stage: &Path, target: &Path) -> io::Result<()> {
            if self.race {
                fs::write(target, b"racing occupant")?;
            }
            fs::hard_link(stage, target)
        }
        fn remove_stage(&self, stage: &Path) -> io::Result<()> {
            if self.cleanup_error {
                Err(io::ErrorKind::PermissionDenied.into())
            } else {
                fs::remove_file(stage)
            }
        }
        fn remove_source(&self, source: &Path) -> io::Result<()> {
            if self.source_error {
                Err(io::ErrorKind::PermissionDenied.into())
            } else {
                fs::remove_file(source)
            }
        }
    }
    struct NoLinks {
        cross_device: bool,
    }
    impl MoveFileIo for NoLinks {
        fn link_source(&self, _: &Path, _: &Path) -> io::Result<()> {
            Err(if self.cross_device {
                io::ErrorKind::CrossesDevices.into()
            } else {
                io::ErrorKind::Unsupported.into()
            })
        }
        fn publish_stage(&self, _: &Path, _: &Path) -> io::Result<()> {
            Err(io::ErrorKind::Unsupported.into())
        }
    }
    #[test]
    fn volumes_without_hard_links_still_move_without_clobbering() {
        for cross_device in [false, true] {
            let directory = Directory(
                std::env::temp_dir().join(format!("fileid-no-links-{}", uuid::Uuid::new_v4())),
            );
            fs::create_dir(&directory.0).unwrap();
            let source = directory.0.join("source");
            let target = directory.0.join("target");
            let io = NoLinks { cross_device };

            fs::write(&source, b"source bytes").unwrap();
            fs::write(&target, b"occupant").unwrap();
            let mut published = false;
            assert!(move_with_io(&source, &target, &io, || {
                published = true;
                Ok(())
            })
            .is_err());
            assert!(!published);
            assert_eq!(fs::read(&source).unwrap(), b"source bytes");
            assert_eq!(fs::read(&target).unwrap(), b"occupant");
            assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);

            fs::remove_file(&target).unwrap();
            move_with_io(&source, &target, &io, || {
                published = true;
                Ok(())
            })
            .unwrap();
            assert!(published);
            assert!(!source.exists());
            assert_eq!(fs::read(&target).unwrap(), b"source bytes");
            assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
        }
    }
    #[test]
    fn cross_device_copy_cleanup_delete_and_publication_failures_preserve_true_state() {
        for scenario in ["complete", "copy", "cleanup", "delete", "race"] {
            let directory = Directory(
                std::env::temp_dir().join(format!("fileid-safe-move-{}", uuid::Uuid::new_v4())),
            );
            fs::create_dir(&directory.0).unwrap();
            let source = directory.0.join("source");
            let target = directory.0.join("target");
            fs::write(&source, b"source bytes").unwrap();
            let io = CrossDevice {
                copy_error: scenario == "copy",
                cleanup_error: scenario == "cleanup",
                source_error: scenario == "delete",
                race: scenario == "race",
            };
            let mut published = false;
            let result = move_with_io(&source, &target, &io, || {
                published = true;
                Ok(())
            });
            if scenario == "complete" {
                assert!(result.is_ok());
                assert!(!source.exists());
            } else {
                assert!(result.is_err());
                assert_eq!(fs::read(&source).unwrap(), b"source bytes");
            }
            if scenario == "copy" {
                assert!(!target.exists());
                assert!(!published);
            } else if scenario == "race" {
                assert_eq!(fs::read(&target).unwrap(), b"racing occupant");
                assert!(!published);
            } else {
                assert_eq!(fs::read(&target).unwrap(), b"source bytes");
                assert!(published);
            }
            assert_eq!(
                fs::read_dir(&directory.0).unwrap().count(),
                match scenario {
                    "complete" | "copy" => 1,
                    "cleanup" => 3,
                    _ => 2,
                }
            );
        }
    }
}
