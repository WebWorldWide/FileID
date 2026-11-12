//! Stage 7: MLX vision-language models for the engine the Mac app hosts.
//!
//! The sessions are the Swift engine's `DeepAnalyze` code compiled into
//! FileIDAppleKit (`FileIDAppleVLM`), so prompts, decoding, the prefix and chat
//! prompt caches and the 5-minute idle park are the Swift engine's own. This
//! module decides what runs: it picks the model folder, checks the licence,
//! holds the model's memory in the engine's [`ResourceScheduler`] while the
//! weights are resident, and serializes users through [`MlxLease`].

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use super::ffi;
use crate::resource_scheduler::{Demand, Lease, Priority, ResourceScheduler};

/// A request the user cancelled.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Cancelled.")
    }
}

impl std::error::Error for Cancelled {}

/// Streamed output of a running request.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Token(String),
    Progress(Value),
}

enum Message {
    Event(Event),
    Done(Value),
    Failed { message: String, cancelled: bool },
}

fn message(event: u32, bytes: &[u8]) -> Message {
    match event {
        ffi::VLM_TOKEN => Message::Event(Event::Token(String::from_utf8_lossy(bytes).into_owned())),
        ffi::VLM_PROGRESS => Message::Event(Event::Progress(serde_json::from_slice(bytes).unwrap_or(Value::Null))),
        ffi::VLM_DONE => match serde_json::from_slice(bytes) {
            Ok(value) => Message::Done(value),
            Err(error) => Message::Failed { message: format!("Unreadable model reply: {error}"), cancelled: false },
        },
        ffi::VLM_FAILED => {
            let value: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
            Message::Failed {
                message: value["message"].as_str().unwrap_or("The model request failed.").to_owned(),
                cancelled: value["cancelled"].as_bool().unwrap_or(false),
            }
        }
        other => Message::Event(Event::Progress(json!({"unknownEvent": other}))),
    }
}

/// An accepted request; dropping it before its terminal event cancels it.
struct Submitted {
    handle: u64,
    finished: bool,
}

impl Drop for Submitted {
    fn drop(&mut self) {
        if !self.finished {
            // SAFETY: plain handle; unknown or finished handles are ignored.
            unsafe { ffi::fileid_apple_vlm_cancel(self.handle) };
        }
    }
}

/// Run one adapter request to its end. `cancel` is polled every 25 ms and
/// forwarded once; the adapter still reports how the request ended.
pub async fn request(body: &Value, cancel: &AtomicBool, mut on_event: impl FnMut(Event)) -> Result<Value> {
    let bytes = serde_json::to_vec(body)?;
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let forward = ffi::VlmReceiver(Box::new(move |event, bytes| {
        let _ = sender.send(message(event, bytes));
    }));
    let context = Box::into_raw(Box::new(forward)).cast::<c_void>();
    // SAFETY: `bytes` is valid for the call; `context` stays owned by the adapter
    // until its terminal callback, which frees it.
    let handle = unsafe { ffi::fileid_apple_vlm_submit(bytes.as_ptr(), bytes.len(), ffi::vlm_callback, context) };
    if handle == 0 {
        // SAFETY: a refused request never calls back, so the box is still ours.
        drop(unsafe { Box::from_raw(context.cast::<ffi::VlmReceiver>()) });
        bail!("The model adapter refused the request");
    }
    let mut submitted = Submitted { handle, finished: false };
    let mut forwarded = false;
    loop {
        let message = tokio::select! {
            message = receiver.recv() => message,
            () = tokio::time::sleep(Duration::from_millis(25)), if !forwarded => {
                if cancel.load(Ordering::Acquire) {
                    // SAFETY: plain handle of a running request.
                    unsafe { ffi::fileid_apple_vlm_cancel(handle) };
                    forwarded = true;
                }
                continue;
            }
        };
        match message {
            Some(Message::Event(event)) => on_event(event),
            Some(Message::Done(value)) => {
                submitted.finished = true;
                return Ok(value);
            }
            Some(Message::Failed { cancelled: true, .. }) => {
                submitted.finished = true;
                return Err(Cancelled.into());
            }
            Some(Message::Failed { message, .. }) => {
                submitted.finished = true;
                bail!(message);
            }
            None => {
                submitted.finished = true;
                bail!("The model adapter ended the request without a reply");
            }
        }
    }
}

/// Resident sizes and names the Swift engine uses for its built-in models
/// (`AIModelKind.ramBudgetGB` / `displayName`).
const MODELS: &[(&str, &str, f64)] = &[
    ("qwen2_5_vl_7b", "Qwen2.5-VL 7B (4-bit)", 7.0),
    ("qwen3_vl_2b", "Qwen3-VL 2B (4-bit)", 3.0),
    ("qwen3_vl_4b", "Qwen3-VL 4B (4-bit)", 5.0),
    ("qwen3_vl_8b", "Qwen3-VL 8B (4-bit)", 8.5),
    ("qwen3_5_2b", "Qwen3.5 2B (4-bit)", 3.0),
    ("gemma_3_4b", "Gemma 3 4B (QAT 4-bit)", 4.5),
    ("gemma_3_12b", "Gemma 3 12B (QAT 4-bit)", 9.0),
    ("mistral_small_3_2", "Mistral Small 3.2 24B (4-bit)", 16.0),
    ("paligemma_3b", "PaliGemma 3B (8-bit)", 4.0),
];

/// The Swift engine's Hugging Face root: `FILEID_HF_CACHE_ROOT` when absolute,
/// else `~/Documents/huggingface` (Developer ID builds).
pub fn huggingface_root() -> PathBuf {
    if let Some(root) = std::env::var_os("FILEID_HF_CACHE_ROOT").map(PathBuf::from).filter(|path| path.is_absolute()) {
        return root;
    }
    // Tests never fall back to the owner's models: loading one rewrites its
    // install markers. Without an explicit root nothing counts as installed.
    if cfg!(test) {
        return std::env::temp_dir().join(format!("fileid-test-no-hf-root-{}", std::process::id()));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    home.join("Documents").join("huggingface")
}

fn repo(kind: &str) -> Option<&'static crate::models::manifest::VlmRepo> {
    crate::models::manifest::manifest()
        .vlm_repos
        .iter()
        .find(|repo| repo.kind == kind && repo.platforms.iter().any(|platform| platform == "macos"))
}

/// Folder that holds a model's MLX weights.
pub fn model_dir(kind: &str) -> Option<PathBuf> {
    let repo = repo(kind)?;
    Some(huggingface_root().join("models").join(&repo.repo))
}

/// The Swift engine's install test (`AIModelKind.isInstalledOnDisk`).
pub fn installed(kind: &str) -> bool {
    MODELS.iter().any(|(id, _, _)| *id == kind) && model_dir(kind).is_some_and(|dir| dir.join("config.json").is_file())
}

/// Resident budget the scheduler holds while the weights are loaded.
pub fn budget_mb(kind: &str) -> Option<u64> {
    MODELS.iter().find(|(id, _, _)| *id == kind).map(|(_, _, gb)| (gb * 1024.0).ceil() as u64)
}

/// Models under 4 GB get the main-subject confirmation in selection checks.
pub fn is_small(kind: &str) -> bool {
    MODELS.iter().find(|(id, _, _)| *id == kind).is_some_and(|(_, _, gb)| *gb < 4.0)
}

fn license_accepted(kind: &str) -> bool {
    match crate::models::license::policy_for(kind) {
        None => true,
        Some(policy) => crate::paths::root().is_ok_and(|root| crate::models::license::is_accepted(&root, &policy)),
    }
}

/// What the adapter needs to load `kind` from its local folder.
pub fn model_spec(kind: &str) -> Result<Value> {
    let (_, display, ram) = MODELS
        .iter()
        .find(|(id, _, _)| *id == kind)
        .ok_or_else(|| anyhow!("{kind} isn't an MLX model on this Mac"))?;
    let repo = repo(kind).ok_or_else(|| anyhow!("{kind} has no pinned MLX repository"))?;
    let license_name = match repo.license.as_str() {
        "Apache-2.0" => "Apache License 2.0",
        "Gemma" => "Gemma Terms of Use",
        other => other,
    };
    Ok(json!({
        "kind": kind,
        "repo": repo.repo,
        "huggingFaceRoot": huggingface_root(),
        "revision": repo.revision,
        "displayName": display,
        "vision": true,
        "ramBudgetGB": ram,
        "licenseName": license_name,
        "licenseAccepted": license_accepted(kind),
    }))
}

/// Adapter state (`status` reply).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub loaded: Option<String>,
    pub parked: bool,
    pub resident_bytes: u64,
    pub footprint_bytes: u64,
    pub peak_bytes: u64,
}

impl Status {
    fn from_reply(value: &Value) -> Self {
        Self {
            loaded: value["loaded"].as_str().map(str::to_owned),
            parked: value["parked"].as_bool().unwrap_or(false),
            resident_bytes: value["residentBytes"].as_u64().unwrap_or(0),
            footprint_bytes: value["footprintBytes"].as_u64().unwrap_or(0),
            peak_bytes: value["peakBytes"].as_u64().unwrap_or(0),
        }
    }

    /// Weights in memory (loaded and not parked).
    pub fn resident(&self) -> bool {
        self.loaded.is_some() && !self.parked
    }
}

pub async fn status() -> Result<Status> {
    let never = AtomicBool::new(false);
    Ok(Status::from_reply(&request(&json!({"op": "status"}), &never, |_| {}).await?))
}

/// Deep Analyze of one file (`DeepAnalyze.analyze`).
#[derive(Debug, Clone, Default)]
pub struct AnalyzeRequest {
    pub path: PathBuf,
    /// The Swift `FileKind` raw value (`image`, `video`, `pdf`, `doc`, `model`, …).
    pub media_kind: String,
    pub document_text: Option<String>,
    pub face_names: Vec<String>,
    pub propose_filename: bool,
    pub tags_only: bool,
    pub scene_caption: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AnalyzeReply {
    pub description: String,
    pub proposed_name: Option<String>,
    pub tags: Vec<String>,
}

impl AnalyzeReply {
    /// `DeepAnalyzeRunner.isAnalysisFailure(description)`.
    pub fn description_failed(&self) -> bool {
        self.description.starts_with("Inference failed")
            || self.description.starts_with("Could not decode")
            || self.description == "Model not loaded."
    }

    /// `DeepAnalyzeRunner.isAnalysisFailure(result)`: a failure text, or nothing usable.
    pub fn failed(&self) -> bool {
        self.description_failed()
            || (self.description.trim().is_empty()
                && self.proposed_name.as_deref().is_none_or(|name| name.trim().is_empty())
                && self.tags.iter().all(|tag| tag.trim().is_empty()))
    }
}

/// Image for a `generate` request.
#[derive(Debug, Clone)]
pub enum Image<'a> {
    None,
    /// The benchmark's deterministic test card.
    TestCard,
    /// A file decoded with the Swift selection-check rules: bounded to this many
    /// pixels on the long side (a video's middle frame), or whole with `None`.
    File(&'a Path, Option<u32>),
}

/// The engine's single MLX model: serializes users, keeps the scheduler's
/// memory lease in step with the adapter's residency.
pub struct Resident {
    state: Arc<tokio::sync::Mutex<State>>,
    waiters: AtomicUsize,
    scheduler: Arc<ResourceScheduler>,
}

#[derive(Default)]
struct State {
    kind: Option<String>,
    memory: Option<Lease>,
}

/// Exclusive use of the loaded model.
pub struct MlxLease {
    kind: String,
    state: tokio::sync::OwnedMutexGuard<State>,
    scheduler: Arc<ResourceScheduler>,
}

struct Waiting<'a>(&'a AtomicUsize);

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Resident {
    pub fn shared() -> &'static Arc<Resident> {
        static SHARED: OnceLock<Arc<Resident>> = OnceLock::new();
        SHARED.get_or_init(|| {
            let resident = Arc::new(Resident {
                state: Arc::new(tokio::sync::Mutex::new(State::default())),
                waiters: AtomicUsize::new(0),
                scheduler: ResourceScheduler::shared(),
            });
            let watcher = resident.clone();
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(Duration::from_secs(5));
                loop {
                    tick.tick().await;
                    watcher.sweep().await;
                }
            });
            resident
        })
    }

    /// Another job (a Chat answer) is waiting for the model.
    pub fn has_waiters(&self) -> bool {
        self.waiters.load(Ordering::Acquire) > 0
    }

    /// Load `kind` (or reuse it), holding its memory in the scheduler, and take
    /// exclusive use of it. `progress` gets the adapter's load events.
    pub async fn acquire(&self, kind: &str, cancel: &Arc<AtomicBool>, mut progress: impl FnMut(Event)) -> Result<MlxLease> {
        let spec = model_spec(kind)?;
        if !installed(kind) {
            bail!("{} isn't installed. Install it from Settings → AI Models.", spec["displayName"].as_str().unwrap_or(kind));
        }
        self.waiters.fetch_add(1, Ordering::AcqRel);
        let waiting = Waiting(&self.waiters);
        let mut state = tokio::select! {
            biased;
            () = wait_cancelled(cancel) => return Err(Cancelled.into()),
            state = self.state.clone().lock_owned() => state,
        };
        drop(waiting);
        let current = status().await?;
        if state.kind.as_deref() != Some(kind) || !current.resident() || current.loaded.as_deref() != Some(kind) {
            state.memory = None;
            state.kind = None;
            let memory_mb = budget_mb(kind).unwrap_or(4096);
            let scheduler = self.scheduler.clone();
            let flag = cancel.clone();
            let lease = tokio::task::spawn_blocking(move || {
                scheduler.reserve(Demand { memory_mb, cpu: 0, io: 0 }, Priority::Interactive, || flag.load(Ordering::Acquire))
            })
            .await
            .map_err(|error| anyhow!(error))??;
            request(&json!({"op": "load", "model": spec}), cancel, &mut progress).await?;
            lease.retain_memory();
            state.memory = Some(lease);
            state.kind = Some(kind.to_owned());
        }
        Ok(MlxLease { kind: kind.to_owned(), state, scheduler: self.scheduler.clone() })
    }

    /// Release the scheduler's memory once the adapter parks or drops the
    /// weights; unload under memory pressure when nobody is using the model.
    async fn sweep(&self) {
        let Ok(mut state) = self.state.clone().try_lock_owned() else { return };
        if state.memory.is_none() {
            return;
        }
        if self.scheduler.under_pressure() {
            let never = AtomicBool::new(false);
            if let Err(error) = request(&json!({"op": "unload"}), &never, |_| {}).await {
                tracing::warn!(?error, "unloading the MLX model under memory pressure failed");
            }
        }
        match status().await {
            Ok(current) if !current.resident() => {
                state.memory = None;
                state.kind = None;
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(?error, "MLX model status failed"),
        }
    }

    /// Drop the weights (model switch in Settings, shutdown).
    pub async fn unload(&self) -> Result<()> {
        let mut state = self.state.clone().lock_owned().await;
        let never = AtomicBool::new(false);
        request(&json!({"op": "unload"}), &never, |_| {}).await?;
        state.memory = None;
        state.kind = None;
        Ok(())
    }
}

async fn wait_cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

impl MlxLease {
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// A parked model reloads inside the adapter on its next use; take its
    /// memory back in the scheduler first.
    async fn resident(&mut self, cancel: &Arc<AtomicBool>) -> Result<()> {
        if self.state.memory.is_some() && status().await?.resident() {
            return Ok(());
        }
        let memory_mb = budget_mb(&self.kind).unwrap_or(4096);
        let scheduler = self.scheduler.clone();
        let flag = cancel.clone();
        let lease = tokio::task::spawn_blocking(move || {
            scheduler.reserve(Demand { memory_mb, cpu: 0, io: 0 }, Priority::Interactive, || flag.load(Ordering::Acquire))
        })
        .await
        .map_err(|error| anyhow!(error))??;
        lease.retain_memory();
        self.state.memory = Some(lease);
        self.state.kind = Some(self.kind.clone());
        Ok(())
    }

    /// Start decoding the next image while the current one runs (`DecodePrefetch`).
    pub async fn prefetch(&self, path: &Path) {
        let never = AtomicBool::new(false);
        let _ = request(&json!({"op": "prefetch", "path": path, "maxPixelSize": 768}), &never, |_| {}).await;
    }

    pub async fn analyze(&mut self, file: &AnalyzeRequest, cancel: &Arc<AtomicBool>, mut on_token: impl FnMut(&str)) -> Result<AnalyzeReply> {
        self.resident(cancel).await?;
        let body = json!({
            "op": "analyze",
            "path": file.path,
            "mediaKind": file.media_kind,
            "documentText": file.document_text,
            "faceNames": file.face_names,
            "proposeFilename": file.propose_filename,
            "tagsOnly": file.tags_only,
            "sceneCaption": file.scene_caption,
            "stream": true,
        });
        let reply = request(&body, cancel, |event| {
            if let Event::Token(chunk) = event {
                on_token(&chunk);
            }
        })
        .await?;
        Ok(AnalyzeReply {
            description: reply["description"].as_str().unwrap_or_default().to_owned(),
            proposed_name: reply["proposedName"].as_str().map(str::to_owned),
            tags: reply["tags"].as_array().map(|tags| tags.iter().filter_map(|tag| tag.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
        })
    }

    /// One greedy generation (`DeepAnalyze.benchmarkPass`): selection checks,
    /// goal ranking and benchmarks.
    pub async fn generate(&mut self, prompt: &str, image: Image<'_>, max_tokens: u32, image_side: u32, cancel: &Arc<AtomicBool>) -> Result<String> {
        self.resident(cancel).await?;
        let mut body = json!({"op": "generate", "prompt": prompt, "maxTokens": max_tokens, "imageSide": image_side});
        match image {
            Image::None => {}
            Image::TestCard => body["testCard"] = json!(true),
            Image::File(path, bound) => {
                body["path"] = json!(path);
                if let Some(side) = bound {
                    body["maxPixelSize"] = json!(side);
                }
            }
        }
        let reply = request(&body, cancel, |_| {}).await?;
        Ok(reply["output"].as_str().unwrap_or_default().to_owned())
    }

    /// A catalog answer with the chat prompt cache (`DeepAnalyze.answerCatalog`).
    pub async fn chat(&mut self, prompt: &str, cancel: &Arc<AtomicBool>, mut on_token: impl FnMut(&str)) -> Result<String> {
        self.resident(cancel).await?;
        let reply = request(&json!({"op": "chat", "prompt": prompt}), cancel, |event| {
            if let Event::Token(chunk) = event {
                on_token(&chunk);
            }
        })
        .await?;
        Ok(reply["answer"].as_str().unwrap_or_default().to_owned())
    }

    /// Greedy transcription of the text in a photo or PDF page (`DeepAnalyze.transcribeText`).
    pub async fn transcribe(&mut self, path: &Path, pdf: bool, cancel: &Arc<AtomicBool>) -> Result<String> {
        self.resident(cancel).await?;
        let reply = request(&json!({"op": "transcribe", "path": path, "quickLook": pdf, "maxPixelSize": 1536}), cancel, |_| {}).await?;
        Ok(reply["text"].as_str().unwrap_or_default().to_owned())
    }

    /// Describe 2–8 ordered frames: a timeline moment, or a take's outcome for `goal`.
    pub async fn sequence(&mut self, frames: &[PathBuf], times: &[f64], goal: Option<&str>, cancel: &Arc<AtomicBool>) -> Result<String> {
        self.resident(cancel).await?;
        let mut body = json!({"op": "sequence", "paths": frames, "times": times});
        if let Some(goal) = goal {
            body["goal"] = json!(goal);
        }
        let reply = request(&body, cancel, |_| {}).await?;
        Ok(reply["description"].as_str().unwrap_or_default().to_owned())
    }

    /// The Swift engine's MLX benchmark (`ModelBenchmarkRunner.run`); returns
    /// the `ModelBenchmark` JSON object. It reloads the model cold.
    pub async fn benchmark(&mut self, cancel: &Arc<AtomicBool>, progress: impl FnMut(Event)) -> Result<Value> {
        let spec = model_spec(&self.kind)?;
        let reply = request(&json!({"op": "benchmark", "model": spec}), cancel, progress).await;
        // The benchmark unloads and reloads the model; keep the memory lease only if it stayed.
        if !status().await.is_ok_and(|current| current.resident() && current.loaded.as_deref() == Some(&self.kind)) {
            self.state.memory = None;
            self.state.kind = None;
        }
        Ok(reply?["benchmark"].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analysis_failures_match_the_swift_runner() {
        let reply = |description: &str, name: Option<&str>, tags: &[&str]| AnalyzeReply {
            description: description.into(),
            proposed_name: name.map(str::to_owned),
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
        };
        assert!(reply("Inference failed: Cancelled.", None, &[]).failed());
        assert!(reply("Could not decode image.", None, &[]).failed());
        assert!(reply("Model not loaded.", None, &[]).failed());
        assert!(reply("  ", Some(" "), &[" "]).failed());
        assert!(!reply("", None, &["dog"]).failed());
        assert!(!reply("A dog on a beach.", None, &[]).failed());
    }

    #[test]
    fn specs_name_the_pinned_mlx_repository() {
        let spec = model_spec("qwen3_vl_4b").unwrap();
        assert_eq!(spec["repo"], "lmstudio-community/Qwen3-VL-4B-Instruct-MLX-4bit");
        assert_eq!(spec["revision"], "552af30c9952c44f1e1a27c7c5810ded58e892bc");
        assert_eq!(spec["ramBudgetGB"], 5.0);
        assert_eq!(spec["licenseAccepted"], true);
        assert_eq!(budget_mb("qwen3_vl_2b"), Some(3072));
        assert!(is_small("qwen3_vl_2b") && !is_small("qwen3_vl_4b"));
        assert!(model_spec("not_a_model").is_err());
    }

    #[tokio::test]
    async fn adapter_answers_status_and_refuses_bad_requests() {
        let current = status().await.unwrap();
        assert_eq!(current.loaded, None);
        assert!(!current.resident());
        let never = AtomicBool::new(false);
        let error = request(&json!({"op": "nonsense"}), &never, |_| {}).await.unwrap_err();
        assert!(error.to_string().contains("Unknown request"), "{error:#}");
        let error = request(&json!({"op": "analyze"}), &never, |_| {}).await.unwrap_err();
        assert!(error.to_string().contains("No file"), "{error:#}");
    }

    #[test]
    fn tests_without_a_root_never_see_the_owners_models() {
        if std::env::var_os("FILEID_HF_CACHE_ROOT").is_none() {
            assert!(!huggingface_root().ends_with("Documents/huggingface"));
            assert!(!installed("qwen3_vl_2b"));
        }
    }

    #[tokio::test]
    async fn loading_a_missing_model_fails_without_a_download() {
        // The test root (scratch or an empty temp path) holds no models.
        let spec = model_spec("qwen3_vl_2b").unwrap();
        let folder = model_dir("qwen3_vl_2b").unwrap();
        let existed = folder.exists();
        let never = AtomicBool::new(false);
        let error = request(&json!({"op": "load", "model": spec}), &never, |_| {}).await.unwrap_err();
        let message = error.to_string();
        assert!(
            ["isn't installed", "mlx.metallib is missing", "different model folders"].iter().any(|text| message.contains(text)),
            "{error:#}"
        );
        assert_eq!(folder.exists(), existed, "a failed load must not create the model folder");
    }

    #[tokio::test]
    async fn cancelling_before_the_reply_reports_cancelled() {
        let cancel = AtomicBool::new(true);
        let mut spec = model_spec("qwen3_vl_2b").unwrap();
        spec["huggingFaceRoot"] = json!(std::env::temp_dir().join("fileid-mlx-cancel"));
        let result = request(&json!({"op": "load", "model": spec}), &cancel, |_| {}).await;
        assert!(result.is_err());
    }

    /// Needs a copied model: `FILEID_HF_CACHE_ROOT=<copy> cargo test … -- --ignored`.
    #[tokio::test]
    #[ignore = "loads a real MLX model"]
    async fn real_model_answers_loads_parks_and_cancels() {
        let root = std::env::var_os("FILEID_HF_CACHE_ROOT").map(PathBuf::from).expect("set FILEID_HF_CACHE_ROOT to a scratch copy of the model");
        assert!(root.is_absolute() && !root.ends_with("Documents/huggingface"), "FILEID_HF_CACHE_ROOT must be a scratch copy, not a live model folder");
        let kind = std::env::var("FILEID_TEST_MLX_KIND").unwrap_or_else(|_| "qwen3_vl_2b".into());
        assert!(installed(&kind), "copy {kind} under FILEID_HF_CACHE_ROOT first");
        let cancel = Arc::new(AtomicBool::new(false));
        let mut lease = Resident::shared().acquire(&kind, &cancel, |_| {}).await.unwrap();
        assert!(status().await.unwrap().resident());
        let answer = lease.generate("What colour is the large circle in this image? Answer with one word.", Image::TestCard, 8, 448, &cancel).await.unwrap();
        assert!(answer.to_lowercase().contains("red"), "{answer}");
        let mut chunks = Vec::new();
        let first = lease.chat("Request: dogs\nMatches found: 0\n<evidence>\n</evidence>", &cancel, |chunk| chunks.push(chunk.to_owned())).await.unwrap();
        let again = lease.chat("Request: dogs\nMatches found: 0\n<evidence>\n</evidence>", &cancel, |_| {}).await.unwrap();
        assert_eq!(first, again, "the prompt cache must not change a greedy answer");
        assert!(!chunks.is_empty());
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            flag.store(true, Ordering::Release);
        });
        let started = std::time::Instant::now();
        let cancelled = lease.generate("Write a long story about a lighthouse.", Image::None, 2000, 448, &stop).await;
        assert!(cancelled.unwrap_err().downcast_ref::<Cancelled>().is_some());
        assert!(started.elapsed() < Duration::from_secs(5));
        drop(lease);
        Resident::shared().unload().await.unwrap();
        assert!(!status().await.unwrap().resident());
    }
}
