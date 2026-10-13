use anyhow::{anyhow, Result};
#[cfg(windows)]
use anyhow::Context;
use parking_lot::Mutex;
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::process::{Child, Command};
use crate::resource_scheduler::Lease;

pub(crate) fn configure(command: &mut Command) {
    command.kill_on_drop(true);
    #[cfg(target_os = "linux")]
    unsafe {
        let parent = std::process::id();
        command.pre_exec(move || install_parent_death(parent));
    }
}
#[cfg(target_os = "linux")]
fn install_parent_death(parent: u32) -> std::io::Result<()> {
    unsafe {
        if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
            return Err(std::io::Error::last_os_error());
        }
        if libc::getppid() as u32 != parent {
            return Err(std::io::Error::other("native runtime parent exited"));
        }
    }
    Ok(())
}

#[derive(Clone, Default)]
struct ExitState {
    exit: Option<std::process::ExitStatus>,
    failure: Option<String>,
}
#[derive(Default)]
struct Resources {
    _resident: Option<Arc<Lease>>,
    work: Option<Arc<Lease>>,
    reaped: bool,
}
#[derive(Clone)]
struct ProcessHandle {
    stop: tokio::sync::watch::Sender<bool>,
    state: tokio::sync::watch::Receiver<ExitState>,
    resources: Arc<Mutex<Resources>>,
}
impl ProcessHandle {
    fn request_stop(&self) { self.stop.send_replace(true); }
    async fn wait(&self, bound: Duration) -> Result<()> {
        let mut state = self.state.clone();
        let wait = async {
            loop {
                let current = state.borrow().clone();
                if let Some(error) = current.failure {
                    return Err(anyhow!("Native cleanup failed (reaped={}): {error}", current.exit.is_some()));
                }
                if current.exit.is_some() { return Ok(()); }
                state.changed().await.map_err(|_| anyhow!("Native cleanup ended without confirmed reaping"))?;
            }
        };
        tokio::time::timeout(bound, wait).await
            .map_err(|_| anyhow!("Native cleanup timed out; exit unconfirmed, ownership/resources retained"))?
    }
}
#[derive(Default)]
struct ProcessRegistry { handles: Vec<ProcessHandle>, closed: bool }
#[derive(Default)]
pub(crate) struct ProcessTracker(Arc<Mutex<ProcessRegistry>>);
impl ProcessTracker {
    pub(crate) fn shared() -> &'static Self {
        static SHARED: std::sync::OnceLock<ProcessTracker> = std::sync::OnceLock::new();
        SHARED.get_or_init(Self::default)
    }
    #[cfg(test)]
    pub(crate) fn has_unreaped(&self) -> bool { self.0.lock().handles.iter().any(|handle| handle.state.borrow().exit.is_none()) }
    pub(crate) fn allows_fallback(&self) -> bool {
        let registry = self.0.lock();
        !registry.closed && registry.handles.iter().all(|handle| {
            let state = handle.state.borrow();
            state.exit.is_some() && state.failure.is_none()
        })
    }
    pub(crate) fn allows_discovery(&self) -> bool {
        let registry = self.0.lock();
        !registry.closed && registry.handles.iter().all(|handle| {
            let state = handle.state.borrow();
            state.failure.is_none() && (state.exit.is_some() || !*handle.stop.borrow())
        })
    }
    pub(crate) async fn shutdown(&self) -> Result<()> {
        let handles = { let mut registry = self.0.lock(); registry.closed = true; registry.handles.clone() };
        for handle in &handles { handle.request_stop(); }
        let mut errors = Vec::new();
        for handle in handles {
            if let Err(error) = handle.wait(Duration::from_secs(6)).await { errors.push(error.to_string()); }
        }
        self.0.lock().handles.retain(|handle| handle.state.borrow().exit.is_none());
        if !errors.is_empty() { return Err(anyhow!(errors.join("; "))); }
        Ok(())
    }
}

trait NativeChild: Send + 'static {
    fn start_kill(&mut self) -> std::io::Result<()>;
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = std::io::Result<std::process::ExitStatus>> + Send + '_>>;
}
impl NativeChild for Child {
    fn start_kill(&mut self) -> std::io::Result<()> { Child::start_kill(self) }
    fn wait(&mut self) -> Pin<Box<dyn Future<Output = std::io::Result<std::process::ExitStatus>> + Send + '_>> {
        Box::pin(Child::wait(self))
    }
}
pub(crate) struct OwnedProcess { handle: ProcessHandle }
impl OwnedProcess {
    pub(crate) fn spawn(command: &mut Command, resources: Option<Arc<Lease>>, tracker: &ProcessTracker) -> Result<Self> {
        let (owned, _, _) = Self::spawn_piped(command, resources, tracker)?;
        Ok(owned)
    }
    pub(crate) fn spawn_piped(command: &mut Command, resources: Option<Arc<Lease>>, tracker: &ProcessTracker)
        -> Result<(Self, Option<tokio::process::ChildStdout>, Option<tokio::process::ChildStderr>)> {
        let mut registry = tracker.0.lock();
        anyhow::ensure!(!registry.closed, "Native runtime spawning closed at shutdown");
        anyhow::ensure!(!registry.handles.iter().any(|handle| {
            let state = handle.state.borrow();
            state.failure.is_some() || (*handle.stop.borrow() && state.exit.is_none())
        }), "Native runtime spawning blocked by unconfirmed or failed cleanup");
        configure(command);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (ownership, failure) = match ParentOwnership::new(&child) {
            Ok(owned) => (Some(owned), None),
            Err(error) => (None, Some(format!("Native parent ownership: {error}"))),
        };
        Ok((Self::own(child, resources, tracker.0.clone(), &mut registry.handles, ownership, failure, Duration::from_secs(5)), stdout, stderr))
    }
    fn own(mut child: impl NativeChild, resources: Option<Arc<Lease>>, registrations: Arc<Mutex<ProcessRegistry>>,
        handles: &mut Vec<ProcessHandle>, ownership: Option<ParentOwnership>, failure: Option<String>, wait_bound: Duration) -> Self {
        let (stop, mut receive) = tokio::sync::watch::channel(failure.is_some());
        let (status, state) = tokio::sync::watch::channel(ExitState { exit: None, failure });
        let resources = Arc::new(Mutex::new(Resources { _resident: resources, work: None, reaped: false }));
        let handle = ProcessHandle { stop, state, resources: resources.clone() };
        handles.push(handle.clone());
        tokio::spawn(async move {
            #[cfg(windows)]
            let mut ownership = ownership;
            #[cfg(not(windows))]
            let _ = ownership;
            let mut stopping = *receive.borrow();
            loop {
                if stopping {
                    if let Err(error) = child.start_kill() {
                        status.send_modify(|s| s.failure = Some(format!("kill failed: {error}")));
                    }
                }
                let outcome = if stopping {
                    match tokio::time::timeout(wait_bound, child.wait()).await {
                        Ok(result) => result.map_err(|error| format!("wait failed: {error}")),
                        Err(_) => Err("wait timed out; exit unconfirmed".into()),
                    }
                } else {
                    tokio::select! {
                        result = child.wait() => result.map_err(|error| format!("wait failed: {error}")),
                        _ = receive.changed() => { stopping = true; continue; },
                    }
                };
                match outcome {
                    Ok(exit) => {
                        // Only confirmed wait releases either startup or inference admission.
                        #[cfg(windows)]
                        drop(ownership.take());
                        *resources.lock() = Resources { _resident: None, work: None, reaped: true };
                        status.send_modify(|s| s.exit = Some(exit));
                        let clean_exit = status.borrow().failure.is_none();
                        if clean_exit {
                            registrations.lock().handles.retain(|registered| {
                                !Arc::ptr_eq(&registered.resources, &resources)
                            });
                        }
                        break;
                    }
                    Err(error) => {
                        tracing::error!(%error, "Native cleanup unconfirmed; retaining ownership/resources and retrying");
                        status.send_modify(|s| s.failure = Some(error));
                        stopping = true;
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        });
        Self { handle }
    }
    pub(crate) fn poll_exit(&self) -> Result<Option<std::process::ExitStatus>> {
        let state = self.handle.state.borrow();
        if let Some(error) = &state.failure { return Err(anyhow!(error.clone())); }
        Ok(state.exit)
    }
    pub(crate) fn retain_work(&self, lease: Arc<Lease>) -> Result<()> {
        let failed = self.handle.state.borrow().failure.is_some();
        let mut resources = self.handle.resources.lock();
        if resources.reaped || failed || *self.handle.stop.borrow() || resources.work.is_some() {
            return Err(anyhow!("Native process reaped, cleanup pending, or work already owned"));
        }
        resources.work = Some(lease);
        Ok(())
    }
    pub(crate) fn reusable_idle(&self) -> bool {
        let healthy = { let state = self.handle.state.borrow(); state.failure.is_none() && state.exit.is_none() };
        let stopping = *self.handle.stop.borrow(); let resources = self.handle.resources.lock();
        healthy && !stopping && !resources.reaped && resources.work.is_none()
    }
    pub(crate) fn request_stop(&self) { self.handle.request_stop(); }
    pub(crate) fn release_work(&self) { self.handle.resources.lock().work = None; }
    pub(crate) async fn wait_exit(&self) -> Result<std::process::ExitStatus> {
        self.handle.wait(Duration::from_secs(5)).await?;
        self.handle.state.borrow().exit.ok_or_else(|| anyhow!("Native exit unconfirmed"))
    }
    pub(crate) async fn stop_and_wait(&self) -> Result<()> {
        self.handle.request_stop();
        self.handle.wait(Duration::from_secs(6)).await
    }
    pub(crate) async fn shutdown(self) -> Result<()> { self.stop_and_wait().await }
}
impl Drop for OwnedProcess {
    fn drop(&mut self) { self.handle.request_stop(); }
}

#[cfg(not(windows))]
pub(crate) struct ParentOwnership;
#[cfg(not(windows))]
impl ParentOwnership {
    pub(crate) fn new(_child: &Child) -> Result<Self> { Ok(Self) }
}

#[cfg(windows)]
pub(crate) struct ParentOwnership(isize);
#[cfg(windows)]
unsafe impl Send for ParentOwnership {}
#[cfg(windows)]
impl ParentOwnership {
    pub(crate) fn new(child: &Child) -> Result<Self> {
        Self::from_handle(child.raw_handle().context("native child handle")? as isize)
    }
    fn from_handle(process: isize) -> Result<Self> {
        #[repr(C)]
        #[derive(Default)]
        struct BasicLimits {
            process_time: i64, job_time: i64, flags: u32, minimum: usize,
            maximum: usize, active: u32, affinity: usize, priority: u32, scheduling: u32,
        }
        #[repr(C)]
        #[derive(Default)]
        struct ExtendedLimits {
            basic: BasicLimits, io: [u64; 6], process_memory: usize,
            job_memory: usize, peak_process: usize, peak_job: usize,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn CreateJobObjectW(attributes: *const std::ffi::c_void, name: *const u16) -> isize;
            fn SetInformationJobObject(job: isize, class: i32, info: *const std::ffi::c_void, size: u32) -> i32;
            fn AssignProcessToJobObject(job: isize, process: isize) -> i32;
        }
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            anyhow::ensure!(job != 0, "CreateJobObjectW: {}", std::io::Error::last_os_error());
            let owned = Self(job);
            let mut limits = ExtendedLimits::default();
            limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE owns abrupt parent exit.
            anyhow::ensure!(SetInformationJobObject(job, 9, &limits as *const _ as *const _, std::mem::size_of_val(&limits) as u32) != 0, "SetInformationJobObject: {}", std::io::Error::last_os_error());
            anyhow::ensure!(AssignProcessToJobObject(job, process) != 0, "AssignProcessToJobObject: {}", std::io::Error::last_os_error());
            Ok(owned)
        }
    }
}
#[cfg(windows)]
impl Drop for ParentOwnership {
    fn drop(&mut self) {
        #[link(name = "kernel32")]
        unsafe extern "system" { fn CloseHandle(handle: isize) -> i32; }
        unsafe { CloseHandle(self.0); }
    }
}


#[cfg(test)]
#[path = "native_owner_integration_tests.rs"]
mod integration_tests;

#[cfg(test)]
#[path = "native_request_accounting_tests.rs"]
mod request_accounting_tests;

#[cfg(test)]
#[path = "fallback_guard_tests.rs"]
mod fallback_guard_tests;

#[cfg(test)]
#[path = "batch_terminal_cleanup_tests.rs"]
mod batch_terminal_cleanup_tests;
