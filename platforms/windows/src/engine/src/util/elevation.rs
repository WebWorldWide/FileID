//! Process elevation detection (Windows). The USN journal read on system
//! volumes requires Administrator; this gates the (Phase 3) USN path so the
//! scan driver can fall back to the always-allowed jwalk walk when the
//! process isn't elevated.
#![allow(dead_code)] // consumed by the Phase 3 USN sub-step (foundation only here).

/// True when the current process token is elevated (the user clicked through
/// UAC, or the engine was launched from an elevated shell). False on standard-
/// user processes and on non-Windows builds.
#[cfg(windows)]
pub(crate) fn is_elevated() -> bool {
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    unsafe {
        let mut token = HANDLE::default();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).is_err() {
            return false;
        }
        let mut elev = TOKEN_ELEVATION::default();
        let mut ret_len: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            Some((&mut elev as *mut TOKEN_ELEVATION).cast()),
            u32::try_from(std::mem::size_of::<TOKEN_ELEVATION>()).unwrap_or(0),
            &mut ret_len,
        );
        let _ = CloseHandle(token);
        ok.is_ok() && elev.TokenIsElevated != 0
    }
}

#[cfg(not(windows))]
pub(crate) fn is_elevated() -> bool {
    // POSIX equivalent (`libc::geteuid() == 0`) is deferred until the Linux port.
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The call is a clean primitive — never panics. Whether it returns
    /// true or false depends on how the test runner was launched; both are
    /// valid, so we just assert no panic and a real bool.
    #[test]
    fn is_elevated_does_not_panic() {
        let _ = is_elevated();
    }
}
