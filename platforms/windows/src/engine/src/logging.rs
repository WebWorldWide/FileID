//! Tracing subscriber + panic hook; both must be installed before any log call site fires.

use anyhow::{Context, Result};

use crate::paths;

/// Daily rolling JSON file in `%LOCALAPPDATA%/FileID/logs/` plus a stderr layer. Local-only; PII redaction happens at
/// call sites. A log that can't be written (full disk, read-only or missing folder) never stops the engine: it keeps
/// running with the stderr layer alone.
pub(crate) fn init() -> Result<()> {
    use tracing_subscriber::{filter::LevelFilter, fmt, prelude::*, EnvFilter};

    let file_layer = match open_log_writer() {
        Ok(file_writer) => Some(
            fmt::layer()
                .json()
                .with_writer(file_writer)
                .with_target(true)
                .with_current_span(false),
        ),
        Err(error) => {
            note_stderr(format_args!("FileIDEngine: no log file ({error:#}); logging to stderr only"));
            None
        }
    };

    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .with_target(true)
        .with_filter(LevelFilter::WARN);

    let env_filter = EnvFilter::try_from_env("FILEID_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(file_layer)
        .with(stderr_layer)
        .init();

    Ok(())
}

fn open_log_writer() -> Result<tracing_appender::non_blocking::NonBlocking> {
    open_log_writer_in(&paths::logs_dir().context("resolving logs dir")?)
}

fn open_log_writer_in(logs_dir: &std::path::Path) -> Result<tracing_appender::non_blocking::NonBlocking> {
    crate::util::read_only::require_writable(logs_dir)?;
    std::fs::create_dir_all(logs_dir).context("creating logs dir")?;

    // The builder reports a file it can't create; `rolling::daily` panics instead.
    let file_appender = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("engine.jsonl")
        .build(logs_dir)
        .context("opening the log file")?;
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);
    // Leaked so the appender flushes on every event; the engine is short-lived.
    Box::leak(Box::new(file_guard));
    Ok(file_writer)
}

/// Writes one line to stderr and ignores a failure. `eprintln!` panics when stderr is closed or its disk is full, and
/// a note about the log must never take the engine down.
pub(crate) fn note_stderr(message: std::fmt::Arguments<'_>) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "{message}");
}

/// Without it a panic crashes the engine silently (the C# app sees a broken pipe with no traceable cause); unwinding is
/// unchanged, it just logs file:line + backtrace first.
pub(crate) fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "(unknown location)".to_string());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_string());
        let backtrace = std::backtrace::Backtrace::force_capture();
        tracing::error!(
            location = %location,
            message = %message,
            backtrace = %backtrace,
            "engine panic"
        );
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_folder_that_cannot_be_created_is_an_error_not_a_panic() {
        let dir = std::env::temp_dir().join(format!("fileid-log-blocked-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let blocker = dir.join("not-a-folder");
        std::fs::write(&blocker, b"file").unwrap();
        assert!(open_log_writer_in(&blocker.join("logs")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_writable_log_folder_opens() {
        let dir = std::env::temp_dir().join(format!("fileid-log-open-{}", uuid::Uuid::new_v4()));
        assert!(open_log_writer_in(&dir).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Re-runs this test in a child whose stderr is a pipe nobody reads: `eprintln!` panics there, `note_stderr` does not.
    #[cfg(unix)]
    #[test]
    fn a_closed_stderr_never_takes_the_process_down() {
        use std::process::{Command, Stdio};
        const HELPER: &str = "FILEID_TEST_CLOSED_STDERR_HELPER";
        if let Some(mode) = std::env::var_os(HELPER) {
            std::thread::sleep(std::time::Duration::from_millis(400));
            if mode == "eprintln" {
                eprintln!("noted");
            } else {
                note_stderr(format_args!("noted"));
            }
            return;
        }
        let run = |mode: &str| {
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "logging::tests::a_closed_stderr_never_takes_the_process_down", "--nocapture"])
                .env(HELPER, mode)
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            drop(child.stderr.take());
            child.wait().unwrap()
        };
        assert!(!run("eprintln").success(), "control: eprintln! on a closed stderr panics");
        assert!(run("note").success(), "note_stderr ignores the failed write");
    }
}
