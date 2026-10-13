use anyhow::{bail, Result};
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path, sync::OnceLock};

pub const DIMENSION: usize = 512;

pub fn model_id() -> &'static str {
    static ID: OnceLock<String> = OnceLock::new();
    ID.get_or_init(|| {
        let image = artifacts("mobileclip_s2");
        let text = artifacts("clip_text");
        let ids = [
            "clip_vitb32_image",
            "clip_vitb32_text",
            "clip_bpe_vocab",
            "clip_bpe_merges",
        ];
        let hashes = image.iter().chain(text.iter());
        let mut descriptor = "clip-rgb-stretch224-bpe77-l2-v1".to_owned();
        for (id, artifact) in ids.iter().zip(hashes) {
            descriptor.push('|');
            descriptor.push_str(id);
            descriptor.push(':');
            descriptor.push_str(artifact.sha256.as_deref().unwrap_or("missing"));
        }
        format!("clip-vitb32-openai-v1:{}", digest(descriptor.as_bytes()))
    })
}

fn artifacts(kind: &str) -> Vec<super::registry::FileEntry> {
    match super::registry::lookup_full(kind) {
        super::registry::LookupResult::Found(model) => model.files,
        super::registry::LookupResult::Unknown => Vec::new(),
    }
}

pub fn verify(path: &Path, kind: &str, index: usize) -> Result<()> {
    let entries = artifacts(kind);
    let expected = entries
        .get(index)
        .and_then(|entry| entry.sha256.as_deref())
        .ok_or_else(|| anyhow::anyhow!("CLIP artifact identity is unavailable"))?;
    let mut file = std::fs::File::open(path)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() == 0 {
        bail!("Invalid CLIP artifact")
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    if hex::encode(hasher.finalize()) != expected {
        bail!("CLIP artifact checksum mismatch; reinstall the approved model")
    }
    Ok(())
}

pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn valid(vector: &[f32]) -> bool {
    vector.len() == DIMENSION
        && vector.iter().all(|value| value.is_finite())
        && (vector
            .iter()
            .map(|value| f64::from(*value).powi(2))
            .sum::<f64>()
            - 1.0)
            .abs()
            <= 0.02
}

pub fn decode(blob: &[u8]) -> Option<Vec<f32>> {
    if blob.len() != DIMENSION * 4 {
        return None;
    }
    let vector: Vec<_> = blob
        .chunks_exact(4)
        .map(|bytes| f32::from_le_bytes(bytes.try_into().expect("four bytes")))
        .collect();
    valid(&vector).then_some(vector)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unverified_or_empty_installed_artifacts_cannot_load_as_current_clip() {
        let path =
            std::env::temp_dir().join(format!("fileid-clip-artifact-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"unverified local fixture").unwrap();
        assert!(verify(&path, "mobileclip_s2", 0)
            .unwrap_err()
            .to_string()
            .contains("checksum"));
        assert!(verify(&path, "clip_text", 0).is_err());
        std::fs::write(&path, []).unwrap();
        assert!(verify(&path, "mobileclip_s2", 0).is_err());
        std::fs::remove_file(path).unwrap();
    }
    #[test]
    fn exact_space_rejects_legacy_dimensions_and_nonfinite_values() {
        let mut vector = vec![0.0f32; DIMENSION];
        vector[0] = 1.0;
        let blob: Vec<_> = vector
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        assert_eq!(decode(&blob), Some(vector.clone()));
        assert!(!valid(&vector[..128]));
        vector[0] = f32::NAN;
        assert!(!valid(&vector));
        vector[0] = 0.0;
        assert!(!valid(&vector));
        assert_eq!(model_id(), "clip-vitb32-openai-v1:126bacbb4038a0820ca6582bd81f58d7892070deab8317b390e37d7632a53471");
        assert_ne!(model_id(), "mobileclip_s2");
    }
}
