//! Files a busy machine timed out are read again once the machine is idle, so a library settles without a manual Rescan.
//! Each pass of a scan records one failed attempt per file (`catch_up::FAILURE_RETRY_LIMIT`), which bounds how often this runs.

use std::collections::HashSet;
use std::sync::OnceLock;
use std::time::Duration;

use parking_lot::Mutex;

pub(crate) const IDLE_POLL: Duration = Duration::from_secs(5);
pub(crate) const IDLE_ATTEMPTS: usize = 120;

pub(crate) fn owed(conn: &rusqlite::Connection, root: &std::path::Path) -> bool {
    crate::scan_session::count_retryable_timeouts(conn, root, 0.0) > 0
}

fn waiting() -> &'static Mutex<HashSet<String>> {
    static WAITING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    WAITING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// One waiting retry per folder; dropping it lets the folder wait again.
pub(crate) struct Claim(String);

impl Drop for Claim {
    fn drop(&mut self) {
        waiting().lock().remove(&self.0);
    }
}

pub(crate) fn claim(root: &str) -> Option<Claim> {
    waiting().lock().insert(root.to_owned()).then(|| Claim(root.to_owned()))
}

/// True once `idle` says so, checked up to `attempts` times `poll` apart.
pub(crate) async fn wait_until_idle(poll: Duration, attempts: usize, idle: impl Fn() -> bool) -> bool {
    for _ in 0..attempts {
        if idle() {
            return true;
        }
        tokio::time::sleep(poll).await;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn a_folder_waits_once_until_its_claim_is_dropped() {
        let first = claim("/lib/timeout-retry-claim").expect("first claim");
        assert!(claim("/lib/timeout-retry-claim").is_none());
        assert!(claim("/lib/other-timeout-retry-claim").is_some());
        drop(first);
        assert!(claim("/lib/timeout-retry-claim").is_some());
    }

    #[test]
    fn retry_is_owed_only_for_timed_out_rows_with_attempts_left_under_the_folder() {
        let conn = crate::db::test_support::memory_db();
        let sep = std::path::MAIN_SEPARATOR;
        let root = std::path::PathBuf::from(format!("{sep}lib"));
        assert!(!owed(&conn, &root));
        crate::db::test_support::insert_file(&conn, 1, &format!("{sep}lib{sep}slow.png"), "image");
        conn.execute(
            "UPDATE files SET failed = 1, error_message = ?1, scanned_at = 100.0 WHERE id = 1",
            [crate::pipeline::tagging::TIMED_OUT_MESSAGE],
        )
        .unwrap();
        assert!(owed(&conn, &root), "an earlier scan's timeout is still owed a retry");
        assert!(!owed(&conn, std::path::Path::new(&format!("{sep}elsewhere"))));
        conn.execute(
            "INSERT INTO scan_failures (file_id, size_bytes, modified_at, engine_build, attempts, failed_at) VALUES (1, 1, 1, 'b', 3, 0)",
            [],
        )
        .unwrap();
        assert!(!owed(&conn, &root), "three attempts end the automatic retries");
    }

    #[tokio::test]
    async fn waiting_ends_when_the_machine_goes_idle_or_the_attempts_run_out() {
        let checks = AtomicUsize::new(0);
        assert!(wait_until_idle(Duration::from_millis(1), 10, || checks.fetch_add(1, Ordering::SeqCst) == 3).await);
        assert_eq!(checks.load(Ordering::SeqCst), 4);
        assert!(!wait_until_idle(Duration::from_millis(1), 3, || false).await);
    }
}
