//! ONNX Runtime environment setup for the `ort-accel` build (Windows GPU/NPU
//! execution providers).
//!
//! `ort` is compiled with `load-dynamic`: on first use it loads
//! `onnxruntime.dll` by name, or the file `ORT_DYLIB_PATH` points at (the
//! accelerator-pack pin in `server.rs`). Every other platform, and a Windows
//! build without `ort-accel`, runs on tract and never loads ONNX Runtime.

/// Read by official ONNX Runtime builds from 1.29 on, whose macOS/Linux
/// libraries carry Microsoft's 1DS uploader. The uploader starts with the first
/// environment, writes a device id and an event queue under
/// `~/Library/Application Support/Microsoft/DeveloperTools/.onnxruntime` and
/// uploads from there; `DisableTelemetryEvents` comes too late to stop either.
/// Only this variable, set before that first environment, keeps it from
/// starting (measured with the pip 1.30.0 dylib; DECISIONS 2026-11-10).
pub const DISABLE_TELEMETRY_ENV: &str = "ORT_DISABLE_TELEMETRY";

/// Turn ONNX Runtime's own telemetry off for this process and the processes it
/// starts. Call first thing in `main`, while the process is single-threaded.
pub fn disable_telemetry_before_load() {
    // SAFETY: callers run this before any other thread exists.
    unsafe { std::env::set_var(DISABLE_TELEMETRY_ENV, "1") };
}

/// The TraceLogging metadata of ONNX Runtime's `Microsoft.ML.ONNXRuntime`
/// provider when it joins Microsoft's telemetry provider group
/// {4f50731a-89cf-4782-b3e0-dce8c90476ba}: name, NUL, a 19-byte group trait,
/// then the group GUID. Windows' diagnostic-data service collects that group.
/// Only builds made with `--use_telemetry` (Microsoft's NuGet and GitHub
/// releases) carry it; other builds hold the same provider with an all-zero
/// group. Their events cannot be turned off from outside in 1.22: the ETW
/// enable callback overrides `DisableTelemetryEvents`.
const WINDOWS_TELEMETRY_PROVIDER: &[u8] = b"Microsoft.ML.ONNXRuntime\0\x13\x00\x01\
    \x1a\x73\x50\x4f\xcf\x89\x82\x47\xb3\xe0\xdc\xe8\xc9\x04\x76\xba";

/// Whether an ONNX Runtime library is a build that reports to Windows'
/// telemetry pipeline. FileID never loads one.
pub fn joins_windows_telemetry(library: &std::path::Path) -> std::io::Result<bool> {
    let bytes = std::fs::read(library)?;
    Ok(bytes
        .windows(WINDOWS_TELEMETRY_PROVIDER.len())
        .any(|window| window == WINDOWS_TELEMETRY_PROVIDER))
}

/// Create the process-wide ORT environment with telemetry events off and the
/// native log severity clamped, or keep the one that already exists. Every
/// session path calls this first: `ort`'s lazy default environment calls
/// `EnableTelemetryEvents`, which Microsoft's Windows builds route to ETW.
/// Warning severity matters with the `tracing` feature: ort otherwise creates
/// the environment at VERBOSE and forwards every native log line through an
/// `extern "system"` callback that aborts the process on a null pointer.
pub fn commit_environment() -> ort::Result<()> {
    if ort::init().with_telemetry(false).commit()? {
        ort::environment::get_environment()?.set_log_level(ort::logging::LogLevel::Warning);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_is_disabled_before_ort_loads() {
        disable_telemetry_before_load();
        assert_eq!(std::env::var(DISABLE_TELEMETRY_ENV).as_deref(), Ok("1"));

        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let server = std::fs::read_to_string(src.join("server.rs")).unwrap();
        let run = &server[server.find("pub fn run() -> Result<()> {").expect("server::run")..];
        let disable = run
            .find("ort_runtime::disable_telemetry_before_load();")
            .expect("run() disables ORT telemetry");
        for later in [
            "tokio::runtime::Builder",
            "logging::init()",
            "ort_runtime::commit_environment",
        ] {
            assert!(
                disable < run.find(later).unwrap_or(usize::MAX),
                "{later} runs before ORT telemetry is disabled"
            );
        }

        let mut stack = vec![src];
        let mut session_sites = 0;
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().is_none_or(|e| e != "rs") || path.ends_with("ort_runtime.rs") {
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap();
                assert!(
                    !text.contains("ort::init()"),
                    "{} builds an ORT environment outside commit_environment",
                    path.display()
                );
                for (at, _) in text.match_indices("Session::builder()") {
                    let line_start = text[..at].rfind('\n').map_or(0, |i| i + 1);
                    if text[line_start..at].trim_start().starts_with("//") {
                        continue;
                    }
                    session_sites += 1;
                    let before = &text[..at];
                    let function = &before[before.rfind("fn ").unwrap_or(0)..];
                    assert!(
                        function.contains("ort_runtime::commit_environment()"),
                        "{} creates a session before committing the telemetry-off environment",
                        path.display()
                    );
                }
            }
        }
        assert!(session_sites >= 1, "session builder sites moved; update this test");
    }

    #[test]
    fn windows_telemetry_builds_are_recognised() {
        let dir = std::env::temp_dir().join(format!("fileid-ort-etw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let library = |group: &[u8]| {
            let mut bytes = b"MZ\0\0.rdata\0".to_vec();
            bytes.extend_from_slice(b"\x2e\0Microsoft.ML.ONNXRuntime\0\x13\x00\x01");
            bytes.extend_from_slice(group);
            bytes.extend_from_slice(b"\0tail");
            bytes
        };
        let telemetry_group = [
            0x1a, 0x73, 0x50, 0x4f, 0xcf, 0x89, 0x82, 0x47, 0xb3, 0xe0, 0xdc, 0xe8, 0xc9, 0x04, 0x76, 0xba,
        ];
        let official = dir.join("official.dll");
        let source_build = dir.join("source-build.dll");
        std::fs::write(&official, library(&telemetry_group)).unwrap();
        std::fs::write(&source_build, library(&[0; 16])).unwrap();

        assert!(joins_windows_telemetry(&official).unwrap());
        assert!(!joins_windows_telemetry(&source_build).unwrap());
        assert!(joins_windows_telemetry(&dir.join("missing.dll")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
