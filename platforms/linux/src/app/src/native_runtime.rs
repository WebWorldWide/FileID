use std::{io::Read, path::Path};

pub const VLM_KIND: &str = "qwen2_5_vl_7b";
pub const EXECUTABLES: [&str; 5] = [
    "FILEID_LLAMA_SERVER",
    "FILEID_LLAMA_CLI",
    "FILEID_WHISPER_CLI",
    "FILEID_FFMPEG",
    "FILEID_FFPROBE",
];

pub fn validate_executable(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || !path.is_file() {
        return Err(
            "Set an absolute path to an installed native executable, not a command or URI.".into(),
        );
    }
    let metadata = path.metadata().map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("The configured file is not executable.".into());
        }
    }
    #[cfg(not(unix))]
    let _ = metadata;
    let mut header = [0; 64];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut header))
        .map_err(|_| "The configured runtime has no valid ELF header.".to_owned())?;
    validate_elf(&header)
}

fn validate_elf(header: &[u8; 64]) -> Result<(), String> {
    let machine = u16::from_le_bytes([header[18], header[19]]);
    let expected = match std::env::consts::ARCH {
        "x86_64" => 62,
        "aarch64" => 183,
        _ => return Err("Native runtime architecture is unsupported.".into()),
    };
    if &header[..4] != b"\x7fELF" || header[4] != 2 || header[5] != 1 || machine != expected {
        return Err("Runtime must be a 64-bit Linux ELF for this host; Windows .exe and incompatible architectures are not supported.".into());
    }
    Ok(())
}

pub fn configured(name: &str) -> Result<(), String> {
    let path = std::env::var_os(name).ok_or_else(|| format!("{name} is not configured."))?;
    validate_executable(Path::new(&path)).map_err(|e| format!("{name}: {e}"))
}

pub fn require_vlm() -> Result<(), String> {
    Err("Native AI analysis, timeline generation and model summaries are unavailable with this backend. Installed weights and optional executable configuration do not establish accepted loader or inference support. Stored captions and keyword retrieval remain available.".into())
}

pub fn summary() -> String {
    EXECUTABLES
        .iter()
        .map(|name| match configured(name) {
            Ok(()) => format!(
                "{name}: ELF configuration check passed (information only; not backend capability or loader attestation)"
            ),
            Err(e) => e,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_runtime_capability_stays_closed_independent_of_configuration() {
        let reason = require_vlm().unwrap_err();
        assert!(reason.contains("unavailable with this backend"));
        assert!(reason.contains("Stored captions and keyword retrieval remain available"));
    }

    #[test]
    fn native_runtime_rejects_windows_wrong_arch_and_command_strings() {
        assert!(validate_executable(Path::new("ffmpeg -i video.mp4")).is_err());
        assert!(validate_executable(Path::new("https://example.invalid/runtime")).is_err());
        let mut header = [0; 64];
        header[..4].copy_from_slice(b"\x7fELF");
        header[4] = 2;
        header[5] = 1;
        header[6] = 1;
        header[16] = 3;
        header[18] = if std::env::consts::ARCH == "aarch64" {
            183
        } else {
            62
        };
        assert!(validate_elf(&header).is_ok());
        header[18] = 3;
        assert!(validate_elf(&header).is_err());
        header[..2].copy_from_slice(b"MZ");
        assert!(validate_elf(&header).is_err());
    }
}
