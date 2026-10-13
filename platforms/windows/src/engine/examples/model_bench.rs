//! Headless on-device benchmark for installed models.
//!
//!     cargo run --release --example model_bench -- [--tune] <model_kind>...
//!
//! Prints one JSON line per model (`{"model":…,"benchmark":…}` or
//! `{"model":…,"error":…}`) and stores results where `listModels` reads them.
//! Uses the same runtime discovery as the app (`FILEID_LLAMA_SERVER`,
//! `FILEID_MODELS_DIR`); it never downloads anything.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let mut tune = false;
    let mut kinds = Vec::new();
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--tune" => tune = true,
            "-h" | "--help" => {
                eprintln!("usage: model_bench [--tune] <model_kind>...");
                return;
            }
            kind => kinds.push(kind.to_owned()),
        }
    }
    if kinds.is_empty() {
        eprintln!("usage: model_bench [--tune] <model_kind>...");
        std::process::exit(2);
    }
    let mut failures = 0;
    for kind in kinds {
        let cancel = Arc::new(AtomicBool::new(false));
        let result = fileid_engine::models::benchmark::run(&kind, tune, cancel, |phase, done, total| {
            eprintln!("{kind}: {phase} {done}/{total}");
        })
        .await;
        let line = match result {
            Ok(benchmark) => serde_json::json!({ "model": kind, "benchmark": benchmark }),
            Err(error) => {
                failures += 1;
                serde_json::json!({ "model": kind, "error": format!("{error:#}") })
            }
        };
        println!("{line}");
    }
    std::process::exit(i32::from(failures > 0));
}
