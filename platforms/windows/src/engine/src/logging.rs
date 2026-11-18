//! Tracing subscriber + panic hook; both must be installed before any log call site fires.

use anyhow::{Context, Result};

use crate::paths;

/// Daily rolling JSON file in `%LOCALAPPDATA%/FileID/logs/` plus a stderr layer. Local-only; PII redaction happens at
/// call sites.
pub(crate) fn init() -> Result<()> {
    use tracing_subscriber::{filter::LevelFilter, fmt, prelude::*, EnvFilter};

    let logs_dir = paths::logs_dir().context("resolving logs dir")?;
    crate::util::read_only::require_writable(&logs_dir)?;
    std::fs::create_dir_all(&logs_dir).context("creating logs dir")?;

    let file_appender = tracing_appender::rolling::daily(&logs_dir, "engine.jsonl");
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);
    // Leaked so the appender flushes on every event; the engine is short-lived.
    Box::leak(Box::new(file_guard));

    let file_layer = fmt::layer()
        .json()
        .with_writer(file_writer)
        .with_target(true)
        .with_current_span(false);

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
