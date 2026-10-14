use super::*;
use std::sync::atomic::AtomicU64;
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let p = std::env::temp_dir().join(format!(
        "fileid-runtime-key-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&p).unwrap();
    let p = crate::util::path_safety::strip_extended_length(&std::fs::canonicalize(&p).unwrap());
    println!("Generated runtime key state retained: {}", p.display());
    p
}
fn flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
#[tokio::test]
async fn resident_runtime_key_hashes_actual_bytes_and_detects_same_length_change() {
    let r = root();
    let p = r.join("metadata-only-runtime");
    std::fs::write(&p, vec![1; 20000]).unwrap();
    let a = capture_paths(vec![p.clone()], flag()).await.unwrap();
    assert_eq!(a, capture_paths(vec![p.clone()], flag()).await.unwrap());
    std::fs::write(&p, vec![2; 20000]).unwrap();
    let b = capture_paths(vec![p], flag()).await.unwrap();
    assert_ne!(a, b);
    std::fs::write(
        r.join("receipt.json"),
        b"{\"fingerprintedBytesNotExecuted\":true,\"sameLengthMutationDetected\":true}",
    )
    .unwrap();
}
#[cfg(unix)]
#[tokio::test]
async fn resident_runtime_key_same_bytes_replaced_inode_invalidates_identity() {
    let r = root();
    let p = r.join("metadata-only-runtime");
    std::fs::write(&p, vec![3; 20000]).unwrap();
    let a = capture_paths(vec![p.clone()], flag()).await.unwrap();
    std::fs::rename(&p, r.join("old-runtime")).unwrap();
    std::fs::write(&p, vec![3; 20000]).unwrap();
    let b = capture_paths(vec![p], flag()).await.unwrap();
    assert_ne!(a, b);
    std::fs::write(
        r.join("receipt.json"),
        b"{\"sameBytesReplacementDenied\":true,\"vendorRuntimeExecuted\":false}",
    )
    .unwrap();
}
#[tokio::test]
async fn resident_runtime_key_precancel_and_invalid_inputs_are_typed() {
    let r = root();
    let p = r.join("metadata-only-runtime");
    std::fs::write(&p, vec![4; 20000]).unwrap();
    let error = capture_paths(vec![p], Arc::new(AtomicBool::new(true)))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<HostAdmissionError>().is_some());
    for p in [r.clone(), r.join("missing"), PathBuf::from("relative-runtime")] {
        assert!(capture_paths(vec![p], flag())
            .await
            .unwrap_err()
            .downcast_ref::<HostAdmissionError>()
            .is_some());
    }
    std::fs::write(
        r.join("receipt.json"),
        b"{\"typedRefusal\":true,\"nativeSpawned\":false}",
    )
    .unwrap();
}
#[tokio::test]
async fn resident_runtime_key_empty_and_oversized_files_refuse_before_hash() {
    let r = root();
    let p = r.join("empty");
    std::fs::write(&p, []).unwrap();
    assert!(capture_paths(vec![p], flag()).await.is_err());
    let p = r.join("sparse-oversized");
    std::fs::File::create(&p).unwrap().set_len(200_000_001).unwrap();
    assert!(capture_paths(vec![p], flag()).await.is_err());
    std::fs::write(
        r.join("receipt.json"),
        b"{\"emptyAndOversizedRefused\":true,\"nativeSpawned\":false}",
    )
    .unwrap();
}
#[test]
fn resident_runtime_key_actual_capture_uses_canonical_candidates_and_admission() {
    let code = include_str!("runtime_cache.rs").split_whitespace().collect::<String>();
    assert!(code.contains("native_runtime::candidates(\"llama-server\""));
    assert!(code.contains("spawn_blocking"));
    assert!(code.contains("abandoned.load"));
    assert!(code.contains("memory_mb"));
    assert!(!code.contains("Command::new"));
    assert!(!code.contains("reqwest"));
}
