// ML inference models for the Windows engine.
//
// One submodule per model + the runtime probe + the registry of
// downloadable artifacts. Mirrors the macOS layer's surface; the
// implementations differ because Windows uses ONNX Runtime (CUDA /
// OpenVINO / DirectML / QNN / CPU) instead of CoreML+Vision and a
// llama.cpp subprocess instead of MLX for the VLMs.
//
// Loading is fail-soft: every model's `load(path)` returns Err if the
// weights aren't on disk or the ORT session refuses to bind. The
// pipeline (`pipeline/tagging.rs::ModelStack::load_default`) wraps each
// load in `load_optional` so a missing model degrades that single stage
// to a no-op without failing the whole scan. Inference paths similarly
// return `anyhow::Error` on any failure; callers `tracing::warn!` and
// move on.
//
// The actual ORT session bind + tensor wrangling lives inside each
// submodule. Cross-cutting concerns (EP priority, DLL-search hardening,
// pack detection) live in `runtime.rs`.

pub mod bge_text;
pub mod catalog;
pub(crate) mod resident_model;
pub mod launch_tuning;
pub mod license;
pub mod manifest;
pub mod mlx;
pub mod benchmark;
pub mod clip_text;
pub mod clip_space;
pub mod clip_tokenizer;
pub mod ep_guard;
pub mod idle;
pub mod mobileclip;
pub mod ram_plus;
pub mod ram_plus_batch;
pub mod registry;
pub mod required;
pub mod runtime;
pub mod scene_vocab;
pub mod search_strength;
pub mod face_align;
pub mod face_landmark_match;
pub mod face_geometry;
pub mod face_mesh;
pub mod sface;
pub mod variants;
pub mod yunet;
pub mod vlm;
pub mod vlm_server;
pub(crate) mod native_process;
pub(crate) mod native_admission;
pub(crate) mod native_probe;
pub(crate) mod native_runtime;
pub(crate) mod model_provenance;
pub(crate) mod model_job;
pub(crate) mod runtime_cache;
pub(crate) mod resident_service;
pub mod wordpiece_tokenizer;

pub use clip_tokenizer::ClipTokenizer;

/// Check a session out of a pool: the first one nobody is using, starting at
/// `preferred`; only when every session is busy does the caller wait, on its
/// preferred one. Indexing by worker number alone let two workers queue on
/// one session while another sat idle.
pub fn checkout<T>(pool: &[parking_lot::Mutex<T>], preferred: usize) -> parking_lot::MutexGuard<'_, T> {
    let start = preferred % pool.len();
    (0..pool.len())
        .find_map(|offset| pool[(start + offset) % pool.len()].try_lock())
        .unwrap_or_else(|| pool[start].lock())
}

#[cfg(test)]
mod pool_tests {
    use super::checkout;
    use parking_lot::Mutex;

    #[test]
    fn checkout_takes_an_idle_session_before_waiting_on_a_busy_one() {
        let pool: Vec<Mutex<usize>> = (0..3).map(Mutex::new).collect();
        assert_eq!(*checkout(&pool, 1), 1, "the preferred session when it is free");
        assert_eq!(*checkout(&pool, 7), 1, "the preference wraps around the pool");
        let busy = pool[1].lock();
        assert_eq!(*checkout(&pool, 1), 2, "the next idle session instead of queueing");
        let also_busy = pool[2].lock();
        assert_eq!(*checkout(&pool, 1), 0);
        let first = checkout(&pool, 1);
        assert!(pool.iter().all(|session| session.try_lock().is_none()), "three holders, three sessions");
        drop((busy, also_busy, first));

        // Every session busy: the caller waits for its preferred one.
        let held: Vec<_> = pool.iter().map(Mutex::lock).collect();
        std::thread::scope(|scope| {
            let waiter = scope.spawn(|| *checkout(&pool, 2));
            std::thread::sleep(std::time::Duration::from_millis(30));
            assert!(!waiter.is_finished());
            drop(held);
            assert_eq!(waiter.join().unwrap(), 2);
        });
    }
}
pub use native_runtime::llama_server_available;
