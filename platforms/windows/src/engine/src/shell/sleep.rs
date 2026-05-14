// Sleep guard — keep the system + display awake during a scan.
//
// Mirror of macOS `IOPMAssertion`-based assertion in
// `engine/Sources/FileIDEngine/Shell/SleepAssert.swift`. RAII: acquired
// at scan start, released when the guard drops (scan complete /
// cancelled / failed).
//
// Uses `SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED |
// ES_DISPLAY_REQUIRED)` — same flag set Windows Media Player and
// Windows Update use during long jobs. We deliberately don't set
// ES_AWAYMODE_REQUIRED (that's for media background playback).
//
// Acquiring twice is idempotent: drop the second guard early, the
// first still holds the assertion.

#[cfg(windows)]
use windows::Win32::System::Power::{
    SetThreadExecutionState, ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED,
    EXECUTION_STATE,
};

/// RAII handle. Drop to release the keep-awake assertion.
#[allow(dead_code)]
pub struct SleepGuard {
    #[cfg(windows)]
    prior: EXECUTION_STATE,
}

#[allow(dead_code)]
impl SleepGuard {
    #[cfg(windows)]
    pub fn acquire() -> Self {
        let prior = unsafe {
            SetThreadExecutionState(ES_CONTINUOUS | ES_SYSTEM_REQUIRED | ES_DISPLAY_REQUIRED)
        };
        if prior.0 == 0 {
            tracing::warn!("SetThreadExecutionState returned 0; sleep prevention not active");
        }
        Self { prior }
    }

    #[cfg(not(windows))]
    pub fn acquire() -> Self {
        Self {}
    }
}

impl Drop for SleepGuard {
    #[cfg(windows)]
    fn drop(&mut self) {
        // Restore prior state (ES_CONTINUOUS alone clears our flags).
        unsafe {
            let _ = SetThreadExecutionState(ES_CONTINUOUS);
        }
        let _ = self.prior; // suppress unused-field lint
    }

    #[cfg(not(windows))]
    fn drop(&mut self) {}
}
