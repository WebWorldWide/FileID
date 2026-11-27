//! Interactive requests run at the user-initiated QoS on macOS: default-QoS threads lose the CPU to busier processes
//! and answer several times slower on a loaded Mac. No-op elsewhere.

/// Restores the thread's previous class afterwards (blocking-pool threads are reused).
pub fn interactive<R>(work: impl FnOnce() -> R) -> R {
    #[cfg(target_os = "macos")]
    {
        use libc::qos_class_t::{QOS_CLASS_DEFAULT, QOS_CLASS_UNSPECIFIED, QOS_CLASS_USER_INITIATED};
        struct Restore(libc::qos_class_t, i32);
        impl Drop for Restore {
            fn drop(&mut self) {
                // SAFETY: sets the calling thread's own QoS; runs on unwind too, so a panic in `work` cannot leave a reused pool thread elevated.
                unsafe {
                    libc::pthread_set_qos_class_self_np(self.0, self.1);
                }
            }
        }
        let mut previous = QOS_CLASS_UNSPECIFIED;
        let mut priority = 0;
        // SAFETY: both calls only read or set the calling thread's own QoS.
        unsafe {
            libc::pthread_get_qos_class_np(libc::pthread_self(), &mut previous, &mut priority);
            libc::pthread_set_qos_class_self_np(QOS_CLASS_USER_INITIATED, 0);
        }
        let _restore = if matches!(previous, QOS_CLASS_UNSPECIFIED) { Restore(QOS_CLASS_DEFAULT, 0) } else { Restore(previous, priority) };
        work()
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

    #[test]
    #[cfg(target_os = "macos")]
    fn a_panic_inside_the_work_still_restores_the_thread() {
        std::thread::spawn(|| {
            let outcome = std::panic::catch_unwind(|| super::interactive(|| panic!("boom")));
            assert!(outcome.is_err());
            let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
            let mut priority = 0;
            // SAFETY: reads the calling thread's own QoS.
            unsafe { libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut priority) };
            assert!(!matches!(class, libc::qos_class_t::QOS_CLASS_USER_INITIATED));
        })
        .join()
        .unwrap();
    }
}
