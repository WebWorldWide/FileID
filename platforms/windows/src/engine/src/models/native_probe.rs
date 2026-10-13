use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};
use anyhow::{anyhow, bail, Context, Result};
use tokio::{io::AsyncReadExt, process::Command};
use crate::resource_scheduler::{Demand, Lease, ResourceScheduler};
use super::{native_admission::{reserve_host, HostAdmissionError}, native_process::{OwnedProcess, ProcessTracker}};

const MAX_PROBE_BYTES: u64 = 256 * 1024;
pub(crate) struct ProbeOutput {
    pub(crate) status: std::process::ExitStatus,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    _host: Arc<Lease>,
}
impl std::fmt::Debug for ProbeOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ProbeOutput").field("status", &self.status)
            .field("stdout_bytes", &self.stdout.len()).field("stderr_bytes", &self.stderr.len()).finish_non_exhaustive()
    }
}
async fn read_pipe(pipe: impl tokio::io::AsyncRead + Unpin) -> Result<Vec<u8>> {
    let mut bytes = Vec::new(); pipe.take(MAX_PROBE_BYTES + 1).read_to_end(&mut bytes).await?;
    if bytes.len() as u64 > MAX_PROBE_BYTES { bail!("Native probe output exceeds bound"); }
    Ok(bytes)
}
pub(crate) async fn run_probe(
    mut command: Command, scheduler: Arc<ResourceScheduler>, cancel: Arc<AtomicBool>,
    tracker: &ProcessTracker, deadline: Duration,
) -> Result<ProbeOutput> {
    let resources = reserve_host(scheduler, Demand { memory_mb: 128, cpu: 1, io: 1 }, cancel.clone()).await?;
    if cancel.load(Ordering::Relaxed) { return Err(HostAdmissionError(anyhow!("Native probe cancelled before spawn")).into()); }
    command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let (process, stdout, stderr) = OwnedProcess::spawn_piped(&mut command, Some(resources.clone()), tracker)
        .map_err(HostAdmissionError)?;
    let result = tokio::select! {
        biased;
        _ = wait_cancelled(&cancel) => Err(anyhow!("Native probe cancelled")),
        result = tokio::time::timeout(deadline, async {
            let stdout = stdout.context("missing native probe stdout")?;
            let stderr = stderr.context("missing native probe stderr")?;
            let (stdout, stderr) = tokio::try_join!(read_pipe(stdout), read_pipe(stderr))?;
            let status = process.wait_exit().await?;
            Ok::<_, anyhow::Error>((status, stdout, stderr))
        }) => result.unwrap_or_else(|_| Err(anyhow!("Native probe timed out"))),
    };
    let cleanup = process.shutdown().await;
    let (status, stdout, stderr) = match (result, cleanup) {
        (Ok(output), Ok(())) => output,
        (Err(error), Ok(())) => return Err(HostAdmissionError(error).into()),
        (Ok(_), Err(cleanup)) => return Err(HostAdmissionError(cleanup.context("Native probe cleanup failed")).into()),
        (Err(error), Err(cleanup)) => return Err(HostAdmissionError(error.context(format!("Native probe cleanup failed: {cleanup:#}"))).into()),
    };
    Ok(ProbeOutput { status, stdout, stderr, _host: resources })
}
async fn wait_cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) { tokio::time::sleep(Duration::from_millis(25)).await; }
}

#[cfg(test)]
#[path = "native_probe_tests.rs"]
mod tests;
