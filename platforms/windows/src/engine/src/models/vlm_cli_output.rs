use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
use anyhow::{anyhow, bail, Context, Result};
use tokio::{io::{AsyncBufReadExt, AsyncReadExt, BufReader}, process::Command};
use crate::resource_scheduler::Lease;
use super::super::native_process::{OwnedProcess, ProcessTracker};

const MAX_STDOUT: u64 = 1024 * 1024;
const MAX_STDERR: u64 = 256 * 1024;
struct Drain { task: tokio::task::JoinHandle<Result<()>>, done: bool }
impl Drop for Drain { fn drop(&mut self) { self.task.abort(); } }

pub(super) async fn run_caption_command(
    mut command: Command, resources: Arc<Lease>, cancel: Arc<AtomicBool>,
    image: &std::path::Path, mut on_token: impl FnMut(&str), tracker: &ProcessTracker,
) -> Result<String> {
    if cancel.load(Ordering::Relaxed) { bail!("VLM startup cancelled"); }
    let (process, stdout, stderr) = OwnedProcess::spawn_piped(&mut command, Some(resources.clone()), tracker)
        .context("spawn owned VLM CLI")?;
    let (Some(stdout), Some(stderr)) = (stdout, stderr) else {
        process.shutdown().await.context("cleanup VLM CLI missing pipes")?;
        bail!("missing VLM CLI stdout/stderr pipe");
    };
    let image_raw = image.to_string_lossy().into_owned();
    let image_redacted = crate::platform::redact_path_for_log(image);
    let host = resources.clone();
    let mut drain = Drain { task: tokio::spawn(async move {
        let _host = host;
        let mut bytes = Vec::new();
        stderr.take(MAX_STDERR + 1).read_to_end(&mut bytes).await.context("read VLM CLI stderr")?;
        if bytes.len() as u64 > MAX_STDERR { bail!("VLM CLI stderr exceeds bound"); }
        for line in String::from_utf8_lossy(&bytes).lines() {
            tracing::debug!(target: "vlm", "{}", line.replace(&image_raw, &image_redacted));
        }
        Ok(())
    }), done: false };
    let result = tokio::select! {
        biased;
        _ = wait_cancelled(&cancel) => Err(anyhow!("cancelled")),
        result = async {
            let mut reader = BufReader::new(stdout.take(MAX_STDOUT + 1)).lines();
            let mut text = String::new(); let mut consumed = 0_u64;
            loop {
                tokio::select! {
                    biased;
                    result = &mut drain.task, if !drain.done => {
                        drain.done = true; result.context("VLM CLI stderr task")??;
                    }
                    line = reader.next_line() => match line.context("read VLM CLI stdout")? {
                        Some(line) => {
                            consumed = consumed.saturating_add(line.len() as u64).saturating_add(1);
                            if consumed > MAX_STDOUT { bail!("VLM CLI stdout exceeds bound"); }
                            on_token(&line);
                            if !text.is_empty() { text.push('\n'); } text.push_str(&line);
                        }
                        None => break,
                    },
                }
            }
            if !drain.done {
                let result = (&mut drain.task).await; drain.done = true;
                result.context("VLM CLI stderr task")??;
            }
            let status = process.wait_exit().await.context("waiting on owned VLM CLI")?;
            if !status.success() { bail!("VLM exited with status {status:?}"); }
            Ok(text.trim().to_owned())
        } => result,
    };
    let cleanup = process.shutdown().await;
    if !drain.done {
        drain.task.abort(); let joined = (&mut drain.task).await; drain.done = true;
        if let Err(error) = joined {
            if !error.is_cancelled() { return Err(anyhow!("VLM CLI stderr cleanup failed: {error}; native cleanup: {cleanup:?}")); }
        }
    }
    match cleanup {
        Ok(()) => result,
        Err(error) => match result {
            Ok(_) => Err(error.context("VLM CLI cleanup")),
            Err(original) => Err(original.context(format!("VLM CLI cleanup failed: {error:#}"))),
        },
    }
}
async fn wait_cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) { tokio::time::sleep(Duration::from_millis(50)).await; }
}

#[cfg(test)]
#[path = "vlm_cli_owned_tests.rs"]
mod tests;
