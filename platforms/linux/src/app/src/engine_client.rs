// Engine client — spawns the shared Rust engine as a subprocess and
// talks newline-delimited JSON over stdin/stdout. Mirror of Windows
// EngineClient.cs (substantially smaller because the GTK app is
// single-threaded on the main context and the scaffold only handles
// the four state transitions it needs to drive the HeaderBar status
// label).
//
// Phase 1 will replace this with a full IpcCommand/IpcEvent router
// that fans events out to per-tab subscribers, matching the macOS +
// Windows clients.

use anyhow::{Context, Result};
use async_channel::{Receiver, Sender};
use serde::Serialize;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Debug, Clone)]
pub enum EngineState {
    Spawning,
    Ready,
    Scanning,
    Done(u64),
    Failed(String),
}

pub struct EngineClient {
    child: Option<Child>,
    stdin: Option<Arc<Mutex<ChildStdin>>>,
    tx: Option<Sender<EngineState>>,
}

impl EngineClient {
    pub fn new() -> Self {
        Self { child: None, stdin: None, tx: None }
    }

    /// Spawn the engine binary. Returns a Receiver the UI subscribes
    /// to for state-change events on the GTK main context.
    pub fn spawn(&mut self) -> Receiver<EngineState> {
        let (tx, rx) = async_channel::unbounded::<EngineState>();
        self.tx = Some(tx.clone());

        let exe = locate_engine_binary();
        let send_failed = |tx: &Sender<EngineState>, msg: String| {
            // Best-effort — channel can't be closed at this point.
            let _ = tx.send_blocking(EngineState::Failed(msg));
        };

        match exe {
            Ok(path) => {
                let _ = tx.send_blocking(EngineState::Spawning);
                match Command::new(&path)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn()
                {
                    Ok(mut child) => {
                        let stdin = child.stdin.take()
                            .expect("piped stdin should be present");
                        let stdout = child.stdout.take()
                            .expect("piped stdout should be present");
                        self.stdin = Some(Arc::new(Mutex::new(stdin)));
                        // Reader thread: parse NDJSON, push EngineState
                        // events down the channel for the GTK main loop.
                        let tx_reader = tx.clone();
                        thread::spawn(move || drain_engine_stdout(stdout, tx_reader));
                        self.child = Some(child);
                    }
                    Err(err) => send_failed(&tx, format!("spawn failed: {err}")),
                }
            }
            Err(err) => send_failed(&tx, format!("engine binary not found: {err}")),
        }
        rx
    }

    /// Send `startScan` to the engine. Caller has already collected the
    /// folder path from the FileDialog.
    pub fn start_scan(&mut self, root_path: &str) -> Result<()> {
        let cmd = StartScanWire {
            cmd: "startScan",
            id: "scan-1",
            root_path: root_path.to_string(),
        };
        let line = serde_json::to_string(&cmd)? + "\n";
        let stdin = self.stdin.as_ref()
            .context("engine not spawned")?
            .clone();
        let mut guard = stdin.lock().expect("engine stdin lock poisoned");
        guard.write_all(line.as_bytes())?;
        guard.flush()?;
        if let Some(tx) = &self.tx {
            let _ = tx.send_blocking(EngineState::Scanning);
        }
        Ok(())
    }
}

impl Drop for EngineClient {
    fn drop(&mut self) {
        // Close stdin so the engine sees EOF and exits cleanly.
        self.stdin.take();
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Locate the engine binary. Search order:
///   1. `$FILEID_ENGINE` environment variable
///   2. `target/release/FileIDEngine` next to the app binary (dev/staged)
///   3. `/usr/lib/FileID/FileIDEngine` (installed via .deb / Flatpak)
fn locate_engine_binary() -> Result<PathBuf> {
    if let Ok(s) = std::env::var("FILEID_ENGINE") {
        let p = PathBuf::from(s);
        if p.exists() { return Ok(p); }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in ["FileIDEngine", "fileid-engine"] {
                let p = dir.join(candidate);
                if p.exists() { return Ok(p); }
            }
        }
    }
    for sys in ["/usr/lib/FileID/FileIDEngine", "/usr/libexec/FileID/FileIDEngine"] {
        let p = PathBuf::from(sys);
        if p.exists() { return Ok(p); }
    }
    anyhow::bail!("engine binary not found (set FILEID_ENGINE or place beside the app exe)")
}

fn drain_engine_stdout(stdout: std::process::ChildStdout, tx: Sender<EngineState>) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        // Tolerant parse: extract only the event-type discriminator we
        // care about for the scaffold's state transitions. Phase 1
        // routes the full IpcEvent payload via the engine's serde types.
        if line.contains("\"ready\"") {
            let _ = tx.send_blocking(EngineState::Ready);
        } else if line.contains("\"scanComplete\"") {
            // Extract total file count if present.
            let n = extract_u64(&line, "processed").unwrap_or(0);
            let _ = tx.send_blocking(EngineState::Done(n));
        } else if line.contains("\"error\"") {
            let _ = tx.send_blocking(EngineState::Failed(line));
        }
    }
}

fn extract_u64(s: &str, key: &str) -> Option<u64> {
    let needle = format!("\"{key}\":");
    let idx = s.find(&needle)?;
    let after = &s[idx + needle.len()..];
    let digits: String = after.chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

#[derive(Serialize)]
struct StartScanWire {
    cmd: &'static str,
    id: &'static str,
    #[serde(rename = "rootPath")]
    root_path: String,
}
