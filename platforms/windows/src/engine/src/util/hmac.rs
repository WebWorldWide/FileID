//! V14.7.2: HMAC-SHA256 hand-rolled atop the existing `sha2` dependency.
//! 30 lines beats adding the `hmac` crate for one call site.
//!
//! Used by the trash + merge sidecar logs to seal each entry against
//! local tampering. The key is persisted at
//! `%LOCALAPPDATA%\FileID\log-hmac.key`; NTFS ACLs on `%LOCALAPPDATA%`
//! already restrict to the user, which is enough for the threat model
//! (defense against another local app's tampering).

use crate::paths;

pub(crate) fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    const BLOCK_SIZE: usize = 64;
    let mut k = [0u8; BLOCK_SIZE];
    if key.len() > BLOCK_SIZE {
        let h = Sha256::digest(key);
        k[..32].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK_SIZE];
    let mut opad = [0x5cu8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner_hash = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner_hash);
    let out = outer.finalize();
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&out);
    bytes
}

pub(crate) fn hmac_sha256_hex(key: &[u8], msg: &[u8]) -> String {
    hex::encode(hmac_sha256(key, msg))
}

/// Constant-time string comparison (avoids timing-side-channel HMAC
/// validation). Caller supplies hex strings of equal length; mismatched
/// lengths short-circuit fail.
pub(crate) fn constant_time_eq_str(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Lazily-loaded 32-byte HMAC key for the trash/merge sidecar logs.
pub(crate) fn log_hmac_key() -> anyhow::Result<Vec<u8>> {
    static KEY: parking_lot::Mutex<Option<Vec<u8>>> = parking_lot::Mutex::new(None);
    let mut guard = KEY.lock();
    if let Some(k) = guard.as_ref() {
        return Ok(k.clone());
    }
    let root = paths::root()?;
    std::fs::create_dir_all(&root).ok();
    let path = root.join("log-hmac.key");
    let bytes = if path.exists() {
        std::fs::read(&path)?
    } else {
        // Generate via getrandom() through `uuid` (already a dep).
        // Two UUIDs = 32 bytes of OS-CSPRNG entropy.
        let mut k = Vec::with_capacity(32);
        k.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        k.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        std::fs::write(&path, &k)?;
        k
    };
    *guard = Some(bytes.clone());
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 4231 test vector: HMAC-SHA256 with key = 20 bytes of 0x0b,
    // msg = "Hi There" → b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7
    #[test]
    fn hmac_sha256_matches_rfc4231_vector_1() {
        let key = [0x0bu8; 20];
        let msg = b"Hi There";
        let mac = hmac_sha256_hex(&key, msg);
        assert_eq!(
            mac,
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    // RFC 4231 test vector 2: key = "Jefe", msg = "what do ya want for nothing?"
    #[test]
    fn hmac_sha256_matches_rfc4231_vector_2() {
        let mac = hmac_sha256_hex(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            mac,
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn hmac_handles_key_longer_than_block_size() {
        // 64 bytes is the block size; 65 should still produce a stable hash
        // (the key is pre-hashed by the SHA function).
        let long_key = [0xaau8; 65];
        let a = hmac_sha256_hex(&long_key, b"x");
        let b = hmac_sha256_hex(&long_key, b"x");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64); // 32 bytes hex-encoded
    }

    #[test]
    fn constant_time_eq_rejects_length_mismatch() {
        assert!(!constant_time_eq_str("abc", "ab"));
        assert!(!constant_time_eq_str("abc", "abcd"));
    }

    #[test]
    fn constant_time_eq_accepts_equal_strings() {
        assert!(constant_time_eq_str("abc", "abc"));
        assert!(constant_time_eq_str("", ""));
    }

    #[test]
    fn constant_time_eq_rejects_single_bit_diff() {
        assert!(!constant_time_eq_str("abc", "abd"));
    }
}
