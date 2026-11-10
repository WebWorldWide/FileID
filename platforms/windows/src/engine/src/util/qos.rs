//! Interactive requests (search, chat, the catalog actions behind a click) run at
//! the user-initiated QoS on macOS, as the Swift engine's database pool and IPC
//! writer do. Threads left at the default QoS lose the CPU to busier processes and
//! answer several times slower on a loaded Mac. A no-op on other platforms.

/// Runs `work` on the calling thread at the user-initiated QoS, then restores the
/// thread's previous class (blocking-pool threads are reused for other work).
pub fn interactive<R>(work: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "macos")]
    {
        use libc::qos_class_t::{QOS_CLASS_DEFAULT, QOS_CLASS_UNSPECIFIED, QOS_CLASS_USER_INITIATED};
        let mut previous = QOS_CLASS_UNSPECIFIED;
        let mut priority = 0;
        // SAFETY: both calls only read or set the calling thread's own QoS.
        unsafe {
            libc::pthread_get_qos_class_np(libc::pthread_self(), &mut previous, &mut priority);
            libc::pthread_set_qos_class_self_np(QOS_CLASS_USER_INITIATED, 0);
        }
        let result = work();
        let (restore, priority) = if matches!(previous, QOS_CLASS_UNSPECIFIED) { (QOS_CLASS_DEFAULT, 0) } else { (previous, priority) };
        // SAFETY: as above.
        unsafe {
            libc::pthread_set_qos_class_self_np(restore, priority);
        }
        result
    }
    #[cfg(not(target_os = "macos"))]
    work()
}

#[cfg(test)]
mod tests {
    #[test]
    fn interactive_returns_the_work_result_and_restores_the_thread() {
        assert_eq!(super::interactive(|| 7), 7);
        #[cfg(target_os = "macos")]
        {
            let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
            let mut priority = 0;
            let inside = super::interactive(|| {
                // SAFETY: reads the calling thread's own QoS.
                unsafe { libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut priority) };
                class
            });
            assert!(matches!(inside, libc::qos_class_t::QOS_CLASS_USER_INITIATED));
            // SAFETY: as above.
            unsafe { libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut priority) };
            assert!(!matches!(class, libc::qos_class_t::QOS_CLASS_USER_INITIATED));
        }
    }
}
