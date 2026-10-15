use super::*;
use sha2::{Digest, Sha256};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, AtomicUsize},
};
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "fileid-model-job-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&p).unwrap();
    println!("Generated model job state retained: {}", p.display());
    p
}
fn files(root: &Path) -> Vec<super::super::registry::FileEntry> {
    ["weights.fixture", "projector.fixture"]
        .iter()
        .map(|name| {
            let data = format!("generated job bytes {name}, not model weights");
            let dest = root.join(name);
            std::fs::write(&dest, &data).unwrap();
            super::super::registry::FileEntry {
                url: "fixture://no-download".into(),
                dest,
                sha256: Some(hex::encode(Sha256::digest(data.as_bytes()))),
                approx_bytes: data.len() as u64,
            }
        })
        .collect()
}
fn flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
fn receipt(root: &Path, value: serde_json::Value) {
    std::fs::write(root.join("receipt.json"), serde_json::to_vec(&value).unwrap()).unwrap();
}
async fn resolve(
    job: &ModelJob,
    kind: &str,
    entries: Vec<super::super::registry::FileEntry>,
    calls: &AtomicUsize,
) -> Result<Arc<VerifiedModel>> {
    job.resolve_with(kind, flag(), || async {
        calls.fetch_add(1, Ordering::Relaxed);
        super::super::model_provenance::verify_files("qwen3_vl_4b", "generated-not-registry-pins", entries, &|| false)
            .map_err(|e| HostAdmissionError(e).into())
    })
    .await
}
#[tokio::test]
async fn job_weights_alias_and_repeated_caption_resolve_hash_once_per_job() {
    let root = root();
    let entries = files(&root);
    let job = ModelJob::default();
    let calls = AtomicUsize::new(0);
    let first = resolve(&job, "qwen3_vl_4b", entries.clone(), &calls).await.unwrap();
    let second = resolve(&job, "qwen3-vl-4b", entries, &calls).await.unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    let paths = first.paths();
    for _ in 0..3 {
        let current = job.require_pair(paths.0, paths.1, flag()).await.unwrap();
        assert!(Arc::ptr_eq(&first, &current));
    }
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    receipt(
        &root,
        serde_json::json!({"fullHashCalls":1,"sameJobAliasAndThreeCaptions":true,"nativeModelLoaded":false}),
    );
}
#[tokio::test]
async fn job_weights_other_model_does_not_inherit_or_replace_verified_identity() {
    let root = root();
    let entries = files(&root);
    let job = ModelJob::default();
    let calls = AtomicUsize::new(0);
    resolve(&job, "qwen3_vl_4b", entries.clone(), &calls).await.unwrap();
    assert!(resolve(&job, "qwen3_vl_8b", entries.clone(), &calls)
        .await
        .unwrap_err()
        .downcast_ref::<HostAdmissionError>()
        .is_some());
    resolve(&job, "qwen3_vl_4b", entries, &calls).await.unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    receipt(
        &root,
        serde_json::json!({"differentModelDenied":true,"noAdditionalHash":true}),
    );
}
#[tokio::test]
async fn job_weights_unprepared_caption_pair_has_no_raw_weight_escape() {
    let root = root();
    let entries = files(&root);
    let job = ModelJob::default();
    let error = job
        .require_pair(&entries[0].dest, &entries[1].dest, flag())
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    receipt(
        &root,
        serde_json::json!({"unverifiedPairDenied":true,"nativeSpawned":false}),
    );
}
#[tokio::test]
async fn job_weights_swapped_or_other_pair_cannot_escape_verified_paths() {
    let root = root();
    let entries = files(&root);
    let job = ModelJob::default();
    let calls = AtomicUsize::new(0);
    let model = resolve(&job, "qwen3_vl_4b", entries, &calls).await.unwrap();
    let (a, b) = model.paths();
    assert!(job.require_pair(b, a, flag()).await.is_err());
    assert!(job.require_pair(&root.join("other"), b, flag()).await.is_err());
    job.require_pair(a, b, flag()).await.unwrap();
    receipt(
        &root,
        serde_json::json!({"mismatchedPairDenied":true,"correctPairRetained":true}),
    );
}
#[tokio::test]
async fn job_weights_changed_source_sticks_refusal_without_automatic_rehash() {
    let root = root();
    let entries = files(&root);
    let job = ModelJob::default();
    let calls = AtomicUsize::new(0);
    let model = resolve(&job, "qwen3_vl_4b", entries.clone(), &calls).await.unwrap();
    let (a, b) = model.paths();
    std::fs::write(a, b"changed source").unwrap();
    let error = job.require_pair(a, b, flag()).await.unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert!(resolve(&job, "qwen3_vl_4b", entries, &calls).await.is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    receipt(
        &root,
        serde_json::json!({"changedSourceRefused":true,"stickyWithinJob":true,"fullHashCalls":1}),
    );
}
#[cfg(unix)]
#[tokio::test]
async fn job_weights_same_bytes_replaced_inode_is_not_cache_validity() {
    let root = root();
    let entries = files(&root);
    let job = ModelJob::default();
    let calls = AtomicUsize::new(0);
    let model = resolve(&job, "qwen3_vl_4b", entries.clone(), &calls).await.unwrap();
    let (a, b) = model.paths();
    let data = std::fs::read(a).unwrap();
    std::fs::rename(a, root.join("original-opened")).unwrap();
    std::fs::write(a, data).unwrap();
    assert!(job.require_pair(a, b, flag()).await.is_err());
    assert!(resolve(&job, "qwen3_vl_4b", entries, &calls).await.is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    receipt(
        &root,
        serde_json::json!({"inodeReplacementDenied":true,"fullHashCalls":1,"postCheckLoaderRaceAccepted":false}),
    );
}
#[tokio::test]
async fn job_weights_precancel_has_no_verifier_or_native_side_effect() {
    let root = root();
    let job = ModelJob::default();
    let calls = AtomicUsize::new(0);
    let error = job
        .resolve_with("qwen3_vl_4b", Arc::new(AtomicBool::new(true)), || async {
            calls.fetch_add(1, Ordering::Relaxed);
            Err(anyhow!("should not run"))
        })
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    receipt(&root, serde_json::json!({"precancelled":true,"fullHashCalls":0}));
}
#[tokio::test]
async fn job_weights_bad_pin_sticks_refusal_before_repeated_file_work() {
    let root = root();
    let mut entries = files(&root);
    entries[0].sha256 = Some("0".repeat(64));
    let job = ModelJob::default();
    let calls = AtomicUsize::new(0);
    assert!(resolve(&job, "qwen3_vl_4b", entries.clone(), &calls).await.is_err());
    assert!(resolve(&job, "qwen3_vl_4b", entries, &calls).await.is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    receipt(
        &root,
        serde_json::json!({"badPinRefused":true,"fullHashCalls":1,"nativeSpawned":false}),
    );
}
#[test]
fn job_weights_actual_cli_and_pipeline_use_verified_job_paths() {
    let pipeline = include_str!("../pipeline/deep_analyze.rs");
    assert!(pipeline.contains("runner.verified_weights(model_kind, cancel.clone()).await?"));
    assert!(!pipeline.contains("vlm::find_weights(model_kind)"));
    let cli = include_str!("vlm.rs");
    assert!(cli.contains("runner.weights.require_pair("));
    assert!(
        cli.find("verified.recheck(cancel.clone()).await?").unwrap()
            < cli
                .find("let resources = super::native_admission::reserve_host(")
                .unwrap()
    );
}
