/// Starts GTK for a widget test, or says why this platform cannot and returns
/// false so the test can end as a skip. GTK on macOS initialises only on the
/// process's main thread, and the test harness runs tests on other threads.
pub fn init() -> bool {
    if !cfg!(target_os = "linux") {
        eprintln!("skipped: widget tests need a display and run on Linux (Xvfb)");
        return false;
    }
    adw::init().unwrap();
    true
}
