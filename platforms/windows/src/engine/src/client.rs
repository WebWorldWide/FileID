//! Client side of the engine's NDJSON stdio protocol, for front ends that run
//! the engine binary as a child process (CLI, TUI, GTK).

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};

use crate::ipc::{
    CommandPayload, Empty, EventPayload, FaceClusteringResult, IpcCommand, IpcEvent, ScanComplete, StartScanPayload,
};

/// Largest engine frame a client accepts; longer lines are discarded whole.
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

pub fn engine_exe_name() -> &'static str {
    if cfg!(windows) {
        "FileIDEngine.exe"
    } else {
        "FileIDEngine"
    }
}

/// Find the engine binary: `$FILEID_ENGINE_BIN` / `$FILEID_ENGINE`, beside the
/// current executable, the dev-layout engine target dir, system install dirs,
/// then `PATH`.
pub fn locate_engine() -> Option<PathBuf> {
    let exe = engine_exe_name();
    for var in ["FILEID_ENGINE_BIN", "FILEID_ENGINE"] {
        if let Some(path) = std::env::var_os(var).filter(|value| !value.is_empty()).map(PathBuf::from) {
            if path.is_file() {
                return Some(path);
            }
        }
    }
    let mut candidates = Vec::new();
    if let Some(dir) = std::env::current_exe().ok().and_then(|exe| exe.parent().map(Path::to_path_buf)) {
        candidates.push(dir.join(exe));
        candidates.push(dir.join("fileid-engine"));
        if let Some(platforms) = dir.ancestors().find(|ancestor| ancestor.ends_with("platforms")) {
            for profile in ["release", "debug"] {
                candidates.push(platforms.join("windows/src/engine/target").join(profile).join(exe));
            }
        }
    }
    if cfg!(all(unix, not(target_os = "macos"))) {
        for system in ["/usr/lib/FileID/FileIDEngine", "/usr/libexec/FileID/FileIDEngine", "/usr/lib/FileID/fileid-engine"] {
            candidates.push(PathBuf::from(system));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|dir| dir.join(exe)));
    }
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// Spawn the engine with piped stdin/stdout and the given stderr disposition.
pub fn spawn(engine: &Path, envs: &[(&str, &OsStr)], stderr: Stdio) -> std::io::Result<Child> {
    let mut command = Command::new(engine);
    command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(stderr);
    for (key, value) in envs {
        command.env(key, value);
    }
    command.spawn()
}

pub fn send_command<W: Write>(writer: &mut W, command: &IpcCommand) -> Result<()> {
    let mut line = serde_json::to_vec(command).context("serializing engine command")?;
    line.push(b'\n');
    writer
        .write_all(&line)
        .and_then(|()| writer.flush())
        .context("writing engine command")
}

pub fn start_scan_command(id: &str, root: &Path, rescan: bool) -> IpcCommand {
    IpcCommand {
        id: id.to_string(),
        payload: CommandPayload::StartScan(StartScanPayload {
            root_path: root.to_string_lossy().into_owned(),
            root_bookmark: None,
            root_display: None,
            rescan,
            excluded_paths: None,
        }),
    }
}

pub enum BoundedLine {
    Eof,
    Line,
    Oversize,
}

/// Read one `\n`-terminated line into `buffer` without ever holding more than
/// `max_bytes`. An oversize line is consumed through its newline and reported
/// as `Oversize`, so the stream resynchronizes on the next frame.
pub fn read_bounded_line<R: BufRead>(reader: &mut R, buffer: &mut Vec<u8>, max_bytes: usize) -> std::io::Result<BoundedLine> {
    buffer.clear();
    let mut draining = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if draining {
                BoundedLine::Oversize
            } else if buffer.is_empty() {
                BoundedLine::Eof
            } else {
                BoundedLine::Line
            });
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.unwrap_or(available.len());
        if draining || buffer.len().saturating_add(take) > max_bytes {
            draining = true;
        } else {
            buffer.extend_from_slice(&available[..take]);
        }
        reader.consume(take + usize::from(newline.is_some()));
        if newline.is_some() {
            if draining {
                buffer.clear();
                return Ok(BoundedLine::Oversize);
            }
            if buffer.last() == Some(&b'\r') {
                buffer.pop();
            }
            return Ok(BoundedLine::Line);
        }
    }
}

pub enum Frame {
    Event(Box<IpcEvent>),
    Oversize,
}

/// Feed every engine event (and every discarded oversize line) to `on_frame`
/// until EOF or until it returns `false`. Blank and non-event lines are skipped.
pub fn read_frames<R: BufRead>(mut reader: R, max_bytes: usize, mut on_frame: impl FnMut(Frame) -> bool) -> std::io::Result<()> {
    let mut line = Vec::with_capacity(8 * 1024);
    loop {
        let frame = match read_bounded_line(&mut reader, &mut line, max_bytes)? {
            BoundedLine::Eof => return Ok(()),
            BoundedLine::Oversize => Frame::Oversize,
            BoundedLine::Line => match serde_json::from_slice::<IpcEvent>(&line) {
                Ok(event) => Frame::Event(Box::new(event)),
                Err(_) => continue,
            },
        };
        if !on_frame(frame) {
            return Ok(());
        }
    }
}

pub enum ScanOutcome {
    Complete {
        scan: ScanComplete,
        clustering: FaceClusteringResult,
    },
    Error {
        kind: String,
        message: String,
    },
    Aborted,
}

/// Run one scan conversation over an engine's stdout/stdin after `startScan`
/// was sent: forward every event payload to `on_event`, request face
/// clustering once the scan completes, and return when clustering finishes,
/// the engine reports an error, or the stream ends.
pub fn drive_scan<R: Read, W: Write>(
    stdout: R,
    stdin: &mut W,
    request_id: &str,
    mut on_event: impl FnMut(&EventPayload),
) -> ScanOutcome {
    let mut scan: Option<ScanComplete> = None;
    let mut outcome = None;
    let read = read_frames(BufReader::new(stdout), MAX_FRAME_BYTES, |frame| {
        let Frame::Event(event) = frame else { return true };
        on_event(&event.payload);
        match event.payload {
            EventPayload::ScanComplete(complete) => {
                scan = Some(complete.inner);
                let command = IpcCommand {
                    id: format!("{request_id}-face-clustering"),
                    payload: CommandPayload::RunFaceClustering(Empty {}),
                };
                if let Err(error) = send_command(stdin, &command) {
                    outcome = Some(ScanOutcome::Error {
                        kind: "face_clustering_start_failed".to_string(),
                        message: error.to_string(),
                    });
                }
            }
            EventPayload::FaceClusteringComplete(result) => {
                if let Some(scan) = scan.take() {
                    outcome = Some(ScanOutcome::Complete {
                        scan,
                        clustering: result.inner,
                    });
                }
            }
            EventPayload::Error(error) => {
                outcome = Some(ScanOutcome::Error {
                    kind: error.inner.kind,
                    message: error.inner.message,
                });
            }
            _ => {}
        }
        outcome.is_none()
    });
    if let Some(outcome) = outcome {
        return outcome;
    }
    match (scan, read) {
        (Some(_), _) => ScanOutcome::Error {
            kind: "face_clustering_aborted".to_string(),
            message: "engine exited before face clustering completed".to_string(),
        },
        (None, Err(error)) => ScanOutcome::Error {
            kind: "engine_output_error".to_string(),
            message: error.to_string(),
        },
        (None, Ok(())) => ScanOutcome::Aborted,
    }
}

/// Wait for the engine to exit after stdin EOF, killing it after `timeout`.
pub fn wait_briefly(child: &mut Child, timeout: Duration) -> std::io::Result<()> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            return child.wait().map(|_| ());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{EngineError, Wrap};

    fn stream(events: &[EventPayload]) -> String {
        events
            .iter()
            .map(|payload| serde_json::to_string(&IpcEvent::now(payload.clone())).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn complete() -> EventPayload {
        EventPayload::ScanComplete(Wrap::new(ScanComplete {
            session_id: "s".into(),
            total_files: 12,
            processed_files: 11,
            failed_files: 1,
            total_seconds: 2.5,
        }))
    }

    fn clustered() -> EventPayload {
        EventPayload::FaceClusteringComplete(Wrap::new(FaceClusteringResult {
            person_count: 3,
            face_count: 9,
            unmatched_faces: 2,
            duration_seconds: 0.4,
            deferred_faces: None,
        }))
    }

    #[test]
    fn scan_completion_requests_face_clustering_before_success() {
        let mut commands = Vec::new();
        let mut seen = 0;
        let input = format!("noise\n\n{}", stream(&[complete(), clustered()]));
        let outcome = drive_scan(input.as_bytes(), &mut commands, "cli-scan", |_| seen += 1);
        let ScanOutcome::Complete { scan, clustering } = outcome else {
            panic!("scan must finish after clustering");
        };
        assert_eq!((scan.processed_files, clustering.person_count, seen), (11, 3, 2));
        let command: IpcCommand = serde_json::from_slice(&commands).unwrap();
        assert_eq!(command.id, "cli-scan-face-clustering");
        assert!(matches!(command.payload, CommandPayload::RunFaceClustering(_)));
    }

    #[test]
    fn errors_and_early_exit_are_distinct_outcomes() {
        let error = EventPayload::Error(Wrap::new(EngineError {
            kind: "models_not_installed".into(),
            message: "install".into(),
            path: None,
            model_kind: None,
        }));
        let mut sink = Vec::new();
        let input = stream(&[error, complete()]);
        assert!(matches!(
            drive_scan(input.as_bytes(), &mut sink, "x", |_| {}),
            ScanOutcome::Error { kind, .. } if kind == "models_not_installed"
        ));
        assert!(sink.is_empty(), "nothing after the error is processed");
        assert!(matches!(drive_scan(&b""[..], &mut sink, "x", |_| {}), ScanOutcome::Aborted));
        let input = stream(&[complete()]);
        assert!(matches!(
            drive_scan(input.as_bytes(), &mut sink, "x", |_| {}),
            ScanOutcome::Error { kind, .. } if kind == "face_clustering_aborted"
        ));
    }

    #[test]
    fn frame_reader_rejects_oversize_and_resynchronizes() {
        let event = stream(&[complete()]);
        let input = format!("{}\n{event}\r\n", "x".repeat(event.len() + 2));
        let mut frames = Vec::new();
        read_frames(input.as_bytes(), event.len() + 1, |frame| {
            frames.push(matches!(frame, Frame::Event(_)));
            true
        })
        .unwrap();
        assert_eq!(frames, [false, true]);
    }

    #[test]
    fn frame_reader_bounds_unterminated_input() {
        let mut reader = std::io::Cursor::new(vec![b'x'; 4096]);
        let mut buffer = Vec::new();
        assert!(matches!(read_bounded_line(&mut reader, &mut buffer, 16).unwrap(), BoundedLine::Oversize));
        assert!(buffer.len() <= 16);
        assert!(matches!(read_bounded_line(&mut reader, &mut buffer, 16).unwrap(), BoundedLine::Eof));
    }
}
