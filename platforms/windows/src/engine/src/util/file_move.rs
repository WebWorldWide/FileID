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
        io.publish_stage(&stage, target)?;
        on_published()?;
        // Cleanup failure must leave the source present, not hide a committed move from its journal.
        io.remove_stage(&stage)?;
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
