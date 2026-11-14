//! The scan's model stack, loaded when a scan first has a file to analyse and
//! kept between scans. A scan that finds nothing new loads nothing; a second
//! scan shortly after the first reuses the loaded stack; an idle stack is
//! freed after `PARK_AFTER`, or at once when another model needs the memory.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;

use crate::models::idle::IdleSlot;
use crate::pipeline::tagging::ModelStack;

/// How long a finished scan's models stay loaded for the next scan. Matches
/// the resident Deep Analyze model's idle period.
pub const PARK_AFTER: Duration = Duration::from_secs(120);

/// Why the scan's models could not be loaded; carried out of the scan as its
/// error so the caller can report it under the right kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelLoadFailure {
    pub kind: &'static str,
    pub message: String,
}

impl ModelLoadFailure {
    pub fn new(kind: &'static str, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl std::fmt::Display for ModelLoadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)
    }
}

impl std::error::Error for ModelLoadFailure {}

pub type LoadFuture = Pin<Box<dyn Future<Output = Result<Arc<ModelStack>, ModelLoadFailure>> + Send>>;
type Loader = Box<dyn FnOnce() -> LoadFuture + Send>;

/// The models of one scan: resolved at most once, on first use.
pub struct ModelSource {
    cell: tokio::sync::OnceCell<Option<Arc<ModelStack>>>,
    loader: Mutex<Option<Loader>>,
    failure: Mutex<Option<ModelLoadFailure>>,
}

impl ModelSource {
    pub fn ready(stack: Arc<ModelStack>) -> Arc<Self> {
        Arc::new(Self {
            cell: tokio::sync::OnceCell::new_with(Some(Some(stack))),
            loader: Mutex::new(None),
            failure: Mutex::new(None),
        })
    }

    pub fn lazy(loader: impl FnOnce() -> LoadFuture + Send + 'static) -> Arc<Self> {
        Arc::new(Self {
            cell: tokio::sync::OnceCell::new(),
            loader: Mutex::new(Some(Box::new(loader))),
            failure: Mutex::new(None),
        })
    }

    /// The stack, loading it on the first call. `None` when loading failed;
    /// `failure()` then says why.
    pub async fn get(&self) -> Option<Arc<ModelStack>> {
        self.cell
            .get_or_init(|| async {
                let loader = self.loader.lock().take();
                let result = match loader {
                    Some(load) => load().await,
                    None => Err(ModelLoadFailure::new(
                        "model_load_failed",
                        "FileID's AI models stopped loading before they were ready. Scan again.",
                    )),
                };
                match result {
                    Ok(stack) => Some(stack),
                    Err(failure) => {
                        *self.failure.lock() = Some(failure);
                        None
                    }
                }
            })
            .await
            .clone()
    }

    /// The stack if this scan has loaded it.
    pub fn loaded(&self) -> Option<Arc<ModelStack>> {
        self.cell.get().cloned().flatten()
    }

    pub fn failure(&self) -> Option<ModelLoadFailure> {
        self.failure.lock().clone()
    }
}

/// Everything a loaded stack depends on. A resident stack is reused only
/// while this is unchanged, so installing or repairing a model, switching the
/// graphics provider or changing a tuning variable loads a fresh one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackKey {
    worker_count: usize,
    runtime_available: bool,
    provider: String,
    apple_scan: bool,
    tuning: Vec<Option<String>>,
    files: Vec<(PathBuf, Option<(u64, Option<SystemTime>)>)>,
}

impl StackKey {
    pub fn current(worker_count: usize, runtime_available: bool) -> Self {
        let paths = [
            crate::models::sface::default_weights_path(),
            crate::models::yunet::default_weights_path(),
            crate::models::mobileclip::default_weights_path(),
            crate::models::ram_plus::default_onnx_path(),
            crate::models::ram_plus::default_tags_path(),
            crate::models::bge_text::default_weights_path(),
            crate::models::bge_text::default_vocab_path(),
        ];
        Self::of(
            worker_count,
            runtime_available,
            crate::models::runtime::armed_provider().as_str(),
            crate::pipeline::tagging::apple_scan_enabled(),
            paths.into_iter().flatten().collect(),
        )
    }

    fn of(worker_count: usize, runtime_available: bool, provider: &str, apple_scan: bool, paths: Vec<PathBuf>) -> Self {
        let tuning = ["FILEID_MODEL_POOL_SIZE", "FILEID_CLIP_USE_BATCH", "FILEID_RAMPLUS_BATCH_SIZE"]
            .into_iter()
            .map(|name| std::env::var(name).ok())
            .collect();
        let files = paths
            .into_iter()
            .map(|path| {
                let stat = std::fs::metadata(&path).ok().map(|m| (m.len(), m.modified().ok()));
                (path, stat)
            })
            .collect();
        Self { worker_count, runtime_available, provider: provider.to_string(), apple_scan, tuning, files }
    }
}

static RESIDENT: IdleSlot<(StackKey, Arc<ModelStack>)> = IdleSlot::new("scan models", PARK_AFTER);
static EVICTIONS: AtomicU64 = AtomicU64::new(0);

/// The stack a previous scan left loaded, if it still fits `key`. A stack that
/// no longer fits is freed here, before its replacement is loaded.
pub fn resident(key: &StackKey) -> Option<Arc<ModelStack>> {
    resident_in(&RESIDENT, key)
}

/// Keep `stack` loaded for the next scan and restart its idle period.
pub fn keep(key: StackKey, stack: Arc<ModelStack>) {
    RESIDENT.put((key, stack));
}

/// Counts `release_for_other_models` calls; a scan that saw one while it ran
/// does not put its stack back.
pub fn evictions() -> u64 {
    EVICTIONS.load(Ordering::Acquire)
}

/// Free the idle scan models now: another model (Deep Analyze, Chat) is about
/// to load and needs the memory more.
pub fn release_for_other_models() -> bool {
    EVICTIONS.fetch_add(1, Ordering::AcqRel);
    let freed = RESIDENT.clear();
    if freed {
        tracing::info!("[SCAN] idle scan models freed for another model");
    }
    freed
}

fn resident_in(slot: &'static IdleSlot<(StackKey, Arc<ModelStack>)>, key: &StackKey) -> Option<Arc<ModelStack>> {
    match slot.peek(|(held, stack)| (held == key).then(|| stack.clone())) {
        Some(Some(stack)) => Some(stack),
        Some(None) => {
            slot.clear();
            None
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn key(dir: &std::path::Path, provider: &str) -> StackKey {
        StackKey::of(4, true, provider, false, vec![dir.join("clip.onnx"), dir.join("faces.onnx")])
    }

    #[test]
    fn the_key_changes_when_a_model_file_is_installed_or_replaced() {
        let dir = std::env::temp_dir().join(format!("fileid-stack-key-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("clip.onnx"), b"weights").unwrap();
        let before = key(&dir, "cpu");
        assert_eq!(before, key(&dir, "cpu"));
        assert_ne!(before, key(&dir, "directml"));
        assert_ne!(before, StackKey::of(2, true, "cpu", false, vec![dir.join("clip.onnx"), dir.join("faces.onnx")]));
        std::fs::write(dir.join("faces.onnx"), b"installed later").unwrap();
        let installed = key(&dir, "cpu");
        assert_ne!(before, installed);
        std::fs::write(dir.join("clip.onnx"), b"repaired weights").unwrap();
        assert_ne!(installed, key(&dir, "cpu"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_resident_stack_is_reused_for_the_same_key_and_freed_for_another() {
        static SLOT: IdleSlot<(StackKey, Arc<ModelStack>)> = IdleSlot::new("test scan models", Duration::from_secs(60));
        let dir = std::env::temp_dir().join("fileid-stack-resident-missing");
        let (cpu, gpu) = (key(&dir, "cpu"), key(&dir, "directml"));
        assert!(resident_in(&SLOT, &cpu).is_none());
        let stack = Arc::new(ModelStack::empty());
        SLOT.put((cpu.clone(), stack.clone()));
        assert!(Arc::ptr_eq(&resident_in(&SLOT, &cpu).unwrap(), &stack));
        assert!(resident_in(&SLOT, &gpu).is_none(), "another provider needs its own sessions");
        assert!(!SLOT.is_loaded(), "the stale stack is freed before its replacement loads");
        drop(stack);

        // Idle parking frees the stack; the next scan loads again.
        SLOT.put((cpu.clone(), Arc::new(ModelStack::empty())));
        let later = std::time::Instant::now() + Duration::from_secs(61);
        assert!(crate::models::idle::Park::park_idle(&SLOT, later));
        assert!(resident_in(&SLOT, &cpu).is_none());
    }

    #[tokio::test]
    async fn models_load_once_on_first_use_and_never_without_one() {
        static LOADS: AtomicUsize = AtomicUsize::new(0);
        let source = ModelSource::lazy(|| {
            Box::pin(async {
                LOADS.fetch_add(1, Ordering::SeqCst);
                Ok(Arc::new(ModelStack::empty()))
            })
        });
        assert!(source.loaded().is_none());
        assert_eq!(LOADS.load(Ordering::SeqCst), 0);
        let (a, b) = tokio::join!(source.get(), source.get());
        assert!(Arc::ptr_eq(&a.unwrap(), &b.unwrap()));
        assert_eq!(LOADS.load(Ordering::SeqCst), 1);
        assert!(source.loaded().is_some() && source.failure().is_none());
    }

    #[tokio::test]
    async fn a_failed_load_is_reported_once_and_remembered() {
        let source = ModelSource::lazy(|| Box::pin(async { Err(ModelLoadFailure::new("model_load_timeout", "too slow")) }));
        assert!(source.get().await.is_none());
        assert!(source.get().await.is_none());
        assert_eq!(source.failure(), Some(ModelLoadFailure::new("model_load_timeout", "too slow")));
        assert!(source.loaded().is_none());
    }

    #[test]
    fn releasing_for_another_model_is_counted() {
        let before = evictions();
        release_for_other_models();
        assert!(evictions() > before);
    }
}
