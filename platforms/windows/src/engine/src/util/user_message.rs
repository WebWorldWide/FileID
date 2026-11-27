//! Plain-words causes for I/O failures, so raw OS strings ("No space left on device (os error 28)") never reach the UI.

use std::io::ErrorKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoCause {
    DiskFull,
    ReadOnly,
    NameTooLong,
}

impl IoCause {
    pub fn sentence(self) -> &'static str {
        match self {
            Self::DiskFull => "There isn’t enough free space on this disk. Free some space, then try again.",
            Self::ReadOnly => "This folder is on a read-only disk, so FileID can’t change it.",
            Self::NameTooLong => "That name is too long. The limit is 255 bytes: about 255 letters, but only about 85 characters of Japanese, Chinese or Korean text.",
        }
    }
}

fn io_error_cause(error: &std::io::Error) -> Option<IoCause> {
    match error.kind() {
        ErrorKind::StorageFull => return Some(IoCause::DiskFull),
        ErrorKind::ReadOnlyFilesystem => return Some(IoCause::ReadOnly),
        ErrorKind::InvalidFilename => return Some(IoCause::NameTooLong),
        _ => {}
    }
    #[cfg(windows)]
    match error.raw_os_error() {
        Some(39 | 112) => return Some(IoCause::DiskFull),
        Some(19) => return Some(IoCause::ReadOnly),
        Some(206) => return Some(IoCause::NameTooLong),
        _ => {}
    }
    None
}

/// Walks every context layer and wrapped source; anyhow's Display hides the io error underneath.
pub fn io_cause(error: &anyhow::Error) -> Option<IoCause> {
    error.chain().find_map(|layer| {
        if let Some(io) = layer.downcast_ref::<std::io::Error>() {
            return io_error_cause(io);
        }
        if let Some(rusqlite::Error::SqliteFailure(failure, _)) = layer.downcast_ref::<rusqlite::Error>()
            && failure.code == rusqlite::ErrorCode::DiskFull
        {
            return Some(IoCause::DiskFull);
        }
        None
    })
}

/// The sentence naming the read-only drive when the error came from the write guard.
pub fn read_only_sentence(error: &anyhow::Error) -> Option<String> {
    crate::util::read_only::location_in(error).map(ToString::to_string)
}

pub const DISK_ALMOST_FULL: &str =
    "Your disk is almost full, so FileID couldn’t save everything it learned. Free some space and scan again.";

/// The text stored on a failed background job: the disk-full sentence, otherwise the error as written.
pub fn job_failure_text(error: &anyhow::Error) -> String {
    if let Some(sentence) = read_only_sentence(error) {
        return sentence;
    }
    match io_cause(error) {
        Some(IoCause::DiskFull) => DISK_ALMOST_FULL.to_string(),
        _ => format!("{error:#}").chars().take(2_000).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn os(code: i32) -> anyhow::Error {
        anyhow::Error::from(std::io::Error::from_raw_os_error(code)).context("outer layer")
    }

    #[cfg(unix)]
    #[test]
    fn unix_errnos_map_to_causes() {
        assert_eq!(io_cause(&os(28)), Some(IoCause::DiskFull));
        assert_eq!(io_cause(&os(30)), Some(IoCause::ReadOnly));
        #[cfg(target_os = "macos")]
        assert_eq!(io_cause(&os(63)), Some(IoCause::NameTooLong));
        #[cfg(target_os = "linux")]
        assert_eq!(io_cause(&os(36)), Some(IoCause::NameTooLong));
        assert_eq!(io_cause(&os(13)), None);
    }

    #[test]
    fn sqlite_disk_full_is_disk_full() {
        let failure = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(13), None);
        assert_eq!(io_cause(&anyhow::Error::from(failure)), Some(IoCause::DiskFull));
        assert_eq!(io_cause(&anyhow::anyhow!("something else")), None);
    }

    #[test]
    fn a_full_disk_job_failure_reads_in_plain_words() {
        let failure = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(13), Some("database or disk is full".into()));
        assert_eq!(job_failure_text(&anyhow::Error::from(failure).context("writing takes")), DISK_ALMOST_FULL);
        assert_eq!(job_failure_text(&anyhow::anyhow!("worker interrupted")), "worker interrupted");
    }

    #[test]
    fn sentences_carry_no_os_strings() {
        for cause in [IoCause::DiskFull, IoCause::ReadOnly, IoCause::NameTooLong] {
            assert!(!cause.sentence().contains("os error"), "{cause:?}");
        }
    }
}
