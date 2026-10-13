#![allow(dead_code)]
use anyhow::{bail, Context, Result};
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

#[derive(Debug)]
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// No shell, bounded pipes, kill/reap on every exit. Admission belongs to the media caller.
pub fn run(
    command: &mut Command,
    input: &[u8],
    cancelled: &AtomicBool,
    timeout: Duration,
    stdout_cap: usize,
    stderr_cap: usize,
) -> Result<Output> {
    if cancelled.load(Ordering::Relaxed) {
        bail!("Media operation cancelled");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;
        command.pre_exec(|| {
            let parent = libc::getppid();
            if libc::setpgid(0, 0) != 0 || libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            let address_space = libc::rlimit {
                rlim_cur: 1024 * 1024 * 1024,
                rlim_max: 1024 * 1024 * 1024,
            };
            let output_size = libc::rlimit {
                rlim_cur: 512 * 1024 * 1024,
                rlim_max: 512 * 1024 * 1024,
            };
            if libc::setrlimit(libc::RLIMIT_AS, &address_space) != 0
                || libc::setrlimit(libc::RLIMIT_FSIZE, &output_size) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != parent {
                libc::_exit(125);
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    let mut child = command.spawn().context("Starting isolated media process")?;
    #[cfg(windows)]
    let job = match Job::attach(&child) {
        Ok(job) => job,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let overflow = Arc::new(AtomicBool::new(false));
    let drain = |mut pipe: Box<dyn Read + Send>, cap: usize, overflow: Arc<AtomicBool>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let keep = n.min(cap.saturating_sub(bytes.len()));
                        bytes.extend_from_slice(&buf[..keep]);
                        if keep < n {
                            overflow.store(true, Ordering::Relaxed);
                        }
                    }
                }
            }
            bytes
        })
    };
    let stdout = drain(
        Box::new(child.stdout.take().context("Missing worker stdout")?),
        stdout_cap,
        overflow.clone(),
    );
    let stderr = drain(
        Box::new(child.stderr.take().context("Missing worker stderr")?),
        stderr_cap,
        overflow.clone(),
    );
    // The worker waits for this handshake before decoding; Windows job assignment precedes it.
    let write_result = child
        .stdin
        .take()
        .context("Missing worker stdin")?
        .write_all(input);
    let deadline = Instant::now() + timeout;
    let result = loop {
        if let Err(error) = &write_result {
            break Err(anyhow::anyhow!("Worker input failed: {error}"));
        }
        if cancelled.load(Ordering::Relaxed) {
            break Err(anyhow::anyhow!("Media operation cancelled"));
        }
        if Instant::now() >= deadline {
            break Err(anyhow::anyhow!("Media operation timed out"));
        }
        if overflow.load(Ordering::Relaxed) {
            break Err(anyhow::anyhow!("Media diagnostic output exceeded cap"));
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break if status.success() {
                    Ok(())
                } else {
                    Err(anyhow::anyhow!("Media worker failed ({status})"))
                }
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => break Err(error.into()),
        }
    };
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    #[cfg(windows)]
    drop(job);
    let _ = child.kill();
    let _ = child.wait();
    let stdout = stdout
        .join()
        .map_err(|_| anyhow::anyhow!("Worker output reader failed"))?;
    let stderr = stderr
        .join()
        .map_err(|_| anyhow::anyhow!("Worker diagnostic reader failed"))?;
    if let Err(error) = result {
        if stderr.is_empty() {
            return Err(error);
        }
        bail!(
            "{error}: {}",
            String::from_utf8_lossy(&stderr)
                .chars()
                .take(800)
                .collect::<String>()
        );
    }
    if overflow.load(Ordering::Relaxed) {
        bail!("Media diagnostic output exceeded cap");
    }
    Ok(Output { stdout, stderr })
}

#[cfg(windows)]
struct Job(windows::Win32::Foundation::HANDLE);
#[cfg(windows)]
impl Job {
    fn attach(child: &std::process::Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::{
            Foundation::HANDLE,
            System::JobObjects::{
                AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
                SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            },
        };
        unsafe {
            let job = Self(CreateJobObjectW(None, None)?);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            limits.ProcessMemoryLimit = 1024 * 1024 * 1024;
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of_val(&limits) as u32,
            )?;
            AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle()))?;
            Ok(job)
        }
    }
}
#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}
