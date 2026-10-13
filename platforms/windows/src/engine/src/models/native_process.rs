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
pub(crate) fn configure_sync(command: &mut std::process::Command) {
    #[cfg(not(target_os = "linux"))]
    let _ = command;
    #[cfg(target_os = "linux")]
    unsafe {
        use std::os::unix::process::CommandExt;
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
pub(crate) struct ProcessTracker(Arc<Mutex<Vec<ProcessHandle>>>);
impl ProcessTracker {
    pub(crate) fn has_unreaped(&self) -> bool { self.0.lock().iter().any(|handle| handle.state.borrow().exit.is_none()) }
    pub(crate) async fn shutdown(&self) -> Result<()> {
        let handles = self.0.lock().clone();
        for handle in &handles { handle.request_stop(); }
        let mut errors = Vec::new();
        for handle in handles {
            if let Err(error) = handle.wait(Duration::from_secs(6)).await { errors.push(error.to_string()); }
        }
        self.0.lock().retain(|handle| handle.state.borrow().exit.is_none());
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
    pub(crate) fn new(child: Child, resources: Option<Arc<Lease>>, tracker: &ProcessTracker) -> Self {
        let ownership = ParentOwnership::new(&child);
        let (ownership, failure) = match ownership {
            Ok(owned) => (Some(owned), None),
            Err(error) => (None, Some(format!("Native parent ownership: {error}"))),
        };
        Self::own(child, resources, tracker, ownership, failure, Duration::from_secs(5))
    }
    fn own(mut child: impl NativeChild, resources: Option<Arc<Lease>>, tracker: &ProcessTracker,
        ownership: Option<ParentOwnership>, failure: Option<String>, wait_bound: Duration) -> Self {
        let (stop, mut receive) = tokio::sync::watch::channel(failure.is_some());
        let (status, state) = tokio::sync::watch::channel(ExitState { exit: None, failure });
        let resources = Arc::new(Mutex::new(Resources { _resident: resources, work: None, reaped: false }));
        let handle = ProcessHandle { stop, state, resources: resources.clone() };
        tracker.0.lock().push(handle.clone());
        let registrations = tracker.0.clone();
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
                        *resources.lock() = Resources { reaped: true, ..Resources::default() };
                        status.send_modify(|s| s.exit = Some(exit));
                        let clean_exit = status.borrow().failure.is_none();
                        if clean_exit {
                            registrations.lock().retain(|registered| {
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
        let mut resources = self.handle.resources.lock();
        if resources.reaped { return Err(anyhow!("Native process already reaped")); }
        resources.work = Some(lease);
        Ok(())
    }
    pub(crate) fn release_work(&self) { self.handle.resources.lock().work = None; }
    pub(crate) async fn wait_exit(&self) -> Result<std::process::ExitStatus> {
        self.handle.wait(Duration::from_secs(5)).await?;
        self.handle.state.borrow().exit.ok_or_else(|| anyhow!("Native exit unconfirmed"))
    }
    pub(crate) async fn shutdown(self) -> Result<()> {
        self.handle.request_stop();
        self.handle.wait(Duration::from_secs(6)).await
    }
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
    pub(crate) fn new_sync(child: &std::process::Child) -> Result<Self> {
        use std::os::windows::io::AsRawHandle;
        Self::from_handle(child.as_raw_handle() as isize)
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
pub(crate) fn test_child_command() -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "models::native_process::tests::owned_child_fixture", "--ignored", "--nocapture"])
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    configure(&mut command);
    command
}
#[cfg(test)]
pub(crate) fn test_child_alive(pid: u32) -> bool { tests::alive(pid) }
#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn alive(pid: u32) -> bool {
        #[cfg(windows)]
        unsafe {
            use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
            let Ok(handle) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) else { return false; };
            let running = WaitForSingleObject(handle, 0).0 == 258;
            let _ = windows::Win32::Foundation::CloseHandle(handle);
            running
        }
        #[cfg(not(windows))]
        { std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| !stat.split(')').nth(1).unwrap_or("").trim_start().starts_with('Z')) }
    }
    #[tokio::test]
    #[ignore = "internal generated child used by owned-process tests"]
    async fn owned_child_fixture() { tokio::time::sleep(Duration::from_secs(120)).await; }

    struct GeneratedChild {
        gate: tokio::sync::watch::Receiver<bool>,
        fail_wait: bool,
    }
    impl NativeChild for GeneratedChild {
        fn start_kill(&mut self) -> std::io::Result<()> { Ok(()) }
        fn wait(&mut self) -> Pin<Box<dyn Future<Output = std::io::Result<std::process::ExitStatus>> + Send + '_>> {
            Box::pin(async move {
                if std::mem::take(&mut self.fail_wait) {
                    return Err(std::io::Error::other("generated wait failure"));
                }
                while !*self.gate.borrow() {
                    self.gate.changed().await.map_err(|_| std::io::Error::other("generated gate closed"))?;
                }
                #[cfg(windows)]
                {
                    use std::os::windows::process::ExitStatusExt;
                    Ok(std::process::ExitStatus::from_raw(0))
                }
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    Ok(std::process::ExitStatus::from_raw(0))
                }
            })
        }
    }
    fn generated_admission() -> (Arc<crate::resource_scheduler::ResourceScheduler>, Arc<Lease>) {
        use crate::resource_scheduler::{AcceleratorCapacity, AcceleratorDemand, Demand, Priority, ResourceScheduler};
        let scheduler = ResourceScheduler::new_with_accelerators(
            || (16384, 12000), 1, 1, AcceleratorCapacity { gpu_mb: Some(2048), npu_mb: None },
        );
        let lease = Arc::new(scheduler.reserve_with_accelerators(
            Demand { memory_mb: 64, cpu: 1, io: 1 },
            AcceleratorDemand { gpu_mb: 32, npu_mb: 0 }, Priority::Interactive, || false,
        ).unwrap());
        (scheduler, lease)
    }
    async fn wait_for_registration_count(tracker: &ProcessTracker, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while tracker.0.lock().len() != expected { tokio::task::yield_now().await; }
        }).await.unwrap();
    }
    #[tokio::test]
    async fn generated_tracker_ordinary_confirmed_exits_retire_without_shutdown() {
        let tracker = ProcessTracker::default();
        for _ in 0..128 {
            let (scheduler, lease) = generated_admission();
            let (_release, gate) = tokio::sync::watch::channel(true);
            let process = OwnedProcess::own(
                GeneratedChild { gate, fail_wait: false }, Some(lease), &tracker, None, None,
                Duration::from_secs(1),
            );
            assert!(process.wait_exit().await.unwrap().success());
            wait_for_registration_count(&tracker, 0).await;
            assert_eq!(scheduler.snapshot().active, 0);
            assert_eq!(scheduler.snapshot().memory_mb, 0);
            assert_eq!(scheduler.snapshot().gpu_mb, 0);
            assert!(process.handle.resources.lock().reaped);
        }
    }
    #[tokio::test]
    async fn generated_tracker_unconfirmed_exit_keeps_registration_and_admission() {
        let tracker = ProcessTracker::default();
        let (scheduler, lease) = generated_admission();
        let (release, gate) = tokio::sync::watch::channel(false);
        let process = OwnedProcess::own(
            GeneratedChild { gate, fail_wait: false }, Some(lease), &tracker, None, None,
            Duration::from_secs(1),
        );
        tokio::task::yield_now().await;
        assert!(tracker.has_unreaped());
        assert_eq!(tracker.0.lock().len(), 1);
        assert_eq!(scheduler.snapshot().active, 1);
        assert_eq!(scheduler.snapshot().memory_mb, 64);
        assert_eq!(scheduler.snapshot().gpu_mb, 32);
        release.send_replace(true);
        assert!(process.wait_exit().await.unwrap().success());
        wait_for_registration_count(&tracker, 0).await;
        assert_eq!(scheduler.snapshot().active, 0);
    }
    #[tokio::test]
    async fn generated_tracker_cleanup_failure_stays_observable_after_reaping() {
        let tracker = ProcessTracker::default();
        let (scheduler, lease) = generated_admission();
        let (release, gate) = tokio::sync::watch::channel(false);
        let process = OwnedProcess::own(
            GeneratedChild { gate, fail_wait: true }, Some(lease), &tracker, None, None,
            Duration::from_secs(3),
        );
        let failure = process.handle.wait(Duration::from_secs(1)).await.unwrap_err();
        assert!(failure.to_string().contains("generated wait failure"));
        assert_eq!(tracker.0.lock().len(), 1);
        assert_eq!(scheduler.snapshot().active, 1);
        release.send_replace(true);
        tokio::time::timeout(Duration::from_secs(2), async {
            while tracker.has_unreaped() { tokio::task::yield_now().await; }
        }).await.unwrap();
        assert_eq!(scheduler.snapshot().active, 0);
        assert_eq!(tracker.0.lock().len(), 1);
        let failure = tracker.shutdown().await.unwrap_err();
        assert!(failure.to_string().contains("generated wait failure"));
        assert!(tracker.0.lock().is_empty());
    }

    struct InjectedChild {
        child: Child,
        gate: tokio::sync::watch::Receiver<bool>,
        kill_error: bool,
        wait_error: bool,
    }
    impl NativeChild for InjectedChild {
        fn start_kill(&mut self) -> std::io::Result<()> {
            if std::mem::take(&mut self.kill_error) { return Err(std::io::Error::other("injected kill failure")); }
            self.child.start_kill()
        }
        fn wait(&mut self) -> Pin<Box<dyn Future<Output = std::io::Result<std::process::ExitStatus>> + Send + '_>> {
            Box::pin(async move {
                if std::mem::take(&mut self.wait_error) { return Err(std::io::Error::other("injected wait failure")); }
                while !*self.gate.borrow() {
                    self.gate.changed().await.map_err(|_| std::io::Error::other("test gate closed"))?;
                }
                self.child.wait().await
            })
        }
    }
    async fn failure_case(kill_error: bool, wait_error: bool, join_timeout: bool) {
        use crate::resource_scheduler::{Demand, Priority, ResourceScheduler, AcceleratorCapacity, AcceleratorDemand};
        let scheduler = ResourceScheduler::new_with_accelerators(|| (16384, 12000), 1, 1, AcceleratorCapacity { gpu_mb: Some(2048), npu_mb: None });
        let lease = Arc::new(scheduler.reserve_with_accelerators(Demand { memory_mb: 1024, cpu: 1, io: 1 }, AcceleratorDemand { gpu_mb: 512, npu_mb: 0 }, Priority::Interactive, || false).unwrap());
        let tracker = ProcessTracker::default();
        let child = test_child_command().spawn().unwrap();
        let pid = child.id().unwrap();
        let ownership = ParentOwnership::new(&child).unwrap();
        let (release, gate) = tokio::sync::watch::channel(false);
        let process = OwnedProcess::own(InjectedChild { child, gate, kill_error, wait_error }, Some(lease), &tracker, Some(ownership), None,
            if join_timeout { Duration::from_secs(5) } else { Duration::from_millis(30) });
        process.handle.request_stop();
        let result = process.handle.wait(if join_timeout { Duration::from_millis(10) } else { Duration::from_secs(1) }).await;
        let error = result.unwrap_err().to_string();
        assert!(error.contains(if join_timeout { "timed out" } else if kill_error { "kill failed" } else if wait_error { "wait failed" } else { "wait timed out" }), "{error}");
        assert!(process.handle.state.borrow().exit.is_none());
        assert_eq!(scheduler.snapshot().memory_mb, 1024);
        assert_eq!(scheduler.snapshot().gpu_mb, 512);
        assert!(scheduler.reserve(Demand { cpu: 1, io: 1, ..Demand::default() }, Priority::Background, || false).is_err());
        drop(process);
        release.send_replace(true);
        tokio::time::timeout(Duration::from_secs(3), async {
            while tracker.has_unreaped() { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.unwrap();
        assert!(!alive(pid));
        assert_eq!(scheduler.snapshot().active, 0);
        // The original failure is still observable even after eventual confirmed reaping.
        if join_timeout { tracker.shutdown().await.unwrap(); }
        else { assert!(tracker.shutdown().await.is_err()); }
    }
    #[tokio::test]
    async fn injected_kill_failure_retains_resources_until_confirmed_wait() { failure_case(true, false, false).await; }
    #[tokio::test]
    async fn injected_wait_failure_retains_resources_until_confirmed_wait() { failure_case(false, true, false).await; }
    #[tokio::test]
    async fn injected_wait_timeout_retains_resources_until_confirmed_wait() { failure_case(false, false, false).await; }
    #[tokio::test]
    async fn join_timeout_does_not_detach_resource_ownership() { failure_case(false, false, true).await; }

    #[tokio::test]
    #[ignore = "internal helper used only by bounded_parent_death_ownership"]
    async fn parent_death_fixture() {
        let path = std::env::var_os("FILEID_PARENT_DEATH_FIXTURE").expect("helper fixture path");
        #[cfg(windows)]
        let mut cmd = Command::new("cmd.exe");
        #[cfg(windows)]
        cmd.args(["/C", "ping -n 120 127.0.0.1 >nul"]);
        #[cfg(not(windows))]
        let mut cmd = Command::new("sleep");
        #[cfg(not(windows))]
        cmd.arg("120");
        configure(&mut cmd);
        let child = cmd.spawn().unwrap();
        let pid = child.id().unwrap();
        let _owner = OwnedProcess::new(child, None, &ProcessTracker::default());
        std::fs::write(path, pid.to_string()).unwrap();
        tokio::time::sleep(Duration::from_secs(120)).await;
    }
    #[tokio::test]
    async fn bounded_parent_death_ownership() {
        let path = std::env::temp_dir().join(format!("fileid-parent-death-{}", uuid::Uuid::new_v4()));
        let mut helper = Command::new(std::env::current_exe().unwrap());
        helper.args(["--exact", "models::native_process::tests::parent_death_fixture", "--ignored", "--nocapture"])
            .env("FILEID_PARENT_DEATH_FIXTURE", &path)
            .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        helper.kill_on_drop(true);
        let mut helper = helper.spawn().unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let pid: u32 = loop {
            if let Ok(value) = std::fs::read_to_string(&path) { break value.parse().unwrap(); }
            assert!(std::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        assert!(alive(pid));
        helper.kill().await.unwrap();
        helper.wait().await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while alive(pid) { assert!(std::time::Instant::now() < deadline); tokio::time::sleep(Duration::from_millis(25)).await; }
        std::fs::remove_file(path).unwrap();
    }
}
