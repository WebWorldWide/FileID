//! Sidecar undo log for `trashFiles` → `restoreFromTrash`.
//!
//! `trash_log.json` is an append-only NDJSON file capped at the last 1024
//! entries. Lets the app's UndoStack stay process-local across restarts
//! AND lets `restoreFromTrash` know which paths to bring back from the
//! Recycle Bin without bloating the SQLite schema.

use std::io::Write;

use crate::paths;
use crate::util::hmac;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct TrashLogEntry {
    pub(crate) batch_id: String,
    pub(crate) timestamp: f64,
    pub(crate) items: Vec<TrashLogItem>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct TrashLogItem {
    pub(crate) file_id: i64,
    pub(crate) original_path: String,
    /// Hint set by IFileOperation if available (.GetName on the IShellItem
    /// after delete) — the Recycle Bin renames each item to a $R*.* form.
    /// Often empty; restore by path is the canonical fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) recycle_bin_id: Option<String>,
}

/// The append-only log is trimmed to the last `MAX_ENTRIES` lines so it can't
/// grow without bound over a long-lived install (the module doc promises this
/// cap). Trimming drops whole oldest lines verbatim — retained lines keep
/// their HMAC, so `read_batch` still verifies them.
const MAX_ENTRIES: usize = 1024;

pub(crate) fn append(entry: &TrashLogEntry) -> anyhow::Result<()> {
    let path = paths::trash_log_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let json = serde_json::to_string(entry)?;
    // HMAC-sign each entry so a local attacker who appends a forged
    // entry can't get it accepted by restoreFromTrash. Entry format is
    // `{json}\t{hex_hmac}`.
    let mac = hmac::hmac_sha256_hex(&hmac::log_hmac_key()?, json.as_bytes());
    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{json}\t{mac}")?;
        // Force flush so a crash immediately after delete-to-trash doesn't lose
        // the log entry (which would orphan the Recycle Bin items).
        file.sync_all()?;
    }
    // Enforce the documented cap. Best-effort: a trim failure must not fail the
    // delete (the entry is already durably appended above).
    trim_to_cap(&path).ok();
    Ok(())
}

/// Keep only the last `MAX_ENTRIES` non-empty lines, atomically (temp + rename).
fn trim_to_cap(path: &std::path::Path) -> anyhow::Result<()> {
    let raw = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= MAX_ENTRIES {
        return Ok(());
    }
    let keep = &lines[lines.len() - MAX_ENTRIES..];
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("trash_log.json");
    let tmp = path.with_file_name(format!("{file_name}.tmp"));
    {
        let mut f = std::fs::File::create(&tmp)?;
        for l in keep {
            writeln!(f, "{l}")?;
        }
        f.sync_all()?;
    }
    // std::fs::rename replaces atomically on Windows (MoveFileEx REPLACE).
    std::fs::rename(&tmp, path)?;
    Ok(())
}

pub(crate) fn read_batch(batch_id: &str) -> anyhow::Result<Option<TrashLogEntry>> {
    let path = paths::trash_log_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let key = hmac::log_hmac_key()?;
    let raw = std::fs::read_to_string(&path)?;
    for line in raw.lines() {
        if line.trim().is_empty() {
            continue;
        }
        // Split {json}\t{hex_hmac}. Entries without the HMAC suffix
        // (legacy writes or forged appends) are rejected.
        let Some(tab) = line.find('\t') else {
            tracing::warn!("trash_log entry missing HMAC suffix -- rejecting");
            continue;
        };
        let (payload, expected) = (&line[..tab], &line[tab + 1..]);
        let actual = hmac::hmac_sha256_hex(&key, payload.as_bytes());
        if !hmac::constant_time_eq_str(&actual, expected) {
            tracing::warn!("trash_log entry HMAC mismatch -- rejecting forged entry");
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<TrashLogEntry>(payload) {
            if entry.batch_id == batch_id {
                return Ok(Some(entry));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::hmac::{hmac_sha256_hex, log_hmac_key};

    fn make_entry(batch_id: &str) -> TrashLogEntry {
        TrashLogEntry {
            batch_id: batch_id.to_string(),
            timestamp: 1700000000.0,
            items: vec![TrashLogItem {
                file_id: 42,
                original_path: r"C:\Users\u\Pictures\cat.jpg".to_string(),
                recycle_bin_id: None,
            }],
        }
    }

    #[test]
    fn entry_serde_round_trip() {
        // Doesn't touch disk — just confirms the wire shape is stable.
        let entry = make_entry("batch-1");
        let json = serde_json::to_string(&entry).unwrap();
        let decoded: TrashLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.batch_id, "batch-1");
        assert_eq!(decoded.items.len(), 1);
        assert_eq!(decoded.items[0].file_id, 42);
    }

    #[test]
    fn forged_entry_rejected_by_hmac_check() {
        // Manually simulate the file format:
        //   {valid_json}\t{valid_hmac}\n
        // Then a hostile appender:
        //   {forged_json}\t{wrong_hmac}\n
        // The hostile line must fail verification.
        let key = log_hmac_key().expect("hmac key");

        let real_entry = make_entry("real-batch");
        let real_json = serde_json::to_string(&real_entry).unwrap();
        let real_mac = hmac_sha256_hex(&key, real_json.as_bytes());

        let forged_entry = make_entry("forged-batch");
        let forged_json = serde_json::to_string(&forged_entry).unwrap();
        let wrong_mac = hmac_sha256_hex(b"different-key", forged_json.as_bytes());

        // Reconstruct the verification logic from read_batch inline so we
        // don't have to touch the filesystem.
        let lines = vec![
            format!("{real_json}\t{real_mac}"),
            format!("{forged_json}\t{wrong_mac}"),
        ];
        let mut accepted_batches = Vec::new();
        for line in &lines {
            if let Some(idx) = line.find('\t') {
                let payload = &line[..idx];
                let mac_hex = &line[idx + 1..];
                let actual = hmac_sha256_hex(&key, payload.as_bytes());
                if !crate::util::hmac::constant_time_eq_str(&actual, mac_hex) {
                    continue;
                }
                let entry: TrashLogEntry = serde_json::from_str(payload).unwrap();
                accepted_batches.push(entry.batch_id);
            }
        }
        assert_eq!(accepted_batches, vec!["real-batch".to_string()]);
    }
}
