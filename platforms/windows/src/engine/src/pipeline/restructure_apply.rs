// Restructure apply — execute a `Vec<ProposedMove>` on disk.
//
// Two modes:
//   * Real move (default): handle-relative, no-replace rename. The source
//     handle stays open from identity verification through mutation, and an
//     occupied destination fails instead of being overwritten (B3). The DB row's `path_text`
//     is updated by a SEPARATE statement AFTER the move returns — this is NOT
//     one transaction with the filesystem op (it can't be). A crash in the
//     move→update window leaves the file relocated with `path_text` stale; the
//     next scan self-heals it via rename-heal on the NTFS `file_ref`, and a
//     failed update is also recorded to a recovery sidecar.
//   * Symlink (advanced): `CreateSymbolicLinkW`. Requires either
//     SeCreateSymbolicLinkPrivilege (admin) OR Developer Mode enabled.
//     Lets the user preview the proposed structure without committing
//     to actual moves.
//
// COLLISION SAFETY (B3): many distinct sources share a basename and the rule
// cascade funnels them into one folder, so two planned moves can target the
// same path. Each real-move destination is uniquified within its parent
// (`name (2).ext`, …) so both files survive; nothing is ever clobbered.
//
// STALE-PLAN / IDENTITY GUARD (B4): a plan is built from a DB snapshot, then
// applied after an arbitrary delay. Before each move the live DB row for
// `file_id` is re-read and required to still name `source`, so a plan that
// went stale (the file was renamed/moved/replaced meanwhile) can't move the
// wrong bytes — the payload `source` string is not authoritative on its own.
//
// PATH-TRAVERSAL GUARD: every destination MUST canonicalize to a path
// inside `library_root`. We refuse to write outside the user's chosen
// library — even if the planner is buggy or someone forges a payload.

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Lines, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::ipc::{RestructureApplyResult, RestructureMove};
use crate::pipeline::restructure_feedback;

type ClaimedDestination = [u8; 16];

const UNDO_JOURNAL_VERSION: u32 = 3;
const SHORTCUT_UNDO_MANIFEST_VERSION: u32 = 3;
const SHORTCUT_UNDO_RECEIPT_VERSION: u32 = 1;
const SHORTCUT_UNDO_INTENT_VERSION: u32 = 1;
const MAX_SHORTCUT_RECORD_BYTES: usize = 64 * 1024;
const MAX_SHORTCUT_STAGING_ENTRIES: usize = 1024;

#[derive(serde::Deserialize)]
struct UndoEntry {
    file_id: i64,
    from: String,
    to: String,
    #[serde(default)]
    source_identity: Option<crate::platform::FileIdentity>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ShortcutUndoEntry {
    file_id: i64,
    source: String,
    link: String,
    #[serde(default)]
    staging_link: Option<String>,
    source_identity: crate::platform::FileIdentity,
    link_identity: crate::platform::FileIdentity,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ShortcutUndoHeader {
    version: u32,
    library_root: String,
    token: String,
    #[serde(default)]
    staging_dir: Option<String>,
    #[serde(default)]
    staging_dir_identity: Option<crate::platform::FileIdentity>,
}

#[derive(serde::Deserialize, serde::Serialize)]
struct ShortcutUndoIntent {
    version: u32,
    token: String,
    operation_id: String,
    file_id: i64,
    source: String,
    link: String,
    staging_link: String,
    source_identity: crate::platform::FileIdentity,
    #[serde(default)]
    staging_link_identity: Option<crate::platform::FileIdentity>,
}

struct PreparedShortcutIntent {
    intent: ShortcutUndoIntent,
    path: PathBuf,
    identity: crate::platform::FileIdentity,
}

struct ScannedShortcutIntent {
    intent: ShortcutUndoIntent,
    path: PathBuf,
    identity: crate::platform::FileIdentity,
    committed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
struct ShortcutUndoReceipt {
    version: u32,
    library_root: String,
    token: String,
    applied: u32,
    planned: u64,
}

#[derive(serde::Deserialize)]
struct UndoJournalHeader {
    version: u32,
    library_root: String,
}

struct UndoJournalScan {
    version: Option<u32>,
    library_root: Option<PathBuf>,
    spans: Vec<(u64, u32)>,
}

struct ShortcutUndoManifestScan {
    file: File,
    header: ShortcutUndoHeader,
    spans: Vec<(u64, u32)>,
}

struct UndoJournalIter {
    lines: Lines<BufReader<File>>,
}

struct ShortcutUndoManifest {
    file: File,
    len: u64,
    path: PathBuf,
    token: String,
    staging_dir: PathBuf,
    staging_dir_identity: crate::platform::FileIdentity,
    committed_entries: usize,
}

impl ShortcutUndoManifest {
    fn create(dir: &Path, library_root: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir)
            .with_context(|| format!("creating shortcut undo directory {}", dir.display()))?;
        let token = uuid::Uuid::new_v4().to_string();
        let staging_base = library_root.join(".fileid-restructure-shortcut-staging");
        std::fs::create_dir_all(&staging_base).with_context(|| {
            format!(
                "creating shortcut staging directory {}",
                staging_base.display()
            )
        })?;
        anyhow::ensure!(
            ensure_inside_root(&staging_base, library_root).is_ok()
                && !has_reparse_point_in_chain(&staging_base, library_root)
                && std::fs::symlink_metadata(&staging_base)
                    .is_ok_and(|metadata| metadata.file_type().is_dir()),
            "shortcut staging base is not a safe directory inside the selected library root"
        );
        let staging_dir = staging_base.join(&token);
        std::fs::create_dir(&staging_dir).with_context(|| {
            format!(
                "creating token-owned shortcut staging directory {}",
                staging_dir.display()
            )
        })?;
        let staging_dir_identity = crate::platform::file_identity(&staging_dir)
            .context("reading shortcut staging directory identity")?;
        let path = shortcut_manifest_path(dir, &token)?;
        let opened = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        let mut file = match opened {
            Ok(file) => file,
            Err(error) => {
                let _ = std::fs::remove_dir(&staging_dir);
                return Err(error)
                    .with_context(|| format!("creating shortcut undo manifest {}", path.display()));
            }
        };
        let mut header = serde_json::to_string(&ShortcutUndoHeader {
            version: SHORTCUT_UNDO_MANIFEST_VERSION,
            library_root: library_root.to_string_lossy().into_owned(),
            token: token.clone(),
            staging_dir: Some(staging_dir.to_string_lossy().into_owned()),
            staging_dir_identity: Some(staging_dir_identity),
        })?;
        header.push('\n');
        file.write_all(header.as_bytes())
            .context("writing shortcut undo manifest header")?;
        file.sync_all()
            .context("syncing shortcut undo manifest header")?;
        Ok(Self {
            file,
            len: header.len() as u64,
            path,
            token,
            staging_dir,
            staging_dir_identity,
            committed_entries: 0,
        })
    }

    fn prepare_intent(
        &self,
        file_id: i64,
        source: &str,
        link: &Path,
        source_identity: crate::platform::FileIdentity,
    ) -> Result<PreparedShortcutIntent> {
        anyhow::ensure!(
            crate::platform::file_identity(&self.staging_dir) == Some(self.staging_dir_identity)
                && std::fs::symlink_metadata(&self.staging_dir)
                    .is_ok_and(|metadata| metadata.file_type().is_dir()),
            "shortcut staging directory changed before intent creation"
        );
        let operation_id = uuid::Uuid::new_v4().to_string();
        let staging_link = self.staging_dir.join(format!("{operation_id}.link"));
        let intent_path = self
            .staging_dir
            .join(format!("{operation_id}.intent.json"));
        let intent = ShortcutUndoIntent {
            version: SHORTCUT_UNDO_INTENT_VERSION,
            token: self.token.clone(),
            operation_id,
            file_id,
            source: source.to_string(),
            link: link.to_string_lossy().into_owned(),
            staging_link: staging_link.to_string_lossy().into_owned(),
            source_identity,
            staging_link_identity: None,
        };
        let mut bytes =
            serde_json::to_vec(&intent).context("serializing shortcut creation intent")?;
        bytes.push(b'\n');
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&intent_path)
            .context("creating shortcut creation intent")?;
        file.write_all(&bytes)
            .context("writing shortcut creation intent")?;
        file.sync_all().context("syncing shortcut creation intent")?;
        let identity = crate::platform::file_identity_from_file(&file)
            .context("reading shortcut creation intent identity")?;
        anyhow::ensure!(
            crate::platform::file_identity(&intent_path) == Some(identity)
                && crate::platform::file_identity(&self.staging_dir)
                    == Some(self.staging_dir_identity),
            "shortcut creation intent changed while publishing"
        );
        Ok(PreparedShortcutIntent {
            intent,
            path: intent_path,
            identity,
        })
    }

    fn record_staged_identity(
        &self,
        prepared: &mut PreparedShortcutIntent,
        link_identity: crate::platform::FileIdentity,
    ) -> Result<()> {
        anyhow::ensure!(
            prepared.path.parent() == Some(self.staging_dir.as_path())
                && crate::platform::file_identity(&prepared.path) == Some(prepared.identity)
                && crate::platform::file_identity(&self.staging_dir)
                    == Some(self.staging_dir_identity)
                && shortcut_link_identity(Path::new(&prepared.intent.staging_link))?
                    == link_identity,
            "shortcut creation intent changed before recording the staged identity"
        );
        prepared.intent.staging_link_identity = Some(link_identity);
        let mut bytes =
            serde_json::to_vec(&prepared.intent).context("serializing staged shortcut intent")?;
        bytes.push(b'\n');
        anyhow::ensure!(
            bytes.len() <= MAX_SHORTCUT_RECORD_BYTES,
            "staged shortcut intent is too large"
        );
        let mut file = OpenOptions::new()
            .write(true)
            .open(&prepared.path)
            .context("opening staged shortcut intent")?;
        anyhow::ensure!(
            crate::platform::file_identity_from_file(&file) == Some(prepared.identity),
            "shortcut creation intent changed while opening for update"
        );
        file.set_len(0)
            .context("truncating staged shortcut intent")?;
        file.write_all(&bytes)
            .context("writing staged shortcut intent")?;
        file.sync_all().context("syncing staged shortcut intent")?;
        anyhow::ensure!(
            crate::platform::file_identity(&prepared.path) == Some(prepared.identity)
                && crate::platform::file_identity(&self.staging_dir)
                    == Some(self.staging_dir_identity),
            "shortcut creation intent changed while recording the staged identity"
        );
        Ok(())
    }

    fn append_committed(
        &mut self,
        file_id: i64,
        source: &str,
        link: &Path,
        staging_link: Option<&Path>,
        source_identity: crate::platform::FileIdentity,
        link_identity: crate::platform::FileIdentity,
    ) -> Result<u64> {
        let previous = self.len;
        let mut line = serde_json::to_string(&ShortcutUndoEntry {
            file_id,
            source: source.to_string(),
            link: link.to_string_lossy().into_owned(),
            staging_link: staging_link.map(|path| path.to_string_lossy().into_owned()),
            source_identity,
            link_identity,
        })?;
        line.push('\n');
        let append = (|| -> Result<()> {
            self.file
                .write_all(line.as_bytes())
                .context("appending committed shortcut undo entry")?;
            self.file
                .sync_data()
                .context("syncing committed shortcut undo entry")
        })();
        if let Err(error) = append {
            self.rollback_to(previous);
            return Err(error);
        }
        self.len = previous + line.len() as u64;
        self.committed_entries += 1;
        Ok(previous)
    }

    fn rollback_committed(&mut self, previous: u64) {
        self.rollback_to(previous);
        self.committed_entries = self.committed_entries.saturating_sub(1);
    }

    fn complete_intent(&self, prepared: &PreparedShortcutIntent) -> Result<()> {
        anyhow::ensure!(
            prepared.path.parent() == Some(self.staging_dir.as_path())
                && crate::platform::file_identity(&prepared.path) == Some(prepared.identity)
                && crate::platform::file_identity(&self.staging_dir)
                    == Some(self.staging_dir_identity),
            "shortcut creation intent changed before completion"
        );
        std::fs::remove_file(&prepared.path)
            .context("removing completed shortcut creation intent")
    }

    fn rollback_to(&mut self, previous: u64) {
        use std::io::Seek as _;
        let _ = self.file.set_len(previous);
        let _ = self.file.seek(std::io::SeekFrom::Start(previous));
        let _ = self.file.sync_data();
        self.len = previous;
    }

    fn finish(self) -> Option<String> {
        let Self {
            file,
            path,
            token,
            staging_dir,
            committed_entries,
            ..
        } = self;
        let staging_has_work = std::fs::read_dir(&staging_dir)
            .ok()
            .is_some_and(|mut entries| entries.next().is_some());
        if !staging_has_work {
            let staging_base = staging_dir.parent().map(Path::to_path_buf);
            let _ = std::fs::remove_dir(&staging_dir);
            if let Some(staging_base) = staging_base {
                let _ = std::fs::remove_dir(staging_base);
            }
        }
        if committed_entries == 0 && !staging_has_work {
            drop(file);
            let _ = std::fs::remove_file(path);
            None
        } else {
            let _ = file.sync_all();
            Some(token)
        }
    }
}

impl Iterator for UndoJournalIter {
    type Item = Result<UndoEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        self.lines.next().map(|line| {
            let line = line.context("reading restructure undo journal")?;
            serde_json::from_str(&line).context("parsing restructure undo journal entry")
        })
    }
}

/// Write-ahead undo journal: every inverse entry is appended, flushed, and
/// fsync'd BEFORE the move it describes executes, and rolled back to the prior
/// offset if that move then fails. The journal therefore never claims a move
/// that didn't happen and never misses one that did — closing the two crash
/// windows the previous write-behind (fsync-every-500) design left open.
/// Mirrors the macOS engine's journal discipline. (audit 2026-07-14)
struct UndoJournal {
    file: File,
    len: u64,
    path: PathBuf,
    prior_backup: Option<PathBuf>,
    first_move_committed: bool,
    committed: bool,
}

impl UndoJournal {
    /// Preserve the previous journal until the first move commits, while making
    /// the new write-ahead entry durable before that move starts.
    fn open_replacing(path: Option<PathBuf>, library_root: &Path) -> Result<UndoJournal> {
        let path = path.context("no undo journal location available")?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("creating undo journal dir {}", dir.display()))?;
        }
        recover_prior_undo_journal(&path, library_root)?;
        let prior_backup = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.file_type().is_file(),
                    "undo journal path is not a regular file"
                );
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("restructure_undo.ndjson");
                let backup =
                    path.with_file_name(format!(".{name}.prior-{}", uuid::Uuid::new_v4()));
                std::fs::rename(&path, &backup)
                    .with_context(|| format!("preserving prior undo journal {}", path.display()))?;
                Some(backup)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspecting undo journal {}", path.display()));
            }
        };
        let opened = (|| -> Result<(File, u64)> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .with_context(|| format!("opening undo journal {}", path.display()))?;
            let mut header = serde_json::json!({
                "version": UNDO_JOURNAL_VERSION,
                "library_root": library_root.to_string_lossy()
            })
            .to_string();
            header.push('\n');
            file.write_all(header.as_bytes())
                .context("writing undo journal header")?;
            file.sync_all()
                .with_context(|| format!("syncing new undo journal {}", path.display()))?;
            Ok((file, header.len() as u64))
        })();
        match opened {
            Ok((file, len)) => Ok(UndoJournal {
                file,
                len,
                path,
                prior_backup,
                first_move_committed: false,
                committed: false,
            }),
            Err(error) => {
                let _ = std::fs::remove_file(&path);
                if let Some(backup) = prior_backup {
                    let _ = std::fs::rename(backup, &path);
                }
                Err(error)
            }
        }
    }

    fn commit_replacement(&mut self) -> Result<()> {
        if self.committed {
            return Ok(());
        }
        self.first_move_committed = true;
        if let Some(backup) = self.prior_backup.as_ref() {
            std::fs::remove_file(backup).with_context(|| {
                format!("removing preserved prior undo journal {}", backup.display())
            })?;
            self.prior_backup = None;
        }
        self.committed = true;
        Ok(())
    }

    fn restore_prior_if_uncommitted(self) {
        if self.committed || self.first_move_committed {
            return;
        }
        let UndoJournal {
            file,
            path,
            prior_backup,
            ..
        } = self;
        drop(file);
        let _ = std::fs::remove_file(&path);
        if let Some(backup) = prior_backup {
            if let Err(error) = std::fs::rename(&backup, &path) {
                tracing::error!(
                    ?error,
                    "[RESTRUCTURE] could not restore preserved prior undo journal"
                );
            }
        }
    }

    /// Durably append one inverse entry; returns the pre-append offset so a
    /// failed move can roll the entry back.
    fn append_ahead(
        &mut self,
        file_id: i64,
        from: &str,
        to: &str,
        source_identity: crate::platform::FileIdentity,
    ) -> Result<u64> {
        let prev = self.len;
        let mut line = serde_json::json!({
            "file_id": file_id,
            "from": from,
            "to": to,
            "source_identity": source_identity
        })
        .to_string();
        line.push('\n');
        self.file
            .write_all(line.as_bytes())
            .context("appending undo journal entry")?;
        self.file.sync_data().context("syncing undo journal entry")?;
        self.len = prev + line.len() as u64;
        Ok(prev)
    }

    /// The move this entry described never happened — truncate it away so undo
    /// can't replay a phantom. Prior entries stay durable. Best-effort: a
    /// failed rollback leaves a phantom entry whose replay stale-skips on the
    /// identity checks.
    fn rollback_to(&mut self, prev: u64) {
        use std::io::Seek as _;
        let _ = self.file.set_len(prev);
        let _ = self.file.seek(std::io::SeekFrom::Start(prev));
        let _ = self.file.sync_data();
        self.len = prev;
    }
}

fn prior_undo_journal_backups(path: &Path) -> Result<Vec<PathBuf>> {
    let Some(parent) = path.parent() else {
        return Ok(Vec::new());
    };
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("undo journal path has no UTF-8 file name")?;
    let prefix = format!(".{name}.prior-");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("listing undo journal directory {}", parent.display()));
        }
    };
    let mut backups = Vec::new();
    for entry in entries {
        let entry = entry.context("reading undo journal directory entry")?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(suffix) = file_name.strip_prefix(&prefix) else {
            continue;
        };
        let parsed = uuid::Uuid::parse_str(suffix)
            .with_context(|| format!("invalid preserved undo journal name {file_name}"))?;
        anyhow::ensure!(
            parsed.to_string() == suffix,
            "preserved undo journal name is not canonical"
        );
        backups.push(entry.path());
    }
    backups.sort();
    Ok(backups)
}

fn validate_owned_undo_scan(scan: &UndoJournalScan, library_root: &Path) -> Result<()> {
    let recorded_root = scan
        .library_root
        .as_ref()
        .context("undo journal predates exact library-root ownership")?;
    let canonical_root = canonicalize_safely(library_root)
        .with_context(|| format!("library root {}", library_root.display()))?;
    let canonical_recorded = canonicalize_safely(recorded_root)
        .with_context(|| format!("recorded library root {}", recorded_root.display()))?;
    anyhow::ensure!(
        paths_equal(
            &canonical_root.to_string_lossy(),
            &canonical_recorded.to_string_lossy()
        ),
        "preserved undo journal belongs to a different library root"
    );
    Ok(())
}

fn regular_file_identity(path: &Path, label: &str) -> Result<crate::platform::FileIdentity> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting {label} {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "{label} is not a regular file"
    );
    crate::platform::file_identity(path)
        .with_context(|| format!("reading {label} identity {}", path.display()))
}

fn read_undo_entry_span(path: &Path, span: (u64, u32)) -> Result<UndoEntry> {
    use std::io::{Read as _, Seek as _};

    let mut file = File::open(path)
        .with_context(|| format!("opening undo journal entry {}", path.display()))?;
    file.seek(std::io::SeekFrom::Start(span.0))
        .context("seeking undo journal recovery entry")?;
    let mut bytes = vec![0u8; span.1 as usize];
    file.read_exact(&mut bytes)
        .context("reading undo journal recovery entry")?;
    serde_json::from_slice(&bytes).context("parsing undo journal recovery entry")
}

fn recover_prior_undo_journal(path: &Path, library_root: &Path) -> Result<()> {
    let backups = prior_undo_journal_backups(path)?;
    if backups.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(
        backups.len() == 1,
        "multiple preserved undo journals make crash recovery ambiguous"
    );
    let backup = &backups[0];
    let backup_identity = regular_file_identity(backup, "preserved undo journal")?;
    let backup_scan = scan_undo_journal_spans(backup)?
        .context("preserved undo journal disappeared during recovery")?;
    validate_owned_undo_scan(&backup_scan, library_root)?;

    let current_scan = scan_undo_journal_spans(path)?;
    let current_identity = if let Some(scan) = current_scan.as_ref() {
        validate_owned_undo_scan(scan, library_root)?;
        Some(regular_file_identity(path, "current undo journal")?)
    } else {
        None
    };

    anyhow::ensure!(
        crate::platform::file_identity(backup) == Some(backup_identity),
        "preserved undo journal changed during recovery"
    );
    if let Some(scan) = current_scan.as_ref().filter(|scan| !scan.spans.is_empty()) {
        anyhow::ensure!(
            scan.version == Some(UNDO_JOURNAL_VERSION),
            "current and preserved undo journals both contain work without recoverable identity evidence"
        );
        let first = read_undo_entry_span(path, scan.spans[0])?;
        let source_identity = first.source_identity.context(
            "current and preserved undo journals both contain work without source identity",
        )?;
        let moved = crate::platform::file_identity(Path::new(&first.from))
            == Some(source_identity);
        let not_moved =
            crate::platform::file_identity(Path::new(&first.to)) == Some(source_identity);
        anyhow::ensure!(
            moved ^ not_moved,
            "current and preserved undo journals both contain work; filesystem state is ambiguous"
        );
        if moved {
            let current_identity =
                current_identity.context("current undo journal disappeared during recovery")?;
            anyhow::ensure!(
                crate::platform::file_identity(path) == Some(current_identity)
                    && crate::platform::file_identity(backup) == Some(backup_identity),
                "undo journal changed during committed replacement recovery"
            );
            std::fs::remove_file(backup).with_context(|| {
                format!(
                    "removing obsolete preserved undo journal {}",
                    backup.display()
                )
            })?;
            return Ok(());
        }
    }
    if let Some(identity) = current_identity {
        anyhow::ensure!(
            crate::platform::file_identity(path) == Some(identity),
            "current undo journal changed during recovery"
        );
        std::fs::remove_file(path)
            .with_context(|| format!("removing empty replacement journal {}", path.display()))?;
    }
    crate::util::rename_no_replace(backup, path)
        .with_context(|| format!("restoring preserved undo journal {}", backup.display()))?;
    anyhow::ensure!(
        crate::platform::file_identity(path) == Some(backup_identity),
        "restored undo journal identity changed during recovery"
    );
    Ok(())
}

/// One forward pass over the journal collecting its root binding and each
/// entry's byte span. Returns None if the journal does not exist.
/// Tolerates exactly a torn TRAILING entry: under write-ahead ordering an
/// entry is fsync'd before its move starts, so a torn tail means that move
/// never executed and skipping it is safe. Corruption anywhere earlier fails
/// closed — an explicit error beats a partial undo that reorders dependents.
fn scan_undo_journal_spans(path: &Path) -> Result<Option<UndoJournalScan>> {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("opening undo journal {}", path.display()))
        }
    };
    let mut reader = BufReader::new(file);
    let mut spans: Vec<(u64, u32)> = Vec::new();
    let mut version = None;
    let mut library_root: Option<PathBuf> = None;
    let mut offset = 0u64;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        buf.clear();
        let n = std::io::BufRead::read_until(&mut reader, b'\n', &mut buf)
            .context("reading undo journal")?;
        if n == 0 {
            break;
        }
        let had_newline = buf.last() == Some(&b'\n');
        let body_len = if had_newline { n - 1 } else { n };
        if offset == 0 {
            if let Ok(header) = serde_json::from_slice::<UndoJournalHeader>(&buf[..body_len]) {
                if !matches!(header.version, 2 | UNDO_JOURNAL_VERSION) {
                    anyhow::bail!("unsupported undo journal version {}", header.version);
                }
                if header.library_root.trim().is_empty() {
                    anyhow::bail!("undo journal has an empty library root");
                }
                version = Some(header.version);
                library_root = Some(PathBuf::from(header.library_root));
                offset += n as u64;
                if !had_newline {
                    break;
                }
                continue;
            }
        }
        let parses = serde_json::from_slice::<UndoEntry>(&buf[..body_len]).is_ok();
        if parses {
            if !had_newline {
                tracing::warn!(
                    offset,
                    "[RESTRUCTURE] dropping unterminated trailing undo entry"
                );
                break;
            }
            spans.push((offset, u32::try_from(body_len).context("journal entry too large")?));
            offset += n as u64;
        } else {
            // Only a torn FINAL entry is acceptable.
            let mut probe = [0u8; 1];
            let at_eof = std::io::Read::read(&mut reader, &mut probe)
                .context("probing undo journal tail")?
                == 0;
            if at_eof && !had_newline {
                tracing::warn!(
                    offset,
                    "[RESTRUCTURE] dropping torn trailing undo entry (its move never executed)"
                );
                break;
            }
            anyhow::bail!(
                "undo journal corrupt at byte {offset}: refusing a partial undo of {} valid entries",
                spans.len()
            );
        }
    }
    Ok(Some(UndoJournalScan {
        version,
        library_root,
        spans,
    }))
}

fn scan_shortcut_undo_manifest(path: &Path) -> Result<Option<ShortcutUndoManifestScan>> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspecting shortcut undo manifest {}", path.display()));
        }
    };
    anyhow::ensure!(
        metadata.file_type().is_file(),
        "shortcut undo manifest is not a regular file"
    );
    let expected_identity = crate::platform::file_identity(path)
        .context("reading shortcut undo manifest identity")?;
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) => {
            return Err(error)
                .with_context(|| format!("opening shortcut undo manifest {}", path.display()));
        }
    };
    anyhow::ensure!(
        crate::platform::file_identity_from_file(&file) == Some(expected_identity),
        "shortcut undo manifest changed while opening"
    );
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let header_len = read_bounded_shortcut_line(
        &mut reader,
        &mut buffer,
        "reading shortcut undo manifest header",
    )?;
    anyhow::ensure!(
        header_len > 0 && buffer.last() == Some(&b'\n'),
        "shortcut undo manifest has a truncated header"
    );
    let header: ShortcutUndoHeader = serde_json::from_slice(&buffer[..header_len - 1])
        .context("parsing shortcut undo manifest header")?;
    anyhow::ensure!(
        matches!(header.version, 2 | SHORTCUT_UNDO_MANIFEST_VERSION),
        "unsupported shortcut undo manifest version {}",
        header.version
    );

    let mut spans = Vec::new();
    let mut offset = header_len as u64;
    loop {
        let length = read_bounded_shortcut_line(
            &mut reader,
            &mut buffer,
            "reading shortcut undo manifest",
        )?;
        if length == 0 {
            break;
        }
        let had_newline = buffer.last() == Some(&b'\n');
        let body_len = if had_newline { length - 1 } else { length };
        if !had_newline {
            tracing::warn!(
                offset,
                "[RESTRUCTURE] dropping uncommitted trailing shortcut undo entry"
            );
            break;
        }
        if serde_json::from_slice::<ShortcutUndoEntry>(&buffer[..body_len]).is_ok() {
            spans.push((
                offset,
                u32::try_from(body_len).context("shortcut undo entry too large")?,
            ));
            offset += length as u64;
            continue;
        }
        anyhow::bail!(
            "shortcut undo manifest corrupt at byte {offset}: refusing partial cleanup"
        );
    }
    Ok(Some(ShortcutUndoManifestScan {
        file: reader.into_inner(),
        header,
        spans,
    }))
}

fn read_bounded_shortcut_line<R: BufRead>(
    reader: &mut R,
    buffer: &mut Vec<u8>,
    context: &'static str,
) -> Result<usize> {
    use std::io::Read as _;

    buffer.clear();
    let mut bounded = (&mut *reader).take((MAX_SHORTCUT_RECORD_BYTES + 1) as u64);
    let length = std::io::BufRead::read_until(&mut bounded, b'\n', buffer).context(context)?;
    anyhow::ensure!(
        length <= MAX_SHORTCUT_RECORD_BYTES,
        "shortcut undo record exceeds the {} byte limit",
        MAX_SHORTCUT_RECORD_BYTES
    );
    Ok(length)
}

fn read_shortcut_entry_at(
    file: &mut File,
    start: u64,
    len: u32,
) -> Result<ShortcutUndoEntry> {
    use std::io::{Read as _, Seek as _};

    anyhow::ensure!(
        len as usize <= MAX_SHORTCUT_RECORD_BYTES,
        "shortcut undo entry exceeds the bounded record limit"
    );
    let mut bytes = vec![0u8; len as usize];
    file.seek(std::io::SeekFrom::Start(start))
        .context("seeking shortcut undo entry")?;
    file.read_exact(&mut bytes)
        .context("reading shortcut undo entry")?;
    serde_json::from_slice(&bytes).context("parsing shortcut undo entry")
}

fn read_shortcut_intent(
    path: &Path,
    expected_operation_id: &str,
    expected_staging_dir: &Path,
    expected_token: &str,
    canonical_root: &Path,
) -> Result<ScannedShortcutIntent> {
    use std::io::Read as _;

    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("inspecting shortcut intent {}", path.display()))?;
    anyhow::ensure!(
        metadata.file_type().is_file()
            && metadata.len() <= u64::try_from(MAX_SHORTCUT_RECORD_BYTES).unwrap_or(u64::MAX),
        "shortcut intent is not a bounded regular file"
    );
    let expected_identity =
        crate::platform::file_identity(path).context("reading shortcut intent identity")?;
    let mut file = File::open(path).context("opening shortcut intent")?;
    anyhow::ensure!(
        crate::platform::file_identity_from_file(&file) == Some(expected_identity),
        "shortcut intent changed while opening"
    );
    let mut bytes = Vec::new();
    (&mut file)
        .take((MAX_SHORTCUT_RECORD_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .context("reading shortcut intent")?;
    anyhow::ensure!(
        !bytes.is_empty()
            && bytes.len() <= MAX_SHORTCUT_RECORD_BYTES
            && bytes.last() == Some(&b'\n'),
        "shortcut intent is truncated or oversized"
    );
    let intent: ShortcutUndoIntent =
        serde_json::from_slice(&bytes[..bytes.len() - 1]).context("parsing shortcut intent")?;
    let operation_id = uuid::Uuid::parse_str(&intent.operation_id)
        .context("shortcut intent has an invalid operation id")?;
    anyhow::ensure!(
        operation_id.to_string() == intent.operation_id
            && intent.operation_id == expected_operation_id
            && intent.version == SHORTCUT_UNDO_INTENT_VERSION
            && intent.token == expected_token
            && intent.file_id > 0
            && !intent.source.trim().is_empty()
            && !intent.link.trim().is_empty(),
        "shortcut intent fields do not match its recovery context"
    );
    let expected_staging_link =
        expected_staging_dir.join(format!("{}.link", intent.operation_id));
    anyhow::ensure!(
        paths_equal(
            &intent.staging_link,
            &expected_staging_link.to_string_lossy()
        ),
        "shortcut intent staging path does not match its operation id"
    );
    let final_link = Path::new(&intent.link);
    let final_parent = final_link
        .parent()
        .context("shortcut intent destination has no parent")?;
    anyhow::ensure!(
        ensure_inside_root(final_parent, canonical_root).is_ok()
            && !has_reparse_point_in_chain(final_parent, canonical_root),
        "shortcut intent destination is not safely contained by the selected library root"
    );
    anyhow::ensure!(
        crate::platform::file_identity(path) == Some(expected_identity),
        "shortcut intent changed while reading"
    );
    Ok(ScannedShortcutIntent {
        intent,
        path: path.to_path_buf(),
        identity: expected_identity,
        committed: false,
    })
}

fn shortcut_staging_context(
    header: &ShortcutUndoHeader,
    token: &str,
    canonical_root: &Path,
) -> Result<Option<(PathBuf, crate::platform::FileIdentity)>> {
    if header.version == 2 {
        return Ok(None);
    }
    let staging_dir = PathBuf::from(
        header
            .staging_dir
            .as_deref()
            .context("v3 shortcut manifest is missing its staging directory")?,
    );
    let staging_identity = header
        .staging_dir_identity
        .context("v3 shortcut manifest is missing its staging directory identity")?;
    let expected = canonical_root
        .join(".fileid-restructure-shortcut-staging")
        .join(token);
    anyhow::ensure!(
        paths_equal(&staging_dir.to_string_lossy(), &expected.to_string_lossy()),
        "shortcut staging directory does not match the manifest token and library root"
    );
    match std::fs::symlink_metadata(&staging_dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspecting shortcut staging directory"),
        Ok(metadata) => {
            anyhow::ensure!(
                metadata.file_type().is_dir()
                    && ensure_inside_root(&staging_dir, canonical_root).is_ok()
                    && !has_reparse_point_in_chain(&staging_dir, canonical_root)
                    && crate::platform::file_identity(&staging_dir) == Some(staging_identity),
                "shortcut staging directory was replaced or is unsafe"
            );
        }
    }
    Ok(Some((staging_dir, staging_identity)))
}

fn recover_shortcut_intents(
    scan: &ShortcutUndoManifestScan,
    token: &str,
    canonical_root: &Path,
) -> Result<u32> {
    let Some((staging_dir, staging_identity)) =
        shortcut_staging_context(&scan.header, token, canonical_root)?
    else {
        return Ok(0);
    };
    let mut intent_paths = Vec::new();
    let mut staged_operation_ids = HashSet::new();
    for (index, entry) in std::fs::read_dir(&staging_dir)
        .context("enumerating shortcut staging directory")?
        .enumerate()
    {
        anyhow::ensure!(
            index < MAX_SHORTCUT_STAGING_ENTRIES,
            "shortcut staging directory exceeds the {} entry limit",
            MAX_SHORTCUT_STAGING_ENTRIES
        );
        let entry = entry.context("reading shortcut staging entry")?;
        let name = entry
            .file_name()
            .to_str()
            .context("shortcut staging entry has a non-Unicode name")?
            .to_string();
        if let Some(operation_id) = name.strip_suffix(".intent.json") {
            let parsed = uuid::Uuid::parse_str(operation_id)
                .context("shortcut intent filename is not a UUID")?;
            anyhow::ensure!(
                parsed.to_string() == operation_id,
                "shortcut intent filename is not a canonical UUID"
            );
            intent_paths.push((operation_id.to_string(), entry.path()));
        } else if let Some(operation_id) = name.strip_suffix(".link") {
            let parsed = uuid::Uuid::parse_str(operation_id)
                .context("staged shortcut filename is not a UUID")?;
            anyhow::ensure!(
                parsed.to_string() == operation_id,
                "staged shortcut filename is not a canonical UUID"
            );
            staged_operation_ids.insert(operation_id.to_string());
        } else {
            anyhow::bail!("shortcut staging directory contains an unexpected entry");
        }
    }

    let mut intents = Vec::with_capacity(intent_paths.len());
    let mut intent_operation_ids = HashSet::new();
    let mut intent_by_staging_key = HashMap::new();
    for (operation_id, path) in intent_paths {
        let intent =
            read_shortcut_intent(&path, &operation_id, &staging_dir, token, canonical_root)?;
        anyhow::ensure!(
            intent_operation_ids.insert(operation_id),
            "shortcut staging directory contains a duplicate intent"
        );
        let key = claimed_destination_key(Path::new(&intent.intent.staging_link));
        anyhow::ensure!(
            intent_by_staging_key.insert(key, intents.len()).is_none(),
            "shortcut intents claim the same staged path"
        );
        intents.push(intent);
    }
    let mut manifest_file = scan
        .file
        .try_clone()
        .context("cloning shortcut manifest for intent recovery")?;
    let mut committed_staged_operation_ids = HashSet::new();
    for &(start, len) in &scan.spans {
        let entry = read_shortcut_entry_at(&mut manifest_file, start, len)?;
        let Some(staging_link) = entry.staging_link.as_deref() else {
            continue;
        };
        let staging_path = Path::new(staging_link);
        let operation_id = staging_path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".link"))
            .context("committed staged shortcut has an invalid filename")?;
        let parsed = uuid::Uuid::parse_str(operation_id)
            .context("committed staged shortcut filename is not a UUID")?;
        anyhow::ensure!(
            parsed.to_string() == operation_id
                && staging_path.parent() == Some(staging_dir.as_path())
                && paths_equal(staging_link, &staging_dir.join(format!("{operation_id}.link")).to_string_lossy())
                && committed_staged_operation_ids.insert(operation_id.to_string()),
            "committed staged shortcut path is duplicated or outside its token-owned directory"
        );
        let key = claimed_destination_key(Path::new(staging_link));
        let Some(&intent_index) = intent_by_staging_key.get(&key) else {
            continue;
        };
        let intent = &mut intents[intent_index];
        anyhow::ensure!(
            paths_equal(staging_link, &intent.intent.staging_link)
                && entry.file_id == intent.intent.file_id
                && paths_equal(&entry.source, &intent.intent.source)
                && paths_equal(&entry.link, &intent.intent.link)
                && entry.source_identity == intent.intent.source_identity
                && intent.intent.staging_link_identity == Some(entry.link_identity),
            "committed shortcut entry does not match its pending intent"
        );
        anyhow::ensure!(
            !intent.committed,
            "shortcut manifest contains duplicate committed staging entries"
        );
        intent.committed = true;
    }
    anyhow::ensure!(
        staged_operation_ids
            .iter()
            .all(|operation_id| {
                intent_operation_ids.contains(operation_id)
                    || committed_staged_operation_ids.contains(operation_id)
            }),
        "shortcut staging directory contains a staged link without durable recovery evidence"
    );

    let mut recovered = 0u32;
    for intent in intents {
        anyhow::ensure!(
            crate::platform::file_identity(&staging_dir) == Some(staging_identity),
            "shortcut staging directory changed during recovery"
        );
        let staged_link = Path::new(&intent.intent.staging_link);
        if !intent.committed {
            match std::fs::symlink_metadata(staged_link) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error).context("inspecting pending staged shortcut"),
                Ok(_) => {
                    let expected_link = intent.intent.staging_link_identity.context(
                        "pending staged shortcut has no durably recorded link identity",
                    )?;
                    remove_recorded_shortcut(
                        staged_link,
                        Path::new(&intent.intent.source),
                        intent.intent.source_identity,
                        expected_link,
                        canonical_root,
                    )
                    .context("removing pending staged shortcut")?;
                }
            }
        }
        anyhow::ensure!(
            crate::platform::file_identity(&intent.path) == Some(intent.identity)
                && crate::platform::file_identity(&staging_dir) == Some(staging_identity),
            "shortcut intent changed during recovery"
        );
        std::fs::remove_file(&intent.path).context("removing recovered shortcut intent")?;
        recovered = recovered.saturating_add(1);
    }
    Ok(recovered)
}

fn cleanup_shortcut_staging_dir(
    header: &ShortcutUndoHeader,
    token: &str,
    canonical_root: &Path,
) -> Result<()> {
    let Some((staging_dir, staging_identity)) =
        shortcut_staging_context(header, token, canonical_root)?
    else {
        return Ok(());
    };
    anyhow::ensure!(
        std::fs::read_dir(&staging_dir)
            .context("checking completed shortcut staging directory")?
            .next()
            .is_none()
            && crate::platform::file_identity(&staging_dir) == Some(staging_identity),
        "completed shortcut staging directory still contains recovery evidence"
    );
    std::fs::remove_dir(&staging_dir).context("removing completed shortcut staging directory")?;
    if let Some(staging_base) = staging_dir.parent() {
        let _ = std::fs::remove_dir(staging_base);
    }
    Ok(())
}

/// Streams journal entries NEWEST-FIRST via pre-scanned byte spans. Dependent
/// moves (A→X then B→A) must be restored newest-first or the older inverse
/// (X→A) finds A occupied by B and uniquifies into "A (2)" — silent
/// corruption. Holds only the span table, never the journal contents.
struct ReverseUndoIter {
    file: File,
    spans: Vec<(u64, u32)>,
    next: usize,
}

impl Iterator for ReverseUndoIter {
    type Item = Result<RestructureMove>;

    fn next(&mut self) -> Option<Self::Item> {
        use std::io::{Read as _, Seek as _};
        if self.next == 0 {
            return None;
        }
        self.next -= 1;
        let (start, len) = self.spans[self.next];
        let mut buf = vec![0u8; len as usize];
        let entry = (|| -> Result<RestructureMove> {
            self.file
                .seek(std::io::SeekFrom::Start(start))
                .context("seeking undo journal entry")?;
            self.file
                .read_exact(&mut buf)
                .context("reading undo journal entry")?;
            let e: UndoEntry = serde_json::from_slice(&buf)
                .context("parsing restructure undo journal entry")?;
            Ok(RestructureMove {
                file_id: e.file_id,
                source: e.from,
                destination: e.to,
                category: String::new(),
                tier: None,
                confidence: String::new(),
                reason: None,
            })
        })();
        Some(entry)
    }
}

fn claimed_destination_key(path: &Path) -> ClaimedDestination {
    let folded = path.to_string_lossy().to_lowercase();
    let digest = blake3::hash(folded.as_bytes());
    let mut key = [0_u8; 16];
    key.copy_from_slice(&digest.as_bytes()[..16]);
    key
}

struct ReverseShortcutUndoIter {
    file: File,
    spans: Vec<(u64, u32)>,
    next: usize,
}

impl Iterator for ReverseShortcutUndoIter {
    type Item = Result<ShortcutUndoEntry>;

    fn next(&mut self) -> Option<Self::Item> {
        use std::io::{Read as _, Seek as _};
        if self.next == 0 {
            return None;
        }
        self.next -= 1;
        let (start, len) = self.spans[self.next];
        let mut buffer = vec![0u8; len as usize];
        let entry = (|| -> Result<ShortcutUndoEntry> {
            self.file
                .seek(std::io::SeekFrom::Start(start))
                .context("seeking shortcut undo entry")?;
            self.file
                .read_exact(&mut buffer)
                .context("reading shortcut undo entry")?;
            serde_json::from_slice(&buffer).context("parsing shortcut undo entry")
        })();
        Some(entry)
    }
}

fn shortcut_manifest_path(dir: &Path, token: &str) -> Result<PathBuf> {
    let parsed = uuid::Uuid::parse_str(token).context("invalid shortcut undo token")?;
    anyhow::ensure!(
        parsed.to_string() == token,
        "shortcut undo token must use canonical UUID form"
    );
    Ok(dir.join(format!("{token}.ndjson")))
}

fn shortcut_receipt_path(dir: &Path, token: &str) -> Result<PathBuf> {
    shortcut_manifest_path(dir, token)?;
    Ok(dir.join(format!("{token}.complete.json")))
}

fn read_shortcut_undo_receipt(path: &Path) -> Result<Option<ShortcutUndoReceipt>> {
    use std::io::Read as _;

    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading shortcut undo receipt metadata"),
    };
    anyhow::ensure!(
        metadata.file_type().is_file() && metadata.len() <= 64 * 1024,
        "shortcut undo receipt must be a bounded regular file"
    );
    let expected_identity = crate::platform::file_identity(path)
        .context("reading shortcut undo receipt identity")?;
    let mut file = File::open(path).context("opening shortcut undo receipt")?;
    anyhow::ensure!(
        crate::platform::file_identity_from_file(&file) == Some(expected_identity),
        "shortcut undo receipt changed while opening"
    );
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.read_to_end(&mut bytes)
        .context("reading shortcut undo receipt")?;
    anyhow::ensure!(
        bytes.last() == Some(&b'\n')
            && crate::platform::file_identity(path) == Some(expected_identity),
        "shortcut undo receipt is truncated or changed"
    );
    let receipt = serde_json::from_slice::<ShortcutUndoReceipt>(&bytes[..bytes.len() - 1])
        .context("parsing shortcut undo receipt")?;
    anyhow::ensure!(
        receipt.version == SHORTCUT_UNDO_RECEIPT_VERSION,
        "unsupported shortcut undo receipt version {}",
        receipt.version
    );
    Ok(Some(receipt))
}

fn write_shortcut_undo_receipt(
    dir: &Path,
    receipt: &ShortcutUndoReceipt,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating shortcut undo directory {}", dir.display()))?;
    let path = shortcut_receipt_path(dir, &receipt.token)?;
    if let Some(existing) = read_shortcut_undo_receipt(&path)? {
        anyhow::ensure!(
            existing == *receipt,
            "shortcut undo receipt conflicts with the completed operation"
        );
        return Ok(path);
    }

    let temp = dir.join(format!(
        ".{}.{}.receipt.tmp",
        receipt.token,
        uuid::Uuid::new_v4()
    ));
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .context("creating temporary shortcut undo receipt")?;
        let mut bytes = serde_json::to_vec(receipt).context("serializing shortcut undo receipt")?;
        bytes.push(b'\n');
        file.write_all(&bytes)
            .context("writing temporary shortcut undo receipt")?;
        file.sync_all()
            .context("syncing temporary shortcut undo receipt")?;
        drop(file);
        crate::util::rename_no_replace(&temp, &path)
            .context("publishing shortcut undo receipt")?;
        let published = File::open(&path).context("opening published shortcut undo receipt")?;
        published
            .sync_all()
            .context("syncing published shortcut undo receipt")
    })();
    if let Err(error) = write_result {
        let _ = std::fs::remove_file(&temp);
        if let Some(existing) = read_shortcut_undo_receipt(&path)? {
            anyhow::ensure!(
                existing == *receipt,
                "shortcut undo receipt conflicts with the completed operation"
            );
            return Ok(path);
        }
        return Err(error);
    }
    anyhow::ensure!(
        read_shortcut_undo_receipt(&path)?.as_ref() == Some(receipt),
        "published shortcut undo receipt did not validate"
    );
    Ok(path)
}

#[derive(Default)]
pub(crate) struct DestinationClaims {
    claimed: HashSet<ClaimedDestination>,
    next_suffix: HashMap<ClaimedDestination, u64>,
}

#[derive(Default)]
struct ExistingShortcutIndex {
    scanned_parents: HashSet<ClaimedDestination>,
    by_target: HashMap<(ClaimedDestination, ClaimedDestination), PathBuf>,
}

impl ExistingShortcutIndex {
    fn find(
        &mut self,
        source: &Path,
        parent: &Path,
        expected_source: crate::platform::FileIdentity,
    ) -> Result<Option<PathBuf>> {
        anyhow::ensure!(
            crate::platform::file_identity(source) == Some(expected_source),
            "shortcut source changed during existing-link inspection"
        );
        let canonical_parent = canonicalize_safely(parent)?;
        let parent_key = claimed_destination_key(&canonical_parent);
        if self.scanned_parents.insert(parent_key) {
            for entry in
                std::fs::read_dir(crate::util::path_safety::to_extended_length(parent))?.flatten()
            {
                let path = entry.path();
                if !std::fs::symlink_metadata(&path)
                    .is_ok_and(|metadata| metadata.file_type().is_symlink())
                {
                    continue;
                }
                let Ok(target) = resolved_symlink_target(&path) else {
                    continue;
                };
                let Ok(canonical_target) = canonicalize_safely(&target) else {
                    continue;
                };
                self.by_target.insert(
                    (parent_key, claimed_destination_key(&canonical_target)),
                    path,
                );
            }
        }
        let canonical_source = canonicalize_safely(source)?;
        Ok(self
            .by_target
            .get(&(parent_key, claimed_destination_key(&canonical_source)))
            .cloned())
    }
}

impl DestinationClaims {
    pub(crate) fn reserve(&mut self, destination: &Path) -> Result<PathBuf> {
        let family = claimed_destination_key(destination);
        let start_suffix = self.next_suffix.get(&family).copied().unwrap_or(2);
        let (reserved, next_suffix) =
            unique_destination_from(destination, &self.claimed, start_suffix)?;
        if reserved != destination {
            self.next_suffix.insert(family, next_suffix);
        }
        self.claimed.insert(claimed_destination_key(&reserved));
        Ok(reserved)
    }

    fn release(&mut self, destination: &Path) {
        self.claimed
            .remove(&claimed_destination_key(destination));
    }
}

#[cfg(windows)]
use windows::core::PCWSTR;
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    CreateSymbolicLinkW, SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE, SYMBOLIC_LINK_FLAGS,
};

pub struct RestructureApply {
    db_conn: Arc<Mutex<Connection>>,
    library_root: PathBuf,
    use_symlinks: bool,
    strict_destinations: bool,
    // F-C6-013: cooperative cancel polled between moves. Defaults to a fresh,
    // never-set flag; the dispatcher injects an operation-specific flag via `with_cancel` so
    // a user "stop" aborts a 100k-move apply between moves (each completed move
    // is already durable, so stopping mid-batch preserves per-move atomicity).
    cancel: Arc<AtomicBool>,
    // Test seam: journal location override so concurrent tests never share (or
    // clobber) the real user journal. None → the app-data location.
    undo_journal_override: Option<PathBuf>,
    shortcut_undo_dir_override: Option<PathBuf>,
    #[cfg(test)]
    fail_next_move_after_journal: AtomicBool,
    #[cfg(test)]
    fail_next_symlink_post_create: AtomicBool,
    #[cfg(test)]
    fail_next_shortcut_manifest_commit: AtomicBool,
    #[cfg(test)]
    cancel_after_undo_replay: AtomicBool,
}

pub(crate) struct ForwardBatchPreflight<'a> {
    apply: &'a RestructureApply,
    canonical_root: PathBuf,
    file_ids: HashSet<i64>,
    sources: HashSet<ClaimedDestination>,
    destinations: HashSet<ClaimedDestination>,
    destination_claims: DestinationClaims,
}

impl ForwardBatchPreflight<'_> {
    pub(crate) fn is_cancelled(&self) -> bool {
        self.apply.is_cancelled()
    }

    pub(crate) fn validate(&mut self, move_: &RestructureMove) -> Result<()> {
        anyhow::ensure!(move_.file_id > 0, "move file ID must be positive");
        anyhow::ensure!(
            self.file_ids.insert(move_.file_id),
            "plan contains a duplicate file ID"
        );
        anyhow::ensure!(
            !move_.source.is_empty()
                && !move_.destination.is_empty()
                && !move_.source.contains('\0')
                && !move_.destination.contains('\0'),
            "move paths must be nonempty and contain no NUL characters"
        );

        let source = Path::new(&move_.source);
        let destination = Path::new(&move_.destination);
        anyhow::ensure!(
            source.is_absolute() && destination.is_absolute(),
            "move paths must be absolute"
        );
        anyhow::ensure!(
            !paths_equal(&move_.source, &move_.destination),
            "plan contains a no-op move"
        );
        ensure_inside_root(source, &self.canonical_root)
            .context("move source is outside the selected library root")?;
        ensure_inside_root(destination, &self.canonical_root)
            .context("move destination is outside the selected library root")?;

        let source_metadata =
            std::fs::symlink_metadata(crate::util::path_safety::to_extended_length(source))
                .context("planned source is unavailable")?;
        anyhow::ensure!(
            !source_metadata.file_type().is_symlink(),
            "planned source must not be a symbolic link"
        );
        let canonical_source =
            std::fs::canonicalize(source).context("canonicalizing planned source")?;
        anyhow::ensure!(
            self.sources
                .insert(claimed_destination_key(&canonical_source)),
            "plan contains a duplicate source"
        );

        let (db_path, db_ref) = current_identity_in_db(&self.apply.db_conn, move_.file_id)?
            .context("planned file no longer exists in the database")?;
        anyhow::ensure!(
            paths_equal(&db_path, &move_.source),
            "planned source no longer matches the database"
        );
        anyhow::ensure!(
            verified_file_identity(db_ref, source).is_some(),
            "planned source identity changed; rescan and re-plan"
        );

        if let Some(name) = destination.file_name().and_then(|name| name.to_str()) {
            anyhow::ensure!(
                crate::util::path_safety::is_safe_filename(name),
                "planned destination has an invalid filename"
            );
        } else {
            anyhow::bail!("planned destination must name a file");
        }
        let normalized_destination =
            crate::util::path_safety::canonicalize_for_containment(destination);
        anyhow::ensure!(
            self.destinations
                .insert(claimed_destination_key(&normalized_destination)),
            "plan contains a duplicate destination"
        );
        if let Some(parent) = destination.parent() {
            anyhow::ensure!(
                !has_reparse_point_in_chain(parent, &self.canonical_root),
                "planned destination parent contains a reparse point"
            );
        }
        let reserved = self.destination_claims.reserve(destination)?;
        anyhow::ensure!(
            reserved == destination,
            "planned destination is no longer collision-free; re-plan before applying"
        );
        Ok(())
    }
}

impl RestructureApply {
    pub fn new(db_conn: Arc<Mutex<Connection>>, library_root: PathBuf, use_symlinks: bool) -> Self {
        Self {
            db_conn,
            library_root,
            use_symlinks,
            strict_destinations: false,
            cancel: Arc::new(AtomicBool::new(false)),
            undo_journal_override: None,
            shortcut_undo_dir_override: None,
            #[cfg(test)]
            fail_next_move_after_journal: AtomicBool::new(false),
            #[cfg(test)]
            fail_next_symlink_post_create: AtomicBool::new(false),
            #[cfg(test)]
            fail_next_shortcut_manifest_commit: AtomicBool::new(false),
            #[cfg(test)]
            cancel_after_undo_replay: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_undo_journal_path(mut self, path: PathBuf) -> Self {
        self.undo_journal_override = Some(path);
        self
    }

    #[cfg(test)]
    pub(crate) fn with_shortcut_undo_dir(mut self, path: PathBuf) -> Self {
        self.shortcut_undo_dir_override = Some(path);
        self
    }

    fn shortcut_undo_dir(&self) -> Result<PathBuf> {
        match &self.shortcut_undo_dir_override {
            Some(path) => Ok(path.clone()),
            #[cfg(test)]
            None => Ok(self
                .library_root
                .join(".fileid-test-restructure-shortcut-undo")),
            #[cfg(not(test))]
            None => crate::paths::restructure_shortcut_undo_dir(),
        }
    }

    /// Inject a shared cancellation flag. `handle_apply_restructure` passes the
    /// flag that the CancelRestructure dispatch arm sets; `apply` polls it at the top of
    /// each move so a long apply is stoppable. (F-C6-013)
    pub fn with_cancel(mut self, cancel: Arc<AtomicBool>) -> Self {
        self.cancel = cancel;
        self
    }

    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    #[cfg(test)]
    fn with_fail_next_move_after_journal(self) -> Self {
        self.fail_next_move_after_journal.store(true, Ordering::Relaxed);
        self
    }

    #[cfg(test)]
    fn with_fail_next_shortcut_manifest_commit(self) -> Self {
        self.fail_next_shortcut_manifest_commit
            .store(true, Ordering::Relaxed);
        self
    }

    #[cfg(test)]
    fn with_cancel_after_undo_replay(self) -> Self {
        self.cancel_after_undo_replay
            .store(true, Ordering::Relaxed);
        self
    }

    pub(crate) fn with_strict_destinations(mut self) -> Self {
        self.strict_destinations = true;
        self
    }

    pub(crate) fn begin_forward_preflight(&self) -> Result<ForwardBatchPreflight<'_>> {
        anyhow::ensure!(
            self.library_root.is_absolute() && self.library_root.is_dir(),
            "restructure library root must be an existing absolute directory"
        );
        let canonical_root = std::fs::canonicalize(&self.library_root)
            .with_context(|| format!("library root {}", self.library_root.display()))?;
        Ok(ForwardBatchPreflight {
            apply: self,
            canonical_root,
            file_ids: HashSet::new(),
            sources: HashSet::new(),
            destinations: HashSet::new(),
            destination_claims: DestinationClaims::default(),
        })
    }

    /// Apply every proposed move. Stops on first hard error; returns the
    /// applied + failed counts. A privilege error in symlink mode short-
    /// circuits with a friendly message instead of partial writes.
    // The engine package compiles this module once for the library and again
    // for the binary. External library callers and unit tests use the slice
    // convenience method; the binary uses `apply_iter` directly.
    #[allow(dead_code)]
    pub fn apply(&self, moves: &[RestructureMove]) -> Result<RestructureApplyResult> {
        self.apply_iter_with(
            moves.iter().cloned().map(Ok),
            Some(moves.len()),
            true,
        )
    }

    /// Apply a move stream without materializing the complete plan. This is the
    /// million-file path used by the CLI and persisted GUI plans; all per-run
    /// state (undo journal, collision set, cancellation, and feedback) remains
    /// shared across the stream exactly as it is for `apply(&[...])`.
    pub fn apply_iter<I>(
        &self,
        moves: I,
        total_hint: Option<usize>,
    ) -> Result<RestructureApplyResult>
    where
        I: IntoIterator<Item = Result<RestructureMove>>,
    {
        self.apply_iter_with(moves, total_hint, true)
    }

    fn apply_iter_with<I>(
        &self,
        moves: I,
        total_hint: Option<usize>,
        record_undo: bool,
    ) -> Result<RestructureApplyResult>
    where
        I: IntoIterator<Item = Result<RestructureMove>>,
    {
        anyhow::ensure!(
            self.library_root.is_absolute() && self.library_root.is_dir(),
            "restructure library root must be an existing absolute directory"
        );
        let canonical_root = std::fs::canonicalize(&self.library_root)
            .with_context(|| format!("library root {}", self.library_root.display()))?;

        let mut applied = 0u32;
        let mut failed = 0u32;
        // WRITE-AHEAD undo journal (macOS parity, audit 2026-07-14): each
        // inverse entry is appended + fsync'd BEFORE its move executes and
        // rolled back if the move then fails, so the journal never claims a
        // move that didn't happen and never misses one that did. Opened
        // LAZILY at the first journaled move: a batch that never journals
        // (symlink mode, all no-ops) can't truncate the prior run's journal,
        // and an unopenable journal aborts before ANY file moves — undo
        // protection is a precondition now, not best-effort.
        let mut journal: Option<UndoJournal> = None;
        let mut shortcut_manifest: Option<ShortcutUndoManifest> = None;
        // (source, final destination) of every successful real move, fed to the
        // learn-from-corrections memory in ONE lock acquisition after the loop so a
        // future plan can boost a move toward a folder the user has filed here
        // before. Populated alongside the undo journal, so it is forward-applies-only
        // (empty on an undo run, record_undo=false). (R3 → learn-your-style)
        let mut applied_pairs: Vec<(String, PathBuf)> =
            Vec::with_capacity(APPLY_PROGRESS_INTERVAL);
        // B3: destinations claimed earlier in THIS batch, so two distinct
        // sources that map to the same basename don't collide before either
        // touches disk. Keyed by the LOWERCASED path string: NTFS (and APFS)
        // are case-insensitive by default, so "Photo.jpg" and "photo.jpg" name
        // the same file — case-folding the key makes the second move uniquify
        // instead of silently clobbering the first (data loss). Mirrors the
        // `ci_starts_with` full-Unicode fold and the macOS `Restructure.swift`
        // lowercased claimed set, so a library round-trips identically.
        let mut claimed = DestinationClaims::default();
        let mut existing_shortcuts = ExistingShortcutIndex::default();

        // F-C6-013: the apply loop was a silent, unstoppable serial walk — at
        // 100k+ moves the user got no feedback and no stop.
        let total = total_hint.unwrap_or(0);
        let planned = total_hint.map(|count| count as u64);
        let mut processed = 0usize;
        let mut cancelled = false;
        for (idx, m) in moves.into_iter().enumerate() {
            // A failed stream read (corrupt / vanished spooled plan) must NOT
            // discard the partial result via `?`: every move already applied is
            // real and journaled, and an Err reply makes the app report "your
            // files are unchanged" with no Undo affordance. Stop, count the
            // unread remainder as failed, and return the truthful partial.
            let m = match m {
                Ok(m) => m,
                Err(err) => {
                    tracing::warn!(
                        ?err,
                        applied,
                        failed,
                        processed = idx,
                        total,
                        "[RESTRUCTURE] plan stream read failed mid-apply; stopping"
                    );
                    let remainder = total.saturating_sub(idx).max(1);
                    failed = failed.saturating_add(u32::try_from(remainder).unwrap_or(u32::MAX));
                    break;
                }
            };
            // Poll the cancel flag at the TOP of every iteration. Every move
            // already completed is durable (per-move FS op + DB update), so
            // stopping BETWEEN moves is safe and preserves per-move atomicity.
            if self.cancel.load(Ordering::Relaxed) {
                cancelled = true;
                tracing::info!(applied, failed, processed = idx, total, "[RESTRUCTURE] apply cancelled by user");
                break;
            }
            processed = idx + 1;
            if should_emit_apply_progress(processed, total, APPLY_PROGRESS_INTERVAL) {
                tracing::info!(applied, failed, processed, total, "[RESTRUCTURE] apply progress");
            }

            // B4/S6/S7: bind the move to the planned file identity. The
            // payload `source` is not authoritative on its own — re-read the
            // live DB row for `file_id` and require it still names this
            // source. A stale plan (file renamed/moved/replaced since
            // planning) is skipped so we never move the wrong bytes or stamp
            // the row with a path that never held this file.
            let current_identity = current_identity_in_db(&self.db_conn, m.file_id);
            if !record_undo {
                if let Ok(Some((db_path, db_ref))) = &current_identity {
                    let source = Path::new(&m.source);
                    let destination = Path::new(&m.destination);
                    if paths_equal(db_path, &m.source)
                        && !source.try_exists().unwrap_or(false)
                        && verified_file_identity(*db_ref, destination).is_some()
                    {
                        if let Err(err) = update_path_in_db(&self.db_conn, m.file_id, destination) {
                            tracing::error!(
                                ?err,
                                file_id = m.file_id,
                                "[RESTRUCTURE] restored file is present but undo DB reconciliation failed"
                            );
                            record_path_update_failure(
                                m.file_id,
                                &m.source,
                                destination,
                                crate::platform::file_identity(destination),
                                &canonical_root,
                            );
                            failed += 1;
                        }
                        continue;
                    }
                }
            }
            let db_file_ref = match current_identity {
                Ok(Some((db_path, db_ref))) if paths_equal(&db_path, &m.source) => db_ref,
                // A cancelled/partially failed undo is intentionally resumable.
                // Entries already restored by the first attempt remain in the
                // journal; recognize their live DB + on-disk destination as an
                // idempotent success so a retry can finish and clear the journal.
                Ok(Some((db_path, db_ref)))
                    if !record_undo
                        && paths_equal(&db_path, &m.destination)
                        && verified_file_identity(db_ref, Path::new(&m.destination)).is_some() =>
                {
                    continue;
                }
                // Undo fallback on journal evidence: the forward apply can
                // succeed the on-disk move but FAIL update_path_in_db (a live
                // UNIQUE path_text conflict, or a kill in the move→update
                // window). Then the file is physically at the journaled
                // final_dest (= this undo move's `source`) while path_text
                // still names the original (= this undo move's `destination`),
                // so neither DB-derived arm above matches and the file would
                // be stale-skipped and stranded forever. Trust the journal: if
                // the file is physically at `source` and `destination` is
                // free, and the file identity still matches the DB row, move
                // it back. Undo-only (record_undo=false) so a forward apply
                // never takes this path.
                Ok(Some((db_path, db_ref)))
                    if !record_undo
                        && paths_equal(&db_path, &m.destination)
                        && Path::new(&m.source).try_exists().unwrap_or(false)
                        && !Path::new(&m.destination).try_exists().unwrap_or(false)
                        && verified_file_identity(db_ref, Path::new(&m.source)).is_some() =>
                {
                    db_ref
                }
                _ => {
                    tracing::warn!(
                        file_id = m.file_id,
                        "[RESTRUCTURE] skipping stale move: source no longer matches the DB row"
                    );
                    failed += 1;
                    continue;
                }
            };

            let source_path = Path::new(&m.source);
            if record_undo && ensure_inside_root(source_path, &canonical_root).is_err() {
                tracing::warn!(
                    file_id = m.file_id,
                    source=%crate::platform::redact_path_for_log(source_path),
                    "rejecting move whose source is outside the selected library root"
                );
                failed += 1;
                continue;
            }
            if std::fs::symlink_metadata(crate::util::path_safety::to_extended_length(source_path))
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            {
                tracing::warn!(file_id = m.file_id, "[RESTRUCTURE] refusing to move a symbolic-link source");
                failed += 1;
                continue;
            }
            let Some(source_identity) = verified_file_identity(db_file_ref, source_path) else {
                tracing::warn!(
                    file_id = m.file_id,
                    "[RESTRUCTURE] source identity is unavailable or changed; rescan before applying"
                );
                failed += 1;
                continue;
            };

            let dest = PathBuf::from(&m.destination);
            // Path-traversal guard. The destination's parent must exist
            // OR be createable under library_root. Canonicalize the
            // closest existing ancestor and verify containment.
            //
            // D1: skip the per-entry destination check on an UNDO replay
            // (record_undo=false). undo_last validates the complete journal
            // against the caller's canonical library root before any inverse
            // move starts, so repeating the path walk here only widens the
            // per-file TOCTOU surface. Forward, plan-generated destinations
            // still require the check immediately before mutation.
            if record_undo {
                if let Err(err) = ensure_inside_root(&dest, &canonical_root) {
                    tracing::warn!(?err, dest=%crate::platform::redact_path_for_log(&dest), "rejecting move outside library root");
                    failed += 1;
                    continue;
                }
            }

            if let Some(parent) = dest.parent() {
                // SEC-5: TOCTOU defense, pass 1. Check the EXISTING ancestor
                // chain BEFORE create_dir_all extends it — an attacker may
                // have planted a junction in a pre-existing folder under
                // library_root that would silently redirect the write
                // outside the root the moment we resolve through it.
                if has_reparse_point_in_chain(parent, &canonical_root) {
                    tracing::warn!(
                        parent=%crate::platform::redact_path_for_log(parent),
                        "rejecting move: pre-existing reparse point in destination parent chain"
                    );
                    failed += 1;
                    continue;
                }
                if let Err(err) = std::fs::create_dir_all(parent) {
                    tracing::warn!(?err, parent=%crate::platform::redact_path_for_log(parent), "create_dir_all failed");
                    failed += 1;
                    continue;
                }
                // SEC-5: TOCTOU defense, pass 2. Re-check after
                // create_dir_all. The window between the pre-check and
                // here is small but non-zero; defense in depth is cheap.
                if has_reparse_point_in_chain(parent, &canonical_root) {
                    tracing::warn!(
                        parent=%crate::platform::redact_path_for_log(parent),
                        "rejecting move: reparse point appeared after create_dir_all"
                    );
                    failed += 1;
                    continue;
                }
            }

            if self.use_symlinks {
                let Some(parent) = dest.parent() else {
                    failed += 1;
                    continue;
                };
                match existing_shortcuts.find(source_path, parent, source_identity) {
                    Ok(Some(existing)) => {
                        tracing::info!(
                            file_id = m.file_id,
                            link = %crate::platform::redact_path_for_log(&existing),
                            "[RESTRUCTURE] matching shortcut already exists"
                        );
                        continue;
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            file_id = m.file_id,
                            "[RESTRUCTURE] could not inspect existing shortcuts"
                        );
                        failed += 1;
                        continue;
                    }
                }
            }

            // Skip a no-op (the file already sits at its PLANNED destination)
            // BEFORE uniquifying. If we uniquified first, `unique_destination`
            // would see the file itself occupying `dest`, bump it to a ` (2)`
            // sibling, and we'd rename an already-correctly-placed file —
            // churning an organized library, silently in auto-file mode. (ENG-42)
            // A no-op is not "applied": reporting it as a fresh move would make
            // the app offer Undo for an older journal that this run did not create.
            if !self.use_symlinks && paths_equal(&m.source, &dest.to_string_lossy()) {
                continue;
            }

            // B3: real moves never clobber. `move_file` uses a no-replace
            // handle-relative rename, and we additionally resolve a
            // collision-free name within the SAME parent (so containment +
            // the reparse checks above still hold) — both distinct files
            // survive. Symlink mode keeps the requested name and fails
            // naturally if it's taken (CreateSymbolicLinkW won't overwrite).
            let final_dest = if self.use_symlinks {
                dest.clone()
            } else {
                match claimed.reserve(&dest) {
                    Ok(destination) if self.strict_destinations && destination != dest => {
                        tracing::warn!(
                            file_id = m.file_id,
                            "[RESTRUCTURE] planned destination became occupied after preflight"
                        );
                        failed += 1;
                        continue;
                    }
                    Ok(destination) => destination,
                    Err(err) => {
                        tracing::warn!(
                            ?err,
                            file_id = m.file_id,
                            "[RESTRUCTURE] could not reserve a collision-free destination"
                        );
                        failed += 1;
                        continue;
                    }
                }
            };

            // WRITE-AHEAD: the inverse entry (final → original) is durable
            // BEFORE the move executes. If the journal cannot open, abort now —
            // no file has moved yet (lazy open fires on the first real move).
            // If a later append fails, stop BEFORE the unrecorded move: every
            // completed move stays undoable, the remainder is reported failed.
            let mut journal_entry_offset: Option<u64> = None;
            if record_undo && !self.use_symlinks {
                if journal.is_none() {
                    journal = Some(
                        UndoJournal::open_replacing(self.undo_journal_path(), &canonical_root)
                            .context("undo journal unavailable; aborting before any file moves")?,
                    );
                }
                let j = journal.as_mut().expect("journal just opened");
                match j.append_ahead(
                    m.file_id,
                    &final_dest.to_string_lossy(),
                    &m.source,
                    source_identity,
                ) {
                    Ok(prev) => journal_entry_offset = Some(prev),
                    Err(err) => {
                        tracing::error!(
                            ?err,
                            applied,
                            failed,
                            "[RESTRUCTURE] undo journal append failed; stopping before the unrecorded move"
                        );
                        let remainder = total.saturating_sub(idx).max(1);
                        failed = failed.saturating_add(u32::try_from(remainder).unwrap_or(u32::MAX));
                        break;
                    }
                }
            }
            if record_undo && self.use_symlinks && shortcut_manifest.is_none() {
                shortcut_manifest = Some(
                    ShortcutUndoManifest::create(&self.shortcut_undo_dir()?, &canonical_root)
                        .context(
                            "shortcut undo manifest unavailable; aborting before creating shortcuts",
                        )?,
                );
            }

            #[cfg(test)]
            let inject_move_failure = self
                .fail_next_move_after_journal
                .swap(false, Ordering::Relaxed);
            #[cfg(not(test))]
            let inject_move_failure = false;
            #[cfg(test)]
            let inject_symlink_post_create_failure = self
                .fail_next_symlink_post_create
                .swap(false, Ordering::Relaxed);
            #[cfg(not(test))]
            let inject_symlink_post_create_failure = false;
            #[cfg(test)]
            let inject_shortcut_commit_failure = self
                .fail_next_shortcut_manifest_commit
                .swap(false, Ordering::Relaxed);
            #[cfg(not(test))]
            let inject_shortcut_commit_failure = false;
            let result = if inject_move_failure {
                Err(ApplyError::Other(anyhow::anyhow!(
                    "injected move failure after journal append"
                )))
            } else if self.use_symlinks {
                if let Some(manifest) = shortcut_manifest.as_mut() {
                    create_recorded_shortcut(
                        manifest,
                        m.file_id,
                        &m.source,
                        &final_dest,
                        source_identity,
                        &canonical_root,
                        inject_symlink_post_create_failure,
                        inject_shortcut_commit_failure,
                    )
                } else {
                    make_symlink(
                        &m.source,
                        &final_dest,
                        source_identity,
                        &canonical_root,
                        inject_symlink_post_create_failure,
                    )
                }
            } else {
                move_file(&m.source, &final_dest, source_identity, &canonical_root)
                    .map(|()| SymlinkOutcome::Created)
            };
            match result {
                Ok(SymlinkOutcome::AlreadyPresent) => {}
                Ok(SymlinkOutcome::Created) => {
                    if let Some(journal) = journal.as_mut() {
                        if let Err(error) = journal.commit_replacement() {
                            tracing::warn!(
                                ?error,
                                "[RESTRUCTURE] preserved prior undo journal cleanup will retry before returning"
                            );
                        }
                    }
                    if !self.use_symlinks {
                        // Only update DB on real moves. Symlinks leave
                        // `path_text` pointing at the original.
                        let db_updated = match update_path_in_db(&self.db_conn, m.file_id, &final_dest) {
                            Ok(()) => true,
                            Err(err) => {
                                tracing::error!(
                                    ?err,
                                    file_id = m.file_id,
                                    dst = %crate::platform::redact_path_for_log(&final_dest),
                                    "[RESTRUCTURE] moved on disk but DB path update failed; recorded for recovery"
                                );
                                record_path_update_failure(
                                    m.file_id,
                                    &m.source,
                                    &final_dest,
                                    crate::platform::file_identity(&final_dest),
                                    &canonical_root,
                                );
                                false
                            }
                        };
                        crate::shell::tags::move_sidecar(
                            std::path::Path::new(&m.source),
                            &final_dest,
                        );
                        if !db_updated {
                            applied += 1;
                            failed += 1;
                            continue;
                        }
                        if record_undo {
                            applied_pairs.push((m.source.clone(), final_dest.clone()));
                            if applied_pairs.len() >= APPLY_PROGRESS_INTERVAL {
                                record_feedback_batch(&self.db_conn, &mut applied_pairs);
                            }
                        }
                    }
                    applied += 1;
                }
                Err(ApplyError::Privilege(msg)) => {
                    // The journaled entry describes a move that never happened —
                    // roll it back so undo can't replay a phantom.
                    if let (Some(j), Some(prev)) = (journal.as_mut(), journal_entry_offset) {
                        j.rollback_to(prev);
                    }
                    let shortcut_undo_token =
                        shortcut_manifest.take().and_then(ShortcutUndoManifest::finish);
                    return Ok(RestructureApplyResult {
                        applied,
                        failed,
                        privilege_error: Some(msg),
                        cancelled: false,
                        planned,
                        remaining: None,
                        shortcut_undo_token,
                    });
                }
                Err(ApplyError::Other(err)) => {
                    tracing::warn!(
                        ?err,
                        src=%crate::platform::redact_path_for_log(&m.source),
                        dst=%crate::platform::redact_path_for_log(&final_dest),
                        "move failed"
                    );
                    if let (Some(j), Some(prev)) = (journal.as_mut(), journal_entry_offset) {
                        j.rollback_to(prev);
                    }
                    // D4: the move never happened, so release the reservation —
                    // otherwise a later move whose natural destination equals
                    // this (now-free) path is needlessly uniquified to " (2)".
                    if !self.use_symlinks {
                        claimed.release(&final_dest);
                    }
                    failed += 1;
                }
            }
        }

        // Every entry is already individually durable (write-ahead); one final
        // sync_all covers file metadata on a clean finish. (None during an undo
        // run, record_undo=false, so a CANCELLED undo leaves the ORIGINAL
        // journal intact and the user can re-run undo for the remainder.)
        if let Some(mut journal) = journal {
            if journal.first_move_committed && !journal.committed {
                journal
                    .commit_replacement()
                    .context("committing replacement undo journal after a successful move")?;
            }
            if journal.committed {
                let _ = journal.file.sync_all();
            } else {
                journal.restore_prior_if_uncommitted();
            }
        }
        let shortcut_undo_token = shortcut_manifest.and_then(ShortcutUndoManifest::finish);

        // Learn-from-corrections: each applied move is an approved example, so credit
        // its filename tokens toward its destination folder for future plans. One lock
        // acquisition for the whole batch; best-effort, never fails an apply. Forward
        // applies only — `applied_pairs` is empty on an undo run (record_undo=false).
        if record_undo {
            record_feedback_batch(&self.db_conn, &mut applied_pairs);
        }
        Ok(RestructureApplyResult {
            applied,
            failed,
            privilege_error: None,
            cancelled,
            planned,
            remaining: cancelled.then(|| total.saturating_sub(processed) as u64),
            shortcut_undo_token,
        })
    }

    // ── Undo (R2 — reversible "Undo last run") ──────────────────────────────

    fn undo_journal_path(&self) -> Option<PathBuf> {
        if let Some(path) = self.undo_journal_override.clone() {
            return Some(path);
        }
        #[cfg(test)]
        {
            Some(self.library_root.join(".fileid-test-restructure-undo.ndjson"))
        }
        #[cfg(not(test))]
        {
            crate::paths::trash_log_path()
                .ok()
                .and_then(|t| t.parent().map(|d| d.join("restructure_undo.ndjson")))
        }
    }

    fn open_undo_journal(&self) -> Result<Option<UndoJournalIter>> {
        let Some(path) = self.undo_journal_path() else {
            return Ok(None);
        };
        match File::open(&path) {
            Ok(file) => Ok(Some(UndoJournalIter {
                lines: BufReader::new(file).lines(),
            })),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("opening undo journal {}", path.display())),
        }
    }

    /// Undo the most recent `apply`: replay the inverse moves through `apply`
    /// itself (so the identical stale-check / containment / no-clobber / DB-update
    /// safety applies), then clear the journal so a run can't be undone twice.
    ///
    /// Replay is NEWEST-FIRST (reverse journal order): with dependent moves
    /// (A→X then B→A) the forward order restores A into the slot B currently
    /// occupies and uniquifies it into "A (2)" — silent corruption. Reverse
    /// order first gives B its home back, then A. A torn TRAILING entry (crash
    /// mid-append, before its fsync — so its move never executed under the
    /// write-ahead ordering) is skipped; torn data anywhere else fails closed.
    /// (RESTRUCTURE.md §6 reversibility; macOS parity, audit 2026-07-14)
    pub fn undo_last(&self) -> Result<RestructureApplyResult> {
        let Some(path) = self.undo_journal_path() else {
            return Ok(RestructureApplyResult {
                applied: 0,
                failed: 0,
                privilege_error: None,
                cancelled: false,
                planned: Some(0),
                remaining: None,
                shortcut_undo_token: None,
            });
        };
        recover_prior_undo_journal(&path, &self.library_root)?;
        // One forward pass collects byte spans + validates entries; replay then
        // seeks backward through the spans. No journal-sized String/Vec is
        // retained even for a million-move apply (16 B/entry of offsets).
        let Some(scan) = scan_undo_journal_spans(&path)? else {
            return Ok(RestructureApplyResult {
                applied: 0,
                failed: 0,
                privilege_error: None,
                cancelled: false,
                planned: Some(0),
                remaining: None,
                shortcut_undo_token: None,
            });
        };
        let total = scan.spans.len();
        let recorded_root = scan.library_root.context(
            "undo journal predates exact library-root ownership; refusing automatic recovery",
        )?;
        let canonical_root = canonicalize_safely(&self.library_root)
            .with_context(|| format!("library root {}", self.library_root.display()))?;
        let canonical_recorded_root = canonicalize_safely(&recorded_root)
            .with_context(|| format!("recorded library root {}", recorded_root.display()))?;
        if !paths_equal(
            &canonical_recorded_root.to_string_lossy(),
            &canonical_root.to_string_lossy(),
        ) {
            anyhow::bail!("undo journal belongs to a different library root");
        }
        if total == 0 {
            std::fs::remove_file(&path)
                .with_context(|| format!("removing empty undo journal {}", path.display()))?;
            return Ok(RestructureApplyResult {
                applied: 0,
                failed: 0,
                privilege_error: None,
                cancelled: false,
                planned: Some(0),
                remaining: None,
                shortcut_undo_token: None,
            });
        }
        let spans = scan.spans;
        let validation_file = File::open(&path)
            .with_context(|| format!("reopening undo journal {}", path.display()))?;
        for entry in (ReverseUndoIter {
            file: validation_file,
            spans: spans.clone(),
            next: total,
        }) {
            let entry = entry?;
            if ensure_inside_root(Path::new(&entry.source), &canonical_root).is_err()
                || ensure_inside_root(Path::new(&entry.destination), &canonical_root).is_err()
            {
                anyhow::bail!("undo journal belongs to a different library root");
            }
        }
        let file = File::open(&path)
            .with_context(|| format!("reopening undo journal {}", path.display()))?;
        let inverse = ReverseUndoIter { file, spans, next: total };
        // record_undo:false so the undo's own moves DON'T overwrite the journal — a
        // cancelled undo must leave the original intact so the user can re-run it and
        // put the REMAINING files back (already-restored ones stale-skip on the
        // retry). Only a fully-completed (non-cancelled) undo clears it.
        let result = self.apply_iter_with(
            inverse,
            Some(total),
            false,
        )?;
        #[cfg(test)]
        if self
            .cancel_after_undo_replay
            .swap(false, Ordering::Relaxed)
        {
            self.cancel.store(true, Ordering::Relaxed);
        }
        // Clear the journal ONLY on a fully-completed undo: not cancelled AND
        // every inverse move succeeded. A partial failure (a file locked by
        // another process, a privilege error) keeps the journal so the user can
        // re-run undo and put the REMAINING files back — the already-restored
        // ones stale-skip on the retry, exactly like the cancel path. Deleting
        // it on partial failure permanently stranded the un-restored files in
        // their group folders with no inverse-move record. (audit 2026-07-08)
        if !result.cancelled && result.failed == 0 {
            // Re-read the journal for bounded-memory empty-directory cleanup
            // before deleting it. Repeated parents are harmless: remove_dir is
            // empty-only, and later entries simply observe an absent directory.
            self.cleanup_empty_dirs_from_journal();
            let _ = std::fs::remove_file(&path);
        }
        Ok(result)
    }

    pub fn undo_shortcuts(&self, token: &str) -> Result<RestructureApplyResult> {
        let manifest_dir = self.shortcut_undo_dir()?;
        let manifest_path = shortcut_manifest_path(&manifest_dir, token)?;
        let receipt_path = shortcut_receipt_path(&manifest_dir, token)?;
        let canonical_root = canonicalize_safely(&self.library_root)
            .with_context(|| format!("library root {}", self.library_root.display()))?;
        if let Some(receipt) = read_shortcut_undo_receipt(&receipt_path)? {
            anyhow::ensure!(
                receipt.token == token,
                "shortcut undo receipt token does not match the requested token"
            );
            let recorded_root = canonicalize_safely(Path::new(&receipt.library_root))
                .context("canonicalizing shortcut undo receipt library root")?;
            anyhow::ensure!(
                paths_equal(
                    &recorded_root.to_string_lossy(),
                    &canonical_root.to_string_lossy()
                ),
                "shortcut undo receipt belongs to a different library root"
            );
            return Ok(RestructureApplyResult {
                applied: receipt.applied,
                failed: 0,
                privilege_error: None,
                cancelled: false,
                planned: Some(receipt.planned),
                remaining: None,
                shortcut_undo_token: None,
            });
        }
        let Some(scan) = scan_shortcut_undo_manifest(&manifest_path)? else {
            anyhow::bail!("shortcut undo token was not found");
        };

        anyhow::ensure!(
            scan.header.token == token,
            "shortcut undo manifest token does not match the requested token"
        );
        let recorded_root = canonicalize_safely(Path::new(&scan.header.library_root))
            .context("canonicalizing shortcut undo manifest library root")?;
        anyhow::ensure!(
            paths_equal(
                &recorded_root.to_string_lossy(),
                &canonical_root.to_string_lossy()
            ),
            "shortcut undo manifest belongs to a different library root"
        );

        let manifest_identity = crate::platform::file_identity_from_file(&scan.file)
            .context("reading shortcut undo manifest identity")?;
        let recovered_intents = recover_shortcut_intents(&scan, token, &canonical_root)?;
        let header = scan.header;
        let entry_total = scan.spans.len();
        let total = entry_total.saturating_add(recovered_intents as usize);
        let planned = Some(u64::try_from(total).unwrap_or(u64::MAX));
        let mut entries = ReverseShortcutUndoIter {
            file: scan.file,
            spans: scan.spans,
            next: entry_total,
        };
        let mut applied = recovered_intents;
        let mut failed = 0u32;
        let mut processed = recovered_intents as usize;
        let mut cancelled = false;

        for entry in &mut entries {
            if self.cancel.load(Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let entry = entry?;
            processed += 1;
            let removal = remove_recorded_shortcut(
                Path::new(&entry.link),
                Path::new(&entry.source),
                entry.source_identity,
                entry.link_identity,
                &canonical_root,
            )
            .and_then(|removed| {
                if removed {
                    return Ok(true);
                }
                let Some(staging_link) = entry.staging_link.as_deref() else {
                    return Ok(false);
                };
                remove_recorded_shortcut(
                    Path::new(staging_link),
                    Path::new(&entry.source),
                    entry.source_identity,
                    entry.link_identity,
                    &canonical_root,
                )
            });
            match removal {
                Ok(_) => applied = applied.saturating_add(1),
                Err(error) => {
                    failed = failed.saturating_add(1);
                    tracing::warn!(
                        ?error,
                        file_id = entry.file_id,
                        "[RESTRUCTURE] shortcut undo entry could not be safely removed"
                    );
                }
            }
        }

        if !cancelled && failed == 0 {
            anyhow::ensure!(
                crate::platform::file_identity(&manifest_path) == Some(manifest_identity),
                "shortcut undo manifest changed during replay"
            );
            cleanup_shortcut_staging_dir(&header, token, &canonical_root)?;
            write_shortcut_undo_receipt(
                &manifest_dir,
                &ShortcutUndoReceipt {
                    version: SHORTCUT_UNDO_RECEIPT_VERSION,
                    library_root: canonical_root.to_string_lossy().into_owned(),
                    token: token.to_string(),
                    applied,
                    planned: u64::try_from(total).unwrap_or(u64::MAX),
                },
            )?;
            if crate::platform::file_identity(&manifest_path) == Some(manifest_identity) {
                if let Err(error) = std::fs::remove_file(&manifest_path) {
                    tracing::warn!(
                        ?error,
                        "[RESTRUCTURE] completed shortcut manifest cleanup will retry later"
                    );
                }
            } else {
                tracing::warn!(
                    "[RESTRUCTURE] completed shortcut manifest changed before cleanup; receipt remains authoritative"
                );
            }
        }

        Ok(RestructureApplyResult {
            applied,
            failed,
            privilege_error: None,
            cancelled,
            planned,
            remaining: cancelled.then(|| {
                u64::try_from(total.saturating_sub(processed)).unwrap_or(u64::MAX)
            }),
            shortcut_undo_token: None,
        })
    }

    /// Remove the empty group folders an apply created, after its undo restored the
    /// files. `std::fs::remove_dir` only succeeds on an EMPTY dir, so user files are
    /// never at risk; we additionally stay strictly inside the library root and never
    /// touch the root itself. Deepest-first so nested empties fully collapse.
    /// Best-effort. (R2 → reversibility completeness)
    fn cleanup_empty_dirs_from_journal(&self) {
        let root = self.library_root.as_path();
        let Ok(Some(entries)) = self.open_undo_journal() else {
            return;
        };
        for entry in entries.flatten() {
            let Some(dir) = Path::new(&entry.from).parent() else {
                continue;
            };
            let mut cur = dir.to_path_buf();
            while cur.as_path() != root && cur.starts_with(root) && std::fs::remove_dir(&cur).is_ok()
            {
                match cur.parent() {
                    Some(p) => cur = p.to_path_buf(),
                    None => break,
                }
            }
        }
    }
}

const APPLY_PROGRESS_INTERVAL: usize = 500;

/// Apply-progress throttle: emit on the first move, on the last, and once per
/// `interval` processed moves, so a 100k-move apply logs ~total/interval lines
/// instead of none (silent) or one-per-move (flood). Pure → the cadence is
/// unit-assertable. (F-C6-013)
fn should_emit_apply_progress(processed: usize, total: usize, interval: usize) -> bool {
    if interval == 0 || processed == 0 {
        return false;
    }
    processed == 1 || processed == total || processed % interval == 0
}

#[derive(Debug)]
#[cfg_attr(not(windows), allow(dead_code))]
enum ApplyError {
    Privilege(String),
    Other(anyhow::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SymlinkOutcome {
    Created,
    AlreadyPresent,
}

#[cfg(windows)]
fn shortcut_link_identity(link: &Path) -> Result<crate::platform::FileIdentity> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FileAttributeTagInfo, GetFileInformationByHandleEx, FILE_ATTRIBUTE_TAG_INFO,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let link_ext = crate::util::path_safety::to_extended_length(link);
    let metadata = std::fs::symlink_metadata(&link_ext).context("reading shortcut metadata")?;
    anyhow::ensure!(
        metadata.file_type().is_symlink(),
        "shortcut path is not a symbolic link"
    );
    let wide: Vec<u16> = link_ext
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )?
    };
    anyhow::ensure!(!handle.is_invalid(), "opening shortcut without following it");
    let link_file = unsafe { File::from_raw_handle(handle.0 as _) };
    let raw_handle = HANDLE(link_file.as_raw_handle());
    let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            raw_handle,
            FileAttributeTagInfo,
            (&raw mut tag).cast(),
            u32::try_from(std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>())
                .context("shortcut attribute structure size")?,
        )?;
    }
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
    anyhow::ensure!(
        tag.ReparseTag == IO_REPARSE_TAG_SYMLINK,
        "shortcut path is not a symbolic link"
    );
    crate::platform::file_identity_from_file(&link_file)
        .context("reading no-follow shortcut identity")
}

#[cfg(not(windows))]
fn shortcut_link_identity(link: &Path) -> Result<crate::platform::FileIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::symlink_metadata(link).context("reading shortcut metadata")?;
    anyhow::ensure!(
        metadata.file_type().is_symlink(),
        "shortcut path is not a symbolic link"
    );
    Ok(crate::platform::FileIdentity {
        volume: metadata.dev(),
        file: metadata.ino(),
    })
}

#[cfg(windows)]
fn remove_recorded_shortcut(
    link: &Path,
    source: &Path,
    expected_source: crate::platform::FileIdentity,
    expected_link: crate::platform::FileIdentity,
    canonical_root: &Path,
) -> Result<bool> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows::Win32::Foundation::{BOOLEAN, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FileAttributeTagInfo, FileDispositionInfo,
        GetFileInformationByHandleEx, SetFileInformationByHandle, FILE_ATTRIBUTE_TAG_INFO,
        FILE_DISPOSITION_INFO, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, DELETE, OPEN_EXISTING,
    };

    let parent = link.parent().context("shortcut has no parent")?;
    anyhow::ensure!(
        ensure_inside_root(parent, canonical_root).is_ok()
            && !has_reparse_point_in_chain(parent, canonical_root),
        "shortcut path is not safely contained by the selected library root"
    );
    let link_ext = crate::util::path_safety::to_extended_length(link);
    let metadata = match std::fs::symlink_metadata(&link_ext) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("reading shortcut metadata"),
    };
    anyhow::ensure!(
        metadata.file_type().is_symlink(),
        "recorded shortcut path is occupied by a non-shortcut"
    );
    anyhow::ensure!(
        crate::platform::file_identity(source) == Some(expected_source)
            && symlink_target_matches(link, source)?,
        "recorded shortcut no longer points to the original file"
    );

    let expected_parent = crate::platform::file_identity(parent)
        .context("reading shortcut parent identity")?;
    let parent_handle = crate::commands::trash::open_windows_directory_lock(parent)?;
    anyhow::ensure!(
        crate::platform::file_identity_from_file(&parent_handle) == Some(expected_parent)
            && crate::platform::file_identity(parent) == Some(expected_parent),
        "shortcut parent changed during validation"
    );
    let held_parent = crate::commands::trash::windows_handle_path(&parent_handle)?;
    anyhow::ensure!(
        ci_starts_with(&held_parent, canonical_root),
        "shortcut parent escaped the selected library root"
    );

    let wide: Vec<u16> = link_ext
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            DELETE.0 | FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )?
    };
    anyhow::ensure!(!handle.is_invalid(), "opening recorded shortcut");
    let link_file = unsafe { File::from_raw_handle(handle.0 as _) };
    let raw_handle = HANDLE(link_file.as_raw_handle());
    let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            raw_handle,
            FileAttributeTagInfo,
            (&raw mut tag).cast(),
            u32::try_from(std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>())
                .context("shortcut attribute structure size")?,
        )?;
    }
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
    anyhow::ensure!(
        tag.ReparseTag == IO_REPARSE_TAG_SYMLINK
            && crate::platform::file_identity_from_file(&link_file) == Some(expected_link),
        "recorded shortcut path is not the original symbolic link"
    );
    anyhow::ensure!(
        crate::platform::file_identity_from_file(&parent_handle) == Some(expected_parent)
            && crate::platform::file_identity(parent) == Some(expected_parent)
            && crate::platform::file_identity(source) == Some(expected_source)
            && symlink_target_matches(link, source)?,
        "recorded shortcut changed during validation"
    );

    let disposition = FILE_DISPOSITION_INFO {
        DeleteFile: BOOLEAN(1),
    };
    unsafe {
        SetFileInformationByHandle(
            raw_handle,
            FileDispositionInfo,
            (&raw const disposition).cast(),
            u32::try_from(std::mem::size_of::<FILE_DISPOSITION_INFO>())
                .context("shortcut disposition structure size")?,
        )?;
    }
    drop(link_file);
    Ok(true)
}

#[cfg(not(windows))]
fn remove_recorded_shortcut(
    link: &Path,
    source: &Path,
    expected_source: crate::platform::FileIdentity,
    expected_link: crate::platform::FileIdentity,
    canonical_root: &Path,
) -> Result<bool> {
    let parent = link.parent().context("shortcut has no parent")?;
    ensure_inside_root(parent, canonical_root)?;
    let metadata = match std::fs::symlink_metadata(link) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error).context("reading shortcut metadata"),
    };
    anyhow::ensure!(
        metadata.file_type().is_symlink(),
        "recorded shortcut path is occupied by a non-shortcut"
    );
    anyhow::ensure!(
        shortcut_link_identity(link)? == expected_link
            && crate::platform::file_identity(source) == Some(expected_source)
            && symlink_target_matches(link, source)?,
        "recorded shortcut is no longer the original link to the original file"
    );
    anyhow::ensure!(
        shortcut_link_identity(link)? == expected_link,
        "recorded shortcut changed during validation"
    );
    std::fs::remove_file(link).context("removing recorded shortcut")?;
    Ok(true)
}

fn cleanup_prepared_shortcut(
    manifest: &ShortcutUndoManifest,
    prepared: &PreparedShortcutIntent,
    expected_link: crate::platform::FileIdentity,
    canonical_root: &Path,
) -> Result<()> {
    remove_recorded_shortcut(
        Path::new(&prepared.intent.staging_link),
        Path::new(&prepared.intent.source),
        prepared.intent.source_identity,
        expected_link,
        canonical_root,
    )?;
    manifest.complete_intent(prepared)
}

fn create_recorded_shortcut(
    manifest: &mut ShortcutUndoManifest,
    file_id: i64,
    source: &str,
    final_link: &Path,
    source_identity: crate::platform::FileIdentity,
    canonical_root: &Path,
    force_post_create_validation_failure: bool,
    force_manifest_commit_failure: bool,
) -> std::result::Result<SymlinkOutcome, ApplyError> {
    let source_path = Path::new(source);
    if let Ok(metadata) = std::fs::symlink_metadata(final_link) {
        if metadata.file_type().is_symlink()
            && crate::platform::file_identity(source_path) == Some(source_identity)
            && symlink_target_matches(final_link, source_path).unwrap_or(false)
        {
            return Ok(SymlinkOutcome::AlreadyPresent);
        }
        return Err(ApplyError::Other(anyhow::anyhow!(
            "shortcut destination is already occupied"
        )));
    }

    let mut prepared = manifest
        .prepare_intent(file_id, source, final_link, source_identity)
        .map_err(ApplyError::Other)?;
    let staged_link = PathBuf::from(&prepared.intent.staging_link);
    match make_symlink(
        source,
        &staged_link,
        source_identity,
        canonical_root,
        force_post_create_validation_failure,
    ) {
        Ok(SymlinkOutcome::Created) => {}
        Ok(SymlinkOutcome::AlreadyPresent) => {
            return Err(ApplyError::Other(anyhow::anyhow!(
                "shortcut staging path was unexpectedly occupied; recovery evidence was preserved"
            )));
        }
        Err(error) => {
            if std::fs::symlink_metadata(&staged_link).is_err() {
                manifest
                    .complete_intent(&prepared)
                    .map_err(ApplyError::Other)?;
            }
            return Err(error);
        }
    }

    let link_identity = shortcut_link_identity(&staged_link).map_err(|error| {
        ApplyError::Other(error.context(
            "created staged shortcut identity could not be read; recovery evidence was preserved",
        ))
    })?;
    if let Err(error) = manifest.record_staged_identity(&mut prepared, link_identity) {
        return match cleanup_prepared_shortcut(
            manifest,
            &prepared,
            link_identity,
            canonical_root,
        ) {
            Ok(()) => Err(ApplyError::Other(error)),
            Err(cleanup_error) => Err(ApplyError::Other(error.context(format!(
                "recording the staged shortcut identity failed and identity-bound cleanup also failed: {cleanup_error:#}"
            )))),
        };
    }

    let commit = if force_manifest_commit_failure {
        Err(anyhow::anyhow!(
            "injected shortcut manifest commit failure"
        ))
    } else {
        manifest.append_committed(
            file_id,
            source,
            final_link,
            Some(&staged_link),
            source_identity,
            link_identity,
        )
    };
    let previous = match commit {
        Ok(previous) => previous,
        Err(error) => {
            return match cleanup_prepared_shortcut(
                manifest,
                &prepared,
                link_identity,
                canonical_root,
            ) {
                Ok(()) => Err(ApplyError::Other(error)),
                Err(cleanup_error) => Err(ApplyError::Other(error.context(format!(
                    "shortcut undo commit failed and identity-bound cleanup also failed: {cleanup_error:#}"
                )))),
            };
        }
    };

    if let Err(error) = rename_staged_shortcut(
        &staged_link,
        final_link,
        source_path,
        source_identity,
        link_identity,
        canonical_root,
    ) {
        let staged_is_original = shortcut_link_identity(&staged_link)
            .is_ok_and(|identity| identity == link_identity)
            && symlink_target_matches(&staged_link, source_path).unwrap_or(false);
        if staged_is_original {
            match cleanup_prepared_shortcut(
                manifest,
                &prepared,
                link_identity,
                canonical_root,
            ) {
                Ok(()) => {
                    manifest.rollback_committed(previous);
                    return Err(ApplyError::Other(error));
                }
                Err(cleanup_error) => {
                    return Err(ApplyError::Other(error.context(format!(
                        "staged shortcut rename failed and identity-bound cleanup also failed: {cleanup_error:#}"
                    ))));
                }
            }
        }
        return Err(ApplyError::Other(error.context(
            "staged shortcut rename was ambiguous; durable recovery evidence was preserved",
        )));
    }

    manifest.complete_intent(&prepared).map_err(|error| {
        ApplyError::Other(error.context(
            "published shortcut is durable but its completed intent could not be removed",
        ))
    })?;
    Ok(SymlinkOutcome::Created)
}

#[cfg(windows)]
fn rename_staged_shortcut(
    staged_link: &Path,
    final_link: &Path,
    source: &Path,
    expected_source: crate::platform::FileIdentity,
    expected_link: crate::platform::FileIdentity,
    canonical_root: &Path,
) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows::Win32::Foundation::{BOOLEAN, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FileAttributeTagInfo, GetFileInformationByHandleEx,
        FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
        FILE_RENAME_INFO, FILE_RENAME_INFO_0, FILE_SHARE_READ, DELETE, OPEN_EXISTING,
    };

    let staged_parent = staged_link.parent().context("staged shortcut has no parent")?;
    let final_parent = final_link.parent().context("shortcut destination has no parent")?;
    anyhow::ensure!(
        ensure_inside_root(staged_parent, canonical_root).is_ok()
            && ensure_inside_root(final_parent, canonical_root).is_ok()
            && !has_reparse_point_in_chain(staged_parent, canonical_root)
            && !has_reparse_point_in_chain(final_parent, canonical_root),
        "staged shortcut rename escaped the selected library root"
    );
    anyhow::ensure!(
        crate::platform::file_identity(source) == Some(expected_source)
            && symlink_target_matches(staged_link, source)?,
        "staged shortcut no longer points to the original source"
    );

    let staged_ext = crate::util::path_safety::to_extended_length(staged_link);
    let wide: Vec<u16> = staged_ext
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            DELETE.0 | FILE_READ_ATTRIBUTES.0,
            FILE_SHARE_READ,
            None,
            OPEN_EXISTING,
            FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )?
    };
    anyhow::ensure!(!handle.is_invalid(), "opening staged shortcut");
    let staged_file = unsafe { File::from_raw_handle(handle.0 as _) };
    let raw_handle = HANDLE(staged_file.as_raw_handle());
    let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
    unsafe {
        GetFileInformationByHandleEx(
            raw_handle,
            FileAttributeTagInfo,
            (&raw mut tag).cast(),
            u32::try_from(std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>())?,
        )?;
    }
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;
    anyhow::ensure!(
        tag.ReparseTag == IO_REPARSE_TAG_SYMLINK
            && crate::platform::file_identity_from_file(&staged_file) == Some(expected_link),
        "staged shortcut is not the recorded symbolic link"
    );

    let expected_parent = crate::platform::file_identity(final_parent)
        .context("reading shortcut destination parent identity")?;
    let parent_handle = crate::commands::trash::open_windows_directory_lock(final_parent)?;
    anyhow::ensure!(
        crate::platform::file_identity_from_file(&parent_handle) == Some(expected_parent)
            && crate::platform::file_identity(final_parent) == Some(expected_parent)
            && ci_starts_with(
                &crate::commands::trash::windows_handle_path(&parent_handle)?,
                canonical_root
            ),
        "shortcut destination parent changed during staged rename"
    );
    let destination_wide: Vec<u16> = final_link
        .file_name()
        .context("shortcut destination has no filename")?
        .encode_wide()
        .collect();
    let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let byte_len = header + destination_wide.len() * std::mem::size_of::<u16>();
    let mut storage = vec![0u64; byte_len.div_ceil(std::mem::size_of::<u64>())];
    let rename = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*rename).Anonymous = FILE_RENAME_INFO_0 {
            ReplaceIfExists: BOOLEAN(0),
        };
        (*rename).RootDirectory = HANDLE(parent_handle.as_raw_handle());
        (*rename).FileNameLength = u32::try_from(destination_wide.len() * 2)?;
        std::ptr::copy_nonoverlapping(
            destination_wide.as_ptr(),
            std::ptr::addr_of_mut!((*rename).FileName).cast::<u16>(),
            destination_wide.len(),
        );
        crate::commands::trash::nt_rename_relative(
            raw_handle,
            rename.cast(),
            u32::try_from(byte_len)?,
        )?;
    }
    anyhow::ensure!(
        crate::platform::file_identity_from_file(&staged_file) == Some(expected_link)
            && shortcut_link_identity(final_link)? == expected_link
            && crate::platform::file_identity(source) == Some(expected_source)
            && symlink_target_matches(final_link, source)?,
        "published shortcut changed during staged rename"
    );
    Ok(())
}

#[cfg(not(windows))]
fn rename_staged_shortcut(
    staged_link: &Path,
    final_link: &Path,
    source: &Path,
    expected_source: crate::platform::FileIdentity,
    expected_link: crate::platform::FileIdentity,
    canonical_root: &Path,
) -> Result<()> {
    let staged_parent = staged_link.parent().context("staged shortcut has no parent")?;
    let final_parent = final_link.parent().context("shortcut destination has no parent")?;
    ensure_inside_root(staged_parent, canonical_root)?;
    ensure_inside_root(final_parent, canonical_root)?;
    anyhow::ensure!(
        shortcut_link_identity(staged_link)? == expected_link
            && crate::platform::file_identity(source) == Some(expected_source)
            && symlink_target_matches(staged_link, source)?,
        "staged shortcut changed before rename"
    );
    crate::util::rename_no_replace(staged_link, final_link)
        .context("publishing staged shortcut")?;
    anyhow::ensure!(
        shortcut_link_identity(final_link)? == expected_link
            && crate::platform::file_identity(source) == Some(expected_source)
            && symlink_target_matches(final_link, source)?,
        "published shortcut changed during staged rename"
    );
    Ok(())
}

#[cfg(windows)]
fn move_file(
    src: &str,
    dst: &Path,
    expected: crate::platform::FileIdentity,
    canonical_root: &Path,
) -> std::result::Result<(), ApplyError> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows::Win32::Foundation::{BOOLEAN, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_RENAME_INFO, FILE_RENAME_INFO_0,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, DELETE, OPEN_EXISTING,
    };

    let result = (|| -> Result<()> {
        let source = crate::util::path_safety::to_extended_length(Path::new(src));
        let mut source_wide: Vec<u16> = source.as_os_str().encode_wide().collect();
        source_wide.push(0);
        let handle = unsafe {
            CreateFileW(
                PCWSTR(source_wide.as_ptr()),
                DELETE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )?
        };
        anyhow::ensure!(!handle.is_invalid(), "open restructure source");
        let source_file = unsafe { File::from_raw_handle(handle.0 as _) };
        anyhow::ensure!(
            crate::platform::file_identity_from_file(&source_file) == Some(expected),
            "restructure source changed during validation"
        );

        let destination_parent = dst.parent().context("restructure destination has no parent")?;
        let expected_parent = crate::platform::file_identity(destination_parent)
            .context("read restructure destination parent identity")?;
        let parent_handle = crate::commands::trash::open_windows_directory_lock(destination_parent)?;
        anyhow::ensure!(
            crate::platform::file_identity_from_file(&parent_handle) == Some(expected_parent)
                && crate::platform::file_identity(destination_parent) == Some(expected_parent),
            "restructure destination parent changed during validation"
        );
        let held_parent = crate::commands::trash::windows_handle_path(&parent_handle)?;
        anyhow::ensure!(
            ci_starts_with(&held_parent, canonical_root),
            "restructure destination parent escaped the authorized library root"
        );
        let destination_wide: Vec<u16> = dst
            .file_name()
            .context("restructure destination has no filename")?
            .encode_wide()
            .collect();
        let header = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
        let byte_len = header + destination_wide.len() * std::mem::size_of::<u16>();
        let mut storage = vec![0u64; byte_len.div_ceil(std::mem::size_of::<u64>())];
        let rename = storage.as_mut_ptr().cast::<FILE_RENAME_INFO>();
        unsafe {
            (*rename).Anonymous = FILE_RENAME_INFO_0 { ReplaceIfExists: BOOLEAN(0) };
            (*rename).RootDirectory = HANDLE(parent_handle.as_raw_handle());
            (*rename).FileNameLength = u32::try_from(destination_wide.len() * 2)?;
            std::ptr::copy_nonoverlapping(
                destination_wide.as_ptr(),
                std::ptr::addr_of_mut!((*rename).FileName).cast::<u16>(),
                destination_wide.len(),
            );
            crate::commands::trash::nt_rename_relative(
                HANDLE(source_file.as_raw_handle()),
                rename.cast(),
                u32::try_from(byte_len)?,
            )?;
        }
        Ok(())
    })();
    result.map_err(ApplyError::Other)
}

#[cfg(windows)]
fn make_symlink(
    src: &str,
    dst: &Path,
    expected_source: crate::platform::FileIdentity,
    canonical_root: &Path,
    force_post_create_validation_failure: bool,
) -> std::result::Result<SymlinkOutcome, ApplyError> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_READ_ATTRIBUTES, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let source = Path::new(src);
    let Some(parent) = dst.parent() else {
        return Err(ApplyError::Other(anyhow::anyhow!(
            "shortcut destination has no parent"
        )));
    };
    let result = (|| -> Result<SymlinkOutcome> {
        anyhow::ensure!(
            ensure_inside_root(dst, canonical_root).is_ok()
                && !has_reparse_point_in_chain(parent, canonical_root),
            "shortcut destination is not safely contained by the selected library root"
        );
        anyhow::ensure!(
            crate::platform::file_identity(source) == Some(expected_source),
            "shortcut source changed during validation"
        );

        let expected_parent = crate::platform::file_identity(parent)
            .context("read shortcut destination parent identity")?;
        let parent_handle = crate::commands::trash::open_windows_directory_lock(parent)?;
        anyhow::ensure!(
            crate::platform::file_identity_from_file(&parent_handle) == Some(expected_parent)
                && crate::platform::file_identity(parent) == Some(expected_parent),
            "shortcut destination parent changed during validation"
        );
        let held_parent = crate::commands::trash::windows_handle_path(&parent_handle)?;
        anyhow::ensure!(
            ci_starts_with(&held_parent, canonical_root),
            "shortcut destination parent escaped the selected library root"
        );

        let src_ext = crate::util::path_safety::to_extended_length(source);
        let mut source_wide: Vec<u16> = src_ext.as_os_str().encode_wide().collect();
        source_wide.push(0);
        let source_handle = unsafe {
            CreateFileW(
                PCWSTR(source_wide.as_ptr()),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )?
        };
        anyhow::ensure!(!source_handle.is_invalid(), "open shortcut source");
        let source_file = unsafe { File::from_raw_handle(source_handle.0 as _) };
        anyhow::ensure!(
            crate::platform::file_identity_from_file(&source_file) == Some(expected_source),
            "shortcut source changed while acquiring its lock"
        );

        if let Ok(metadata) =
            std::fs::symlink_metadata(crate::util::path_safety::to_extended_length(dst))
        {
            if metadata.file_type().is_symlink() && symlink_target_matches(dst, source)? {
                return Ok(SymlinkOutcome::AlreadyPresent);
            }
            anyhow::bail!("shortcut destination is already occupied");
        }

        let dst_ext = crate::util::path_safety::to_extended_length(dst);
        let src_w: Vec<u16> = src_ext
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let dst_w: Vec<u16> = dst_ext
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let flags = SYMBOLIC_LINK_FLAG_ALLOW_UNPRIVILEGED_CREATE;
        let created = unsafe {
            CreateSymbolicLinkW(
                PCWSTR(dst_w.as_ptr()),
                PCWSTR(src_w.as_ptr()),
                SYMBOLIC_LINK_FLAGS(flags.0),
            )
        };
        if !created.as_bool() {
            let error = std::io::Error::last_os_error();
            if std::fs::symlink_metadata(&dst_ext)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
                && symlink_target_matches(dst, source).unwrap_or(false)
            {
                return Ok(SymlinkOutcome::AlreadyPresent);
            }
            return Err(anyhow::Error::new(error));
        }

        let valid_after_create = crate::platform::file_identity_from_file(&parent_handle)
            == Some(expected_parent)
            && crate::platform::file_identity(parent) == Some(expected_parent)
            && !has_reparse_point_in_chain(parent, canonical_root)
            && crate::platform::file_identity_from_file(&source_file) == Some(expected_source)
            && crate::platform::file_identity(source) == Some(expected_source)
            && std::fs::symlink_metadata(&dst_ext)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
            && symlink_target_matches(dst, source).unwrap_or(false)
            && !force_post_create_validation_failure;
        if !valid_after_create {
            if std::fs::symlink_metadata(&dst_ext)
                .is_ok_and(|metadata| metadata.file_type().is_symlink())
                && symlink_target_matches(dst, source).unwrap_or(false)
            {
                let _ = std::fs::remove_file(&dst_ext);
            }
            anyhow::bail!("shortcut validation changed during creation; the new link was removed");
        }
        Ok(SymlinkOutcome::Created)
    })();

    match result {
        Ok(outcome) => Ok(outcome),
        Err(error)
            if error
                .chain()
                .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
                .any(|error| error.raw_os_error() == Some(1314)) =>
        {
            Err(ApplyError::Privilege(
                "Symlink mode needs Developer Mode enabled \
                 (Settings → Privacy & security → For developers) \
                 OR an elevated FileID. Try the default 'real move' mode instead."
                    .into(),
            ))
        }
        Err(error) => Err(ApplyError::Other(error)),
    }
}

#[cfg(not(windows))]
fn move_file(
    src: &str,
    dst: &Path,
    expected: crate::platform::FileIdentity,
    _canonical_root: &Path,
) -> std::result::Result<(), ApplyError> {
    // Unix moves are no-replace and identity-bound. Cross-filesystem moves fail
    // closed because copy/delete cannot preserve every source filesystem's
    // metadata and atomicity guarantees.
    let src_path = Path::new(src);
    let source_handle = File::open(src_path)
        .map_err(|error| ApplyError::Other(anyhow::Error::msg(error.to_string())))?;
    if crate::platform::file_identity_from_file(&source_handle) != Some(expected) {
        return Err(ApplyError::Other(anyhow::anyhow!(
            "restructure source changed during validation"
        )));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))?;
    }
    match crate::util::rename_no_replace(src_path, dst) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            Err(ApplyError::Other(anyhow::anyhow!(
                "cross-filesystem restructure moves are not supported; source left unchanged"
            )))
        }
        Err(error) => Err(ApplyError::Other(anyhow::Error::msg(error.to_string()))),
    }
}

#[cfg(not(windows))]
fn make_symlink(
    src: &str,
    dst: &Path,
    expected_source: crate::platform::FileIdentity,
    canonical_root: &Path,
    force_post_create_validation_failure: bool,
) -> std::result::Result<SymlinkOutcome, ApplyError> {
    // The app's "use shortcuts/symlinks instead of moving" option. `dst` is the
    // link to create, `src` the existing target it points at — same operand
    // order as the Windows CreateSymbolicLinkW(dst, src) path. A pre-existing
    // `dst` makes symlink() fail naturally (no clobber). Unix symlink creation
    // is unprivileged, so there is no ApplyError::Privilege arm here.
    if crate::platform::file_identity(Path::new(src)) != Some(expected_source) {
        return Err(ApplyError::Other(anyhow::anyhow!(
            "shortcut source changed during validation"
        )));
    }
    if ensure_inside_root(dst, canonical_root).is_err() {
        return Err(ApplyError::Other(anyhow::anyhow!(
            "shortcut destination is outside the selected library root"
        )));
    }
    if let Ok(metadata) = std::fs::symlink_metadata(dst) {
        if metadata.file_type().is_symlink()
            && symlink_target_matches(dst, Path::new(src)).unwrap_or(false)
        {
            return Ok(SymlinkOutcome::AlreadyPresent);
        }
        return Err(ApplyError::Other(anyhow::anyhow!(
            "shortcut destination is already occupied"
        )));
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))?;
    }
    std::os::unix::fs::symlink(src, dst)
        .map_err(|e| ApplyError::Other(anyhow::Error::msg(e.to_string())))?;
    if force_post_create_validation_failure {
        let _ = std::fs::remove_file(dst);
        return Err(ApplyError::Other(anyhow::anyhow!(
            "shortcut validation changed during creation; the new link was removed"
        )));
    }
    Ok(SymlinkOutcome::Created)
}

fn update_path_in_db(conn: &Arc<Mutex<Connection>>, file_id: i64, new_path: &Path) -> Result<()> {
    let conn = conn.lock();
    // ENG-91: keep path_hash in sync with path_text (same as the rename command
    // + every dbwriter insert) so the column stays consistent for lookups/dedup
    // and cross-platform DB parity — a move that updated only path_text left a
    // stale hash.
    let path_text = new_path.to_string_lossy();
    let path_hash = crate::util::path_safety::stable_path_hash(&path_text);
    // prepare_cached: a plan can issue thousands of moves, so cache the parse on
    // the long-lived writer connection (codebase idiom — see bulk.rs/dbwriter.rs).
    // NFC-normalize path_search like the dbwriter insert + macOS do, so an
    // NFD-accented name stays findable by the app's NFC-normalized search query
    // (the v16 contract). Without this, a moved file is unsearchable by its
    // accented name until the next rescan re-stamps it. (audit parity fix)
    let path_search = crate::pipeline::dbwriter::nfc_path_search(&path_text);
    // OR ABORT is load-bearing: `path_text` is UNIQUE ON CONFLICT REPLACE, so a
    // PLAIN update that collides with a LIVE row already at the new path (a
    // transient earlier update failure left this file's on-disk move done but its
    // DB path stale, then a later move routes another file here; or an external
    // rename desynced the row) would silently REPLACE-delete that row and cascade
    // its user tags/person assignments. OR ABORT raises instead, and the caller's
    // record_path_update_failure recovery arm reconciles it on the next scan.
    // (audit 2026-07: rename-heal ON CONFLICT REPLACE sibling)
    let changed = conn
        .prepare_cached("UPDATE OR ABORT files SET path_text = ?1, path_hash = ?2, path_search = ?4 WHERE id = ?3")?
        .execute(params![path_text, path_hash, file_id, path_search])
        .context("DB UPDATE files.path_text")?;
    if changed != 1 {
        anyhow::bail!("DB UPDATE files.path_text affected {changed} rows (expected 1)");
    }
    Ok(())
}

/// B4 + R-#14: the current `(path_text, file_ref)` the DB holds for `file_id`, or None
/// if the row is gone. `path_text` is the authoritative name; `file_ref` (NTFS MFT
/// reference, stored `u64 as i64`) is the planned-file identity the swap guard checks
/// against the on-disk ref. `file_ref` is None for a row scanned before v8 or on a
/// volume with no readable ref.
fn current_identity_in_db(
    conn: &Arc<Mutex<Connection>>,
    file_id: i64,
) -> Result<Option<(String, Option<i64>)>> {
    let conn = conn.lock();
    let mut stmt = conn.prepare_cached("SELECT path_text, file_ref FROM files WHERE id = ?1")?;
    stmt.query_row(params![file_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
    })
    .optional()
    .context("DB SELECT files.path_text,file_ref")
}

fn identity_matching_db_ref(
    db_ref: Option<i64>,
    identity: Option<crate::platform::FileIdentity>,
) -> Option<crate::platform::FileIdentity> {
    let expected = db_ref? as u64;
    identity.filter(|current| current.file == expected)
}

fn verified_file_identity(
    db_ref: Option<i64>,
    path: &Path,
) -> Option<crate::platform::FileIdentity> {
    identity_matching_db_ref(db_ref, crate::platform::file_identity(path))
}

/// Path equality that tolerates separator/case differences. Fast path is a
/// string compare (the normal case — both came from the same DB row at plan
/// time); otherwise compare canonical forms (a non-existent path canonicalizes
/// to Err and is treated as not-equal, so a vanished source is a mismatch).
fn paths_equal(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// B3: resolve a destination that collides with neither an on-disk file nor a
/// destination already claimed by an earlier move in this batch, by appending
/// ` (2)`, ` (3)`, … before the extension — within the same parent so the
/// containment/reparse checks already performed on `dest` still hold.
#[cfg(test)]
fn unique_destination(
    dest: &Path,
    claimed: &HashSet<ClaimedDestination>,
) -> Result<PathBuf> {
    unique_destination_from(dest, claimed, 2).map(|(destination, _)| destination)
}

fn resolved_symlink_target(link: &Path) -> Result<PathBuf> {
    let target = std::fs::read_link(crate::util::path_safety::to_extended_length(link))
        .with_context(|| format!("reading shortcut target {}", link.display()))?;
    if target.is_absolute() {
        Ok(target)
    } else {
        Ok(link
            .parent()
            .context("shortcut has no parent")?
            .join(target))
    }
}

fn symlink_target_matches(link: &Path, source: &Path) -> Result<bool> {
    let target = resolved_symlink_target(link)?;
    let target = canonicalize_safely(&target)?;
    let source = canonicalize_safely(source)?;
    Ok(claimed_destination_key(&target) == claimed_destination_key(&source))
}

fn unique_destination_from(
    dest: &Path,
    claimed: &HashSet<ClaimedDestination>,
    start_suffix: u64,
) -> Result<(PathBuf, u64)> {
    let occupied = |p: &Path| {
        // \\?\ prefix so a deep already-occupied destination is detected rather
        // than mis-probed as free (std::fs silently fails past MAX_PATH).
        // `claimed` is keyed by the lowercased path string so a case-only
        // difference (NTFS/APFS are case-insensitive) still registers as taken.
        claimed.contains(&claimed_destination_key(p))
            || std::fs::symlink_metadata(crate::util::path_safety::to_extended_length(p)).is_ok()
    };
    if !occupied(dest) {
        return Ok((dest.to_path_buf(), start_suffix.max(2)));
    }
    let parent = dest.parent().unwrap_or_else(|| Path::new(""));
    let stem = dest
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = dest.extension().map(|e| e.to_string_lossy().into_owned());
    let mut n = start_suffix.max(2);
    loop {
        let name = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let candidate = parent.join(name);
        if !occupied(&candidate) {
            let next = n
                .checked_add(1)
                .context("destination suffix space exhausted")?;
            return Ok((candidate, next));
        }
        n = n
            .checked_add(1)
            .context("destination suffix space exhausted")?;
    }
}

/// Persist one bounded feedback batch, then release its path strings. Keeping
/// every successful pair until a million-file run completed duplicated the
/// whole plan in memory even though feedback recording itself is append-only.
fn record_feedback_batch(
    db: &Arc<Mutex<Connection>>,
    pairs: &mut Vec<(String, PathBuf)>,
) {
    if pairs.is_empty() {
        return;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    restructure_feedback::record(
        db,
        pairs.iter().map(|(s, d)| (Path::new(s), d.as_path())),
        now,
    );
    pairs.clear();
}

/// Consume `restructure_recover.ndjson` once at engine startup: for each
/// recorded (file_id → dst) whose DB row still names the exact recorded source
/// and whose durable file identity matches both the row and the live destination,
/// realign the stale `path_text` to `dst`. Recovery fails closed when identity is
/// unavailable (for example, exFAT/network volumes) rather than letting a stale
/// or tampered sidecar repoint a row to unrelated bytes. The write-ahead Undo
/// journal and the next scan remain the recovery routes in that case. The file is
/// cleared after one pass; records that cannot heal are dropped as best-effort.
/// Returns the number of rows realigned.
pub fn reconcile_pending_path_updates(db: &Arc<Mutex<Connection>>) -> usize {
    let Ok(trash) = crate::paths::trash_log_path() else {
        return 0;
    };
    let Some(dir) = trash.parent() else {
        return 0;
    };
    let path = dir.join("restructure_recover.ndjson");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return 0; // no record file → nothing to do
    };
    let mut healed = 0usize;
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(line) else {
            continue; // torn/partial line — skip
        };
        let (Some(file_id), Some(src), Some(dst), Some(root)) = (
            rec.get("file_id").and_then(|v| v.as_i64()),
            rec.get("src").and_then(|v| v.as_str()),
            rec.get("dst").and_then(|v| v.as_str()),
            rec.get("library_root").and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let Ok(recorded_identity) = serde_json::from_value::<crate::platform::FileIdentity>(
            rec.get("identity").cloned().unwrap_or(serde_json::Value::Null),
        ) else {
            continue;
        };
        let Ok(canonical_root) = canonicalize_safely(Path::new(root)) else {
            continue;
        };
        if ensure_inside_root(Path::new(src), &canonical_root).is_err()
            || ensure_inside_root(Path::new(dst), &canonical_root).is_err()
        {
            continue;
        }
        // Only heal when the file is actually where the record says it is.
        if !Path::new(dst).is_file() {
            continue;
        }
        let current: Option<(String, Option<i64>)> = {
            let conn = db.lock();
            conn.query_row(
                "SELECT path_text, file_ref FROM files WHERE id = ?1",
                rusqlite::params![file_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()
        };
        let Some((current_path, db_ref)) = current else {
            continue;
        };
        if paths_equal(&current_path, dst) {
            continue;
        }
        let root_owned = {
            let conn = db.lock();
            conn.prepare_cached("SELECT root_path FROM scan_sessions")
                .and_then(|mut stmt| {
                    let roots = stmt.query_map([], |row| row.get::<_, String>(0))?;
                    for candidate in roots {
                        if candidate.is_ok_and(|candidate| paths_equal(&candidate, root)) {
                            return Ok(true);
                        }
                    }
                    Ok(false)
                })
                .unwrap_or(false)
        };
        let live_identity = crate::platform::file_identity(Path::new(dst));
        if !root_owned
            || !paths_equal(&current_path, src)
            || db_ref.map(|value| value as u64) != Some(recorded_identity.file)
            || live_identity != Some(recorded_identity)
        {
            continue;
        }
        if update_path_in_db(db, file_id, Path::new(dst)).is_ok() {
            healed += 1;
        }
    }
    // Best-effort single-pass consumption: clear the record file regardless so
    // it can't grow unbounded or re-heal a since-moved file.
    let _ = std::fs::remove_file(&path);
    if healed > 0 {
        tracing::info!(healed, "[RESTRUCTURE] reconciled stale path_text from recovery record");
    }
    healed
}

/// B5: best-effort durable record of a successful on-disk move whose DB
/// path-update failed. The volume-qualified identity and canonical library root
/// bind startup reconciliation to the same file and owned tree checked during
/// the move; records without both proofs are never applied automatically.
fn record_path_update_failure(
    file_id: i64,
    src: &str,
    dst: &Path,
    identity: Option<crate::platform::FileIdentity>,
    library_root: &Path,
) {
    let Ok(trash) = crate::paths::trash_log_path() else {
        return;
    };
    let Some(dir) = trash.parent() else {
        return;
    };
    let path = dir.join("restructure_recover.ndjson");
    let line = serde_json::json!({
        "file_id": file_id,
        "src": src,
        "dst": dst.to_string_lossy(),
        "identity": identity,
        "library_root": library_root.to_string_lossy(),
    })
    .to_string();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = writeln!(f, "{line}");
        let _ = f.sync_all();
    }
}

/// Canonicalize a path, treating a missing target as "exists in spirit".
/// Walks up to the closest existing ancestor and canonicalizes that —
/// the unresolved tail is appended back. Lets us containment-check
/// destinations that don't exist yet (we're about to create them).
fn canonicalize_safely(p: &Path) -> Result<PathBuf> {
    if let Ok(c) = std::fs::canonicalize(p) {
        return Ok(c);
    }
    let mut cur = p.to_path_buf();
    let mut tail = PathBuf::new();
    while !cur.exists() {
        if let Some(name) = cur.file_name() {
            tail = if tail.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                Path::new(name).join(tail)
            };
        }
        if !cur.pop() {
            break;
        }
    }
    let mut canonical = std::fs::canonicalize(&cur)
        .with_context(|| format!("canonicalize ancestor {}", cur.display()))?;
    canonical.push(tail);
    Ok(canonical)
}

fn ensure_inside_root(dest: &Path, canonical_root: &Path) -> Result<()> {
    let canonical_dest = canonicalize_safely(dest)?;
    if !canonical_dest.starts_with(canonical_root) {
        anyhow::bail!(
            "destination {} is outside library root {}",
            canonical_dest.display(),
            canonical_root.display()
        );
    }
    Ok(())
}

/// SEC-5: walk every ancestor of `path` up to (but not including) `root`
/// and return true if any of them is a reparse point (junction or
/// symlink). Used as a TOCTOU defense before opening the destination parent:
/// even if the CANONICAL path checks out, an attacker who plants a junction in the
/// destination's parent BETWEEN the canonicalize call and the handle-bound rename
/// call would redirect the write outside library_root. Refusing moves
/// that pass through reparse points eliminates that surface.
#[cfg(windows)]
fn has_reparse_point_in_chain(parent: &Path, root: &Path) -> bool {
    use std::os::windows::fs::MetadataExt;
    use crate::util::path_safety::strip_extended_length;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    // `parent` is the raw (non-verbatim) destination parent from the IPC plan,
    // but `root` arrives canonicalized — on Windows that is a verbatim `\\?\C:\…`
    // path. Comparing the two prefix forms made `cur.starts_with(root)` false on
    // the FIRST iteration, so the walk broke after checking only the leaf parent
    // and never inspected intermediate ancestors — silently reducing the SEC-5
    // junction-TOCTOU defense to one level. Normalize BOTH operands with
    // strip_extended_length, which removes the `\\?\` prefix WITHOUT resolving the
    // link (std::fs::canonicalize must NOT be used here: it follows the junction
    // and defeats detection), so the ancestor walk runs up to the real root.
    let root_norm = strip_extended_length(root);
    let mut cur = parent.to_path_buf();
    loop {
        if let Ok(meta) = std::fs::symlink_metadata(&cur) {
            if (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0 {
                return true;
            }
        }
        // Stop once we reach (or pass) the root. Compared CASE-INSENSITIVELY and
        // component-wise: NTFS is case-insensitive, so a raw IPC parent that
        // differs only in casing from the canonical root (e.g. `d:\library\…` vs
        // canonical `D:\Library`) must still be recognized as inside it. Plain
        // `Path::starts_with` is case-sensitive and broke this walk after a single
        // level on any casing mismatch — silently reducing SEC-5 to one ancestor.
        // (audit F-A2)
        let cur_norm = strip_extended_length(&cur);
        let under = ci_starts_with(&cur_norm, &root_norm);
        let at_root = under && ci_starts_with(&root_norm, &cur_norm);
        if at_root || !under {
            break;
        }
        if !cur.pop() { break; }
    }
    false
}

/// Component-wise, case-insensitive prefix test (Windows NTFS is
/// case-insensitive). Unlike a lowercased-string `starts_with`, this respects
/// path-component boundaries so a sibling like `…\PhotosBackup` cannot
/// prefix-match `…\Photos`. (audit F-A2)
///
/// Folds with full Unicode `to_lowercase`, not `eq_ignore_ascii_case`: an
/// ASCII-only fold left a non-ASCII component (e.g. `Café` vs `CAFÉ`) compared
/// byte-exact, so a library root with a case-differing accented component made
/// `under` false on the first iteration and the SEC-5 reparse walk broke after
/// inspecting only the leaf parent — leaving every intermediate ancestor
/// unchecked. Unicode folding keeps the component-wise structure (siblings
/// still can't prefix-match) and only ever makes the walk continue further,
/// the conservative/safe direction. (audit R3-18)
#[cfg(windows)]
fn ci_starts_with(p: &Path, prefix: &Path) -> bool {
    let mut pc = p.components();
    for pre in prefix.components() {
        match pc.next() {
            Some(c)
                if c.as_os_str().to_string_lossy().to_lowercase()
                    == pre.as_os_str().to_string_lossy().to_lowercase() =>
            {
                continue
            }
            _ => return false,
        }
    }
    true
}

#[cfg(not(windows))]
fn has_reparse_point_in_chain(_parent: &Path, _root: &Path) -> bool { false }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_emit_apply_progress_cadence() {
        // Never on the zeroth processed item or with a zero interval.
        assert!(!should_emit_apply_progress(0, 1000, 500));
        assert!(!should_emit_apply_progress(500, 1000, 0));
        // First move (immediate feedback), every `interval`, and the last move.
        assert!(should_emit_apply_progress(1, 1000, 500));
        assert!(should_emit_apply_progress(500, 1000, 500));
        assert!(should_emit_apply_progress(1000, 1000, 500));
        // Silent on the in-between indices (so 100k moves → ~200 lines, not 100k).
        assert!(!should_emit_apply_progress(2, 1000, 500));
        assert!(!should_emit_apply_progress(499, 1000, 500));
        assert!(!should_emit_apply_progress(501, 1000, 500));
    }

    /// F-C6-013: a pre-cancelled apply must break before touching the filesystem
    /// — no move, and a cancel is NOT counted as a failure. Cross-platform: the
    /// cancel poll sits at the top of the loop, ahead of the (Windows-only)
    /// move_file, so the loop exits without reaching it.
    #[test]
    fn apply_honors_cancel_before_moving_any_file() {
        let root = std::env::temp_dir().join(format!("fileid-apply-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("a.jpg");
        std::fs::write(&src, b"data").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));

        // Already cancelled before apply runs.
        let cancel = Arc::new(AtomicBool::new(true));
        let apply = RestructureApply::new(db, root.clone(), false).with_cancel(cancel);
        let dest = root.join("Sorted").join("a.jpg").to_string_lossy().into_owned();
        let res = apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dest)])
            .unwrap();

        assert_eq!(res.applied, 0, "cancelled before any move applies");
        assert_eq!(res.failed, 0, "a cancel is not a failure");
        assert!(res.cancelled);
        assert_eq!(res.planned, Some(1));
        assert_eq!(res.remaining, Some(1));
        assert!(src.exists(), "source untouched by a cancelled apply");
        assert!(!root.join("Sorted").join("a.jpg").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A mid-stream plan read error (corrupt/vanished spool during a paged
    /// apply) must return the truthful PARTIAL result — moves already applied
    /// stay counted (so the app surfaces Undo) and the unread remainder is
    /// reported as failed — instead of aborting with Err, which the app maps
    /// to "your files are unchanged".
    #[test]
    fn stream_error_mid_apply_returns_partial_result() {
        let root = std::env::temp_dir().join(format!(
            "fileid-apply-stream-err-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("a.jpg");
        std::fs::write(&src, b"data").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));

        let apply = RestructureApply::new(db, root.clone(), false);
        let dest = root.join("Sorted").join("a.jpg").to_string_lossy().into_owned();
        let stream = vec![
            Ok(move_fixture(1, &src.to_string_lossy(), &dest)),
            Err(anyhow::anyhow!("spooled plan truncated")),
        ];
        let res = apply.apply_iter(stream, Some(3)).unwrap();

        assert_eq!(res.applied, 1, "the completed move stays counted");
        assert_eq!(res.failed, 2, "unread remainder (total 3 - 1 processed) reported as failed");
        assert!(!src.exists(), "first move really happened on disk");
        assert!(root.join("Sorted").join("a.jpg").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Regression (audit 2026-07 — rename-heal ON CONFLICT REPLACE, sibling site):
    /// `update_path_in_db` must NOT REPLACE-delete a LIVE row already occupying the
    /// destination path. `path_text` is UNIQUE ON CONFLICT REPLACE, so before the
    /// `UPDATE OR ABORT` fix a plain UPDATE onto an occupied path silently deleted
    /// the occupant + FK-cascaded its user data. This can happen mid-restructure
    /// after a transient earlier update failure desyncs a row.
    #[test]
    fn update_path_in_db_aborts_instead_of_clobbering_a_live_row() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, "C:/lib/A.jpg"); // row to be moved
        insert_file_row(&conn, 2, "C:/lib/B.jpg"); // LIVE row occupying the target
        // Give row 2 a user tag so a cascade would be observable.
        conn.execute(
            "INSERT INTO tags (file_id, tag, source, score) VALUES (2, 'Grandma', 'user', 1.0)",
            [],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));

        // Move row 1 onto B.jpg, which row 2 still owns → must error, not clobber.
        let res = update_path_in_db(&db, 1, Path::new("C:/lib/B.jpg"));
        assert!(res.is_err(), "colliding path update must abort, not silently REPLACE");

        let g = db.lock();
        let rows: i64 = g.query_row("SELECT COUNT(*) FROM files", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2, "both rows must survive the aborted update");
        let tag: i64 = g
            .query_row(
                "SELECT COUNT(*) FROM tags WHERE file_id = 2 AND tag = 'Grandma'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tag, 1, "the live row's user tag must not be FK-cascade-deleted");
    }

    #[test]
    fn update_path_in_db_rejects_a_disappeared_row() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));

        let error = update_path_in_db(&db, 404, Path::new("C:/lib/moved.jpg")).unwrap_err();
        assert!(error.to_string().contains("affected 0 rows"));
    }

    #[test]
    fn apply_reports_db_path_update_failure_and_keeps_undo() {
        let root = undo_fixture_root("db-update-failure");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.jpg");
        let destination = root.join("Sorted").join("source.jpg");
        std::fs::write(&source, b"source").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &source.to_string_lossy());
        insert_file_row(&conn, 2, &destination.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db.clone(), root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let result = apply
            .apply(&[move_fixture(
                1,
                &source.to_string_lossy(),
                &destination.to_string_lossy(),
            )])
            .unwrap();

        assert_eq!((result.applied, result.failed), (1, 1));
        assert!(!source.exists(), "the filesystem move already completed");
        assert!(destination.exists());
        assert!(journal.exists(), "the recovery boundary must remain available");
        let stored_path: String = db
            .lock()
            .query_row("SELECT path_text FROM files WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored_path, source.to_string_lossy());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// M2: after an apply moves the file on disk but FAILS the DB path update
    /// (a live UNIQUE conflict, as in the test above), undo must still restore
    /// the file to its original location using the journal's physical evidence
    /// — the DB-derived arms can't, because path_text still names the original.
    /// Before the fix, undo stale-skipped the entry and stranded the file.
    #[test]
    fn undo_restores_a_moved_but_db_update_failed_file() {
        let root = undo_fixture_root("undo-db-update-failure");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("source.jpg");
        let destination = root.join("Sorted").join("source.jpg");
        std::fs::write(&source, b"source").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &source.to_string_lossy());
        // A live row already occupies the destination path → apply's
        // update_path_in_db(1, destination) aborts on the UNIQUE conflict.
        insert_file_row(&conn, 2, &destination.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db.clone(), root.clone(), false)
            .with_undo_journal_path(journal.clone());

        // Forward apply: on-disk move succeeds, DB update fails.
        let fwd = apply
            .apply(&[move_fixture(
                1,
                &source.to_string_lossy(),
                &destination.to_string_lossy(),
            )])
            .unwrap();
        assert_eq!((fwd.applied, fwd.failed), (1, 1));
        assert!(!source.exists() && destination.exists());

        // The destination row (id 2) was only a fixture to force the conflict;
        // drop it so undo's move-back to `source` isn't itself blocked, then
        // undo the run.
        db.lock()
            .execute("DELETE FROM files WHERE id = 2", [])
            .unwrap();
        let undo = RestructureApply::new(db.clone(), root.clone(), false)
            .with_undo_journal_path(journal.clone())
            .undo_last()
            .unwrap();

        assert_eq!(undo.applied, 1, "undo must restore the stranded file");
        assert!(source.exists(), "file is back at its original path");
        assert!(!destination.exists(), "file left the post-move location");
        let restored: String = db
            .lock()
            .query_row("SELECT path_text FROM files WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert_eq!(restored, source.to_string_lossy(), "DB path realigned to disk");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_inside_root_accepts_canonical_descendant() {
        let tmp = std::env::temp_dir();
        let root = tmp.join("fileid-test-root");
        let _ = std::fs::create_dir_all(&root);
        let inside = root.join("Photos").join("2024").join("a.jpg");
        let canonical_root = canonicalize_safely(&root).unwrap();
        assert!(ensure_inside_root(&inside, &canonical_root).is_ok());
    }

    #[test]
    fn unique_destination_disambiguates_collisions() {
        let tmp = std::env::temp_dir().join("fileid-uniq-dest-test");
        let _ = std::fs::create_dir_all(&tmp);
        let dest = tmp.join("audio.mp3");
        // Nothing assigned, file absent → original name.
        let assigned0: HashSet<ClaimedDestination> = HashSet::new();
        assert_eq!(unique_destination(&dest, &assigned0).unwrap(), dest);
        // A second move targeting the same name in-batch → " (2)".
        let mut assigned1: HashSet<ClaimedDestination> = HashSet::new();
        assigned1.insert(claimed_destination_key(&dest));
        let d2 = unique_destination(&dest, &assigned1).unwrap();
        assert_eq!(d2, tmp.join("audio (2).mp3"));
        assert_ne!(d2, dest);
        // A file already on disk also forces disambiguation.
        std::fs::write(&dest, b"x").unwrap();
        let d3 = unique_destination(&dest, &assigned0).unwrap();
        assert_eq!(d3, tmp.join("audio (2).mp3"));
        let _ = std::fs::remove_file(&dest);
    }

    #[test]
    fn ensure_inside_root_rejects_traversal() {
        let tmp = std::env::temp_dir();
        let root = tmp.join("fileid-test-root2");
        let _ = std::fs::create_dir_all(&root);
        let canonical_root = canonicalize_safely(&root).unwrap();
        let outside = canonical_root.parent().unwrap().join("evil.jpg");
        assert!(ensure_inside_root(&outside, &canonical_root).is_err());
    }

    #[test]
    fn unique_destination_avoids_disk_and_claimed_collisions() {
        let dir = std::env::temp_dir().join(format!("fileid-uniqdest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dest = dir.join("IMG.jpg");

        // Free → returned as-is.
        let empty = HashSet::new();
        assert_eq!(unique_destination(&dest, &empty).unwrap(), dest);

        // On disk → bumped to " (2)".
        std::fs::write(&dest, b"x").unwrap();
        assert_eq!(
            unique_destination(&dest, &empty).unwrap(),
            dir.join("IMG (2).jpg")
        );

        // " (2)" also claimed this batch → bumped to " (3)".
        let mut claimed = HashSet::new();
        claimed.insert(claimed_destination_key(&dir.join("IMG (2).jpg")));
        assert_eq!(
            unique_destination(&dest, &claimed).unwrap(),
            dir.join("IMG (3).jpg")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// DATA-INTEGRITY: NTFS/APFS are case-insensitive by default, so a target
    /// claimed earlier in the batch as "photo.jpg" and a later move to
    /// "Photo.jpg" name the SAME file. The case-folded `claimed` key must catch
    /// this so the second move uniquifies instead of silently clobbering the
    /// first. Parity with `Restructure.swift`'s lowercased claimed set.
    #[test]
    fn unique_destination_detects_case_only_claimed_collision() {
        let dir = std::env::temp_dir().join(format!("fileid-uniqdest-ci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // First move claimed "photo.jpg" (stored lowercased, as `apply` does).
        let mut claimed = HashSet::new();
        claimed.insert(claimed_destination_key(&dir.join("photo.jpg")));

        // Second move targets the case-variant "Photo.jpg" — same file on a
        // case-insensitive FS → must be detected and bumped to " (2)".
        assert_eq!(
            unique_destination(&dir.join("Photo.jpg"), &claimed).unwrap(),
            dir.join("Photo (2).jpg")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn destination_claims_scale_past_ten_thousand_identical_basenames() {
        let dir = std::env::temp_dir().join(format!(
            "fileid-uniqdest-10k-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let destination = dir.join("Report.pdf");
        let mut claims = DestinationClaims::default();
        let mut reserved = PathBuf::new();

        for _ in 0..10_001 {
            reserved = claims.reserve(&destination).unwrap();
        }

        assert_eq!(reserved, dir.join("Report (10001).pdf"));
        assert_eq!(claims.claimed.len(), 10_001);
    }

    fn move_fixture(file_id: i64, source: &str, destination: &str) -> RestructureMove {
        RestructureMove {
            file_id,
            source: source.to_string(),
            destination: destination.to_string(),
            category: "Sorted".to_string(),
            tier: None,
            confidence: String::new(),
            reason: None,
        }
    }

    fn insert_file_row(conn: &Connection, id: i64, path: &str) {
        let file_ref = crate::platform::file_ref(Path::new(path)).map(|value| value as i64);
        conn.execute(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, failed, file_ref) \
             VALUES (?1, ?2, 0, 4, 0.0, 'image', 'jpg', 0, ?3)",
            params![id, path, file_ref],
        )
        .unwrap();
    }

    #[test]
    fn strict_apply_never_invents_a_destination_after_preflight() {
        let root = std::env::temp_dir().join(format!(
            "fileid-strict-race-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let incoming = root.join("incoming");
        let sorted = root.join("Sorted");
        std::fs::create_dir_all(&incoming).unwrap();
        std::fs::create_dir_all(&sorted).unwrap();
        let source = incoming.join("photo.jpg");
        let destination = sorted.join("photo.jpg");
        std::fs::write(&source, b"planned").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &source.to_string_lossy());
        let journal_path = root.join("undo.json");
        let apply = RestructureApply::new(
            Arc::new(Mutex::new(conn)),
            root.clone(),
            false,
        )
        .with_strict_destinations()
        .with_undo_journal_path(journal_path.clone());
        let move_ = move_fixture(
            1,
            &source.to_string_lossy(),
            &destination.to_string_lossy(),
        );
        {
            let mut preflight = apply.begin_forward_preflight().unwrap();
            preflight.validate(&move_).unwrap();
        }

        std::fs::write(&destination, b"raced").unwrap();
        let result = apply.apply(&[move_]).unwrap();

        assert_eq!((result.applied, result.failed), (0, 1));
        assert_eq!(std::fs::read(&source).unwrap(), b"planned");
        assert_eq!(std::fs::read(&destination).unwrap(), b"raced");
        assert!(!sorted.join("photo (2).jpg").exists());
        assert!(!journal_path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn undo_retry_treats_an_already_restored_entry_as_idempotent() {
        let root = std::env::temp_dir().join(format!(
            "fileid-undo-retry-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let original_dir = root.join("incoming");
        std::fs::create_dir_all(&original_dir).unwrap();
        let original = original_dir.join("photo.jpg");
        std::fs::write(&original, b"photo").unwrap();
        let already_vacated = root.join("Photos").join("photo.jpg");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &original.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let apply = RestructureApply::new(db, root.clone(), false);
        let inverse = move_fixture(
            1,
            &already_vacated.to_string_lossy(),
            &original.to_string_lossy(),
        );

        let result = apply
            .apply_iter_with(std::iter::once(Ok(inverse)), Some(1), false)
            .unwrap();
        assert_eq!(result.applied, 0);
        assert_eq!(result.failed, 0, "retry must not become permanently stale");
        assert!(original.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn undo_retry_reconciles_a_completed_move_after_db_update_failure() {
        let root = undo_fixture_root("undo-db-retry");
        std::fs::create_dir_all(&root).unwrap();
        let original = root.join("photo.jpg");
        let sorted = root.join("Photos").join("photo.jpg");
        std::fs::write(&original, b"photo").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &original.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db.clone(), root.clone(), false)
            .with_undo_journal_path(journal.clone());
        let forward = apply
            .apply(&[move_fixture(
                1,
                &original.to_string_lossy(),
                &sorted.to_string_lossy(),
            )])
            .unwrap();
        assert_eq!((forward.applied, forward.failed), (1, 0));

        db.lock()
            .execute_batch(
                "CREATE TRIGGER fail_undo_path BEFORE UPDATE OF path_text ON files
                 WHEN OLD.id = 1
                 BEGIN
                     SELECT RAISE(ABORT, 'injected undo path failure');
                 END;",
            )
            .unwrap();
        let first = apply.undo_last().unwrap();
        assert_eq!((first.applied, first.failed), (1, 1));
        assert!(original.exists());
        assert!(!sorted.exists());
        assert!(journal.exists());
        let stale_db_path: String = db
            .lock()
            .query_row("SELECT path_text FROM files WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert!(paths_equal(&stale_db_path, &sorted.to_string_lossy()));

        db.lock().execute_batch("DROP TRIGGER fail_undo_path;").unwrap();
        let retry = apply.undo_last().unwrap();
        assert_eq!((retry.applied, retry.failed), (0, 0));
        let repaired_db_path: String = db
            .lock()
            .query_row("SELECT path_text FROM files WHERE id = 1", [], |row| row.get(0))
            .unwrap();
        assert!(paths_equal(&repaired_db_path, &original.to_string_lossy()));
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn undo_fixture_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "fileid-{tag}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ))
    }

    /// Dependent moves (A→Albums/X, then B→A's vacated slot) must undo
    /// NEWEST-FIRST: forward replay restores A into the slot B still occupies
    /// and uniquifies it into "A (2).txt" — silent corruption. (audit 2026-07-14)
    #[test]
    fn undo_restores_dependent_moves_in_reverse_order() {
        let root = undo_fixture_root("undo-reverse");
        std::fs::create_dir_all(&root).unwrap();
        let a = root.join("A.txt");
        let b = root.join("B.txt");
        std::fs::write(&a, b"AAAA").unwrap();
        std::fs::write(&b, b"BBBB").unwrap();
        let a_new = root.join("Albums").join("X.txt");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &a.to_string_lossy());
        insert_file_row(&conn, 2, &b.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let moves = vec![
            move_fixture(1, &a.to_string_lossy(), &a_new.to_string_lossy()),
            move_fixture(2, &b.to_string_lossy(), &a.to_string_lossy()),
        ];
        let res = apply.apply(&moves).unwrap();
        assert_eq!((res.applied, res.failed), (2, 0));
        assert_eq!(std::fs::read(&a).unwrap(), b"BBBB", "B took A's vacated slot");
        assert_eq!(std::fs::read(&a_new).unwrap(), b"AAAA");

        let undo = apply.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (2, 0), "undo must fully restore");
        assert_eq!(std::fs::read(&a).unwrap(), b"AAAA", "A restored to its own slot");
        assert_eq!(std::fs::read(&b).unwrap(), b"BBBB", "B restored home");
        assert!(!a_new.exists());
        assert!(
            !root.join("A (2).txt").exists(),
            "forward-order replay corruption: A was uniquified instead of restored"
        );
        assert!(!journal.exists(), "completed undo clears the journal");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A torn trailing journal entry (crash mid-append, before its fsync — so
    /// its move never executed) must not abort the undo of every valid,
    /// durable entry before it. (audit 2026-07-14)
    #[test]
    fn undo_tolerates_a_torn_trailing_journal_entry() {
        let root = undo_fixture_root("undo-torn");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("photo.jpg");
        std::fs::write(&src, b"PIC").unwrap();
        let dst = root.join("Sorted").join("photo.jpg");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let res = apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!((res.applied, res.failed), (1, 0));

        {
            use std::io::Write as _;
            let mut f = OpenOptions::new().append(true).open(&journal).unwrap();
            f.write_all(b"{\"file_id\":9,\"fro").unwrap();
        }

        let undo = apply.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (1, 0), "valid entry still undone");
        assert!(src.exists(), "file restored despite the torn tail");
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Fail-closed: if the undo journal cannot open, a recorded apply must
    /// abort BEFORE any file moves — undo protection is a precondition, not
    /// best-effort. (audit 2026-07-14; macOS parity)
    #[test]
    fn unopenable_undo_journal_aborts_apply_before_any_move() {
        let root = undo_fixture_root("undo-noopen");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("doc.txt");
        std::fs::write(&src, b"DOC").unwrap();
        let dst = root.join("Docs").join("doc.txt");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        // A DIRECTORY at the journal path makes the file open fail.
        let journal = root.join("undo.ndjson");
        std::fs::create_dir_all(&journal).unwrap();
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let res = apply.apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())]);
        assert!(res.is_err(), "apply must fail closed without a journal");
        assert!(src.exists(), "nothing may move without undo protection");
        assert!(!dst.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A move that fails AFTER its write-ahead entry landed must roll the
    /// entry back, so undo never replays a phantom; later entries continue
    /// cleanly at the rolled-back offset. (audit 2026-07-14)
    #[test]
    fn failed_move_rolls_back_its_journal_entry() {
        let root = undo_fixture_root("undo-rollback");
        std::fs::create_dir_all(&root).unwrap();
        let missing = root.join("ghost.txt"); // DB row exists, file does not
        let real = root.join("real.txt");
        std::fs::write(&real, b"REAL").unwrap();
        let dst_missing = root.join("Sorted").join("ghost.txt");
        let dst_real = root.join("Sorted").join("real.txt");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &missing.to_string_lossy());
        insert_file_row(&conn, 2, &real.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let moves = vec![
            move_fixture(1, &missing.to_string_lossy(), &dst_missing.to_string_lossy()),
            move_fixture(2, &real.to_string_lossy(), &dst_real.to_string_lossy()),
        ];
        let res = apply.apply(&moves).unwrap();
        assert_eq!((res.applied, res.failed), (1, 1));

        let journal_text = std::fs::read_to_string(&journal).unwrap();
        assert_eq!(
            journal_text.lines().count(),
            2,
            "phantom entry must be rolled back: {journal_text:?}"
        );
        assert!(journal_text.contains("real.txt"));

        let undo = apply.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (1, 0), "no phantom replay");
        assert!(real.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn undo_ignores_a_valid_but_unterminated_trailing_entry() {
        let root = undo_fixture_root("undo-valid-torn");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("photo.jpg");
        let dst = root.join("Sorted").join("photo.jpg");
        std::fs::write(&src, b"PIC").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());
        apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        let phantom = serde_json::json!({
            "file_id": 9,
            "from": root.join("Sorted").join("phantom.jpg"),
            "to": root.join("phantom.jpg")
        })
        .to_string();
        {
            use std::io::Write as _;
            let mut file = OpenOptions::new().append(true).open(&journal).unwrap();
            file.write_all(phantom.as_bytes()).unwrap();
        }

        let undo = apply.undo_last().unwrap();

        assert_eq!((undo.applied, undo.failed), (1, 0));
        assert!(src.exists());
        assert!(!dst.exists());
        assert!(!journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restart_recovers_prior_journal_when_replacement_never_committed() {
        let root = undo_fixture_root("undo-prior-recovery");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("photo.jpg");
        let dst = root.join("Photos").join("photo.jpg");
        std::fs::write(&src, b"JPEG").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());
        let result = apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!((result.applied, result.failed), (1, 0));

        let replacement =
            UndoJournal::open_replacing(Some(journal.clone()), &root).unwrap();
        drop(replacement);
        assert_eq!(
            scan_undo_journal_spans(&journal)
                .unwrap()
                .unwrap()
                .spans
                .len(),
            0
        );
        assert_eq!(prior_undo_journal_backups(&journal).unwrap().len(), 1);

        let undo = apply.undo_last().unwrap();

        assert_eq!((undo.applied, undo.failed), (1, 0));
        assert!(src.exists());
        assert!(!dst.exists());
        assert!(!journal.exists());
        assert!(prior_undo_journal_backups(&journal).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restart_keeps_the_current_journal_when_its_first_move_reached_disk() {
        let root = undo_fixture_root("undo-current-committed-recovery");
        std::fs::create_dir_all(&root).unwrap();
        let prior_source = root.join("prior.jpg");
        let prior_destination = root.join("Photos").join("prior.jpg");
        let current_source = root.join("current.jpg");
        let current_destination = root.join("Photos").join("current.jpg");
        std::fs::write(&prior_source, b"PRIOR").unwrap();
        std::fs::write(&current_source, b"CURRENT").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &prior_source.to_string_lossy());
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(
            Arc::new(Mutex::new(conn)),
            root.clone(),
            false,
        )
        .with_undo_journal_path(journal.clone());
        apply
            .apply(&[move_fixture(
                1,
                &prior_source.to_string_lossy(),
                &prior_destination.to_string_lossy(),
            )])
            .unwrap();

        let current_identity = crate::platform::file_identity(&current_source).unwrap();
        let mut replacement =
            UndoJournal::open_replacing(Some(journal.clone()), &root).unwrap();
        replacement
            .append_ahead(
                2,
                &current_destination.to_string_lossy(),
                &current_source.to_string_lossy(),
                current_identity,
            )
            .unwrap();
        std::fs::create_dir_all(current_destination.parent().unwrap()).unwrap();
        crate::util::rename_no_replace(&current_source, &current_destination).unwrap();
        drop(replacement);

        recover_prior_undo_journal(&journal, &root).unwrap();

        assert!(prior_undo_journal_backups(&journal).unwrap().is_empty());
        assert_eq!(
            scan_undo_journal_spans(&journal)
                .unwrap()
                .unwrap()
                .spans
                .len(),
            1
        );
        assert!(!current_source.exists());
        assert_eq!(
            crate::platform::file_identity(&current_destination),
            Some(current_identity)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restart_restores_the_prior_journal_when_write_ahead_move_never_started() {
        let root = undo_fixture_root("undo-current-uncommitted-recovery");
        std::fs::create_dir_all(&root).unwrap();
        let prior_source = root.join("prior.jpg");
        let prior_destination = root.join("Photos").join("prior.jpg");
        let current_source = root.join("current.jpg");
        let current_destination = root.join("Photos").join("current.jpg");
        std::fs::write(&prior_source, b"PRIOR").unwrap();
        std::fs::write(&current_source, b"CURRENT").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &prior_source.to_string_lossy());
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(
            Arc::new(Mutex::new(conn)),
            root.clone(),
            false,
        )
        .with_undo_journal_path(journal.clone());
        apply
            .apply(&[move_fixture(
                1,
                &prior_source.to_string_lossy(),
                &prior_destination.to_string_lossy(),
            )])
            .unwrap();
        let prior_identity = crate::platform::file_identity(&journal).unwrap();

        let current_identity = crate::platform::file_identity(&current_source).unwrap();
        let mut replacement =
            UndoJournal::open_replacing(Some(journal.clone()), &root).unwrap();
        replacement
            .append_ahead(
                2,
                &current_destination.to_string_lossy(),
                &current_source.to_string_lossy(),
                current_identity,
            )
            .unwrap();
        drop(replacement);

        recover_prior_undo_journal(&journal, &root).unwrap();

        assert!(prior_undo_journal_backups(&journal).unwrap().is_empty());
        assert_eq!(
            crate::platform::file_identity(&journal),
            Some(prior_identity)
        );
        assert!(current_source.exists());
        assert!(!current_destination.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_fails_closed_when_current_and_prior_journals_both_have_work() {
        let root = undo_fixture_root("undo-prior-ambiguous");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("photo.jpg");
        let dst = root.join("Photos").join("photo.jpg");
        std::fs::write(&src, b"JPEG").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());
        apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();

        let mut replacement =
            UndoJournal::open_replacing(Some(journal.clone()), &root).unwrap();
        replacement
            .append_ahead(
                2,
                &root.join("New").join("other.jpg").to_string_lossy(),
                &root.join("other.jpg").to_string_lossy(),
                crate::platform::file_identity(&dst).unwrap(),
            )
            .unwrap();
        drop(replacement);

        let error = apply.undo_last().unwrap_err();

        assert!(error.to_string().contains("both contain work"));
        assert!(journal.exists());
        assert_eq!(prior_undo_journal_backups(&journal).unwrap().len(), 1);
        assert!(dst.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn recovery_rejects_a_preserved_journal_from_another_root() {
        let root_a = undo_fixture_root("undo-prior-owner-a");
        let root_b = undo_fixture_root("undo-prior-owner-b");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        let journal = root_a.join("undo.ndjson");
        drop(UndoJournal::open_replacing(Some(journal.clone()), &root_a).unwrap());
        drop(UndoJournal::open_replacing(Some(journal.clone()), &root_b).unwrap());
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let wrong_root = RestructureApply::new(
            Arc::new(Mutex::new(conn)),
            root_b.clone(),
            false,
        )
        .with_undo_journal_path(journal.clone());

        let error = wrong_root.undo_last().unwrap_err();

        assert!(error.to_string().contains("different library root"));
        assert!(journal.exists());
        assert_eq!(prior_undo_journal_backups(&journal).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&root_a);
        let _ = std::fs::remove_dir_all(&root_b);
    }

    #[test]
    fn first_failed_move_restores_the_prior_undo_journal() {
        let root = undo_fixture_root("undo-preserve-first-failure");
        std::fs::create_dir_all(&root).unwrap();
        let first_source = root.join("first.txt");
        let second_source = root.join("second.txt");
        std::fs::write(&first_source, b"FIRST").unwrap();
        std::fs::write(&second_source, b"SECOND").unwrap();
        let first_destination = root.join("Sorted").join("first.txt");
        let second_destination = root.join("Sorted").join("second.txt");
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &first_source.to_string_lossy());
        insert_file_row(&conn, 2, &second_source.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");

        let first_apply = RestructureApply::new(db.clone(), root.clone(), false)
            .with_undo_journal_path(journal.clone());
        let first = first_apply
            .apply(&[move_fixture(
                1,
                &first_source.to_string_lossy(),
                &first_destination.to_string_lossy(),
            )])
            .unwrap();
        assert_eq!((first.applied, first.failed), (1, 0));
        let prior_journal = std::fs::read(&journal).unwrap();

        let failed_apply = RestructureApply::new(db.clone(), root.clone(), false)
            .with_undo_journal_path(journal.clone())
            .with_fail_next_move_after_journal();
        let failed = failed_apply
            .apply(&[move_fixture(
                2,
                &second_source.to_string_lossy(),
                &second_destination.to_string_lossy(),
            )])
            .unwrap();

        assert_eq!((failed.applied, failed.failed), (0, 1));
        assert_eq!(std::fs::read(&journal).unwrap(), prior_journal);
        assert!(second_source.exists());
        assert!(!second_destination.exists());
        let undo = failed_apply.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (1, 0));
        assert!(first_source.exists());
        assert!(!first_destination.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    /// An apply that journals nothing (here: a pure no-op move) must NOT
    /// truncate the previous run's journal — that undo history is the user's
    /// only path back. (audit 2026-07-14)
    #[test]
    fn non_journaling_apply_preserves_the_prior_journal() {
        let root = undo_fixture_root("undo-preserve");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("song.mp3");
        std::fs::write(&src, b"MP3").unwrap();
        let dst = root.join("Music").join("song.mp3");

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());

        let res = apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!((res.applied, res.failed), (1, 0));
        let first_journal = std::fs::read_to_string(&journal).unwrap();
        assert_eq!(first_journal.lines().count(), 2);

        // Second run: the file is already exactly where the plan wants it — a
        // no-op that journals nothing and must leave run 1's journal intact.
        let res2 = apply
            .apply(&[move_fixture(1, &dst.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!((res2.applied, res2.failed), (0, 0));
        assert_eq!(
            std::fs::read_to_string(&journal).unwrap(),
            first_journal,
            "a non-journaling apply truncated the prior undo journal"
        );

        let undo = apply.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (1, 0));
        assert!(src.exists(), "run 1 still undoable after the no-op run");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn undo_rejects_a_journal_from_another_library_root() {
        let root_a = undo_fixture_root("undo-owner-a");
        let root_b = undo_fixture_root("undo-owner-b");
        std::fs::create_dir_all(&root_a).unwrap();
        std::fs::create_dir_all(&root_b).unwrap();
        let src = root_a.join("photo.jpg");
        let dst = root_a.join("Photos").join("photo.jpg");
        std::fs::write(&src, b"JPEG").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root_a.join("undo.ndjson");
        let apply_a = RestructureApply::new(db.clone(), root_a.clone(), false)
            .with_undo_journal_path(journal.clone());
        let result = apply_a
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!((result.applied, result.failed), (1, 0));

        let wrong_root = RestructureApply::new(db.clone(), root_b.clone(), false)
            .with_undo_journal_path(journal.clone());
        let err = wrong_root.undo_last().unwrap_err();
        assert!(err.to_string().contains("different library root"));
        assert!(dst.exists(), "wrong-root undo must not move the journaled file");
        assert!(journal.exists(), "wrong-root rejection must preserve retry evidence");

        let undo = apply_a.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (1, 0));
        assert!(src.exists());
        let _ = std::fs::remove_dir_all(&root_a);
        let _ = std::fs::remove_dir_all(&root_b);
    }

    #[test]
    fn undo_rejects_a_nested_journal_when_parent_root_is_selected() {
        let parent = undo_fixture_root("undo-owner-parent");
        let child = parent.join("ChildLibrary");
        std::fs::create_dir_all(&child).unwrap();
        let src = child.join("photo.jpg");
        let dst = child.join("Photos").join("photo.jpg");
        std::fs::write(&src, b"JPEG").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = parent.join("undo.ndjson");
        let apply_child = RestructureApply::new(db.clone(), child.clone(), false)
            .with_undo_journal_path(journal.clone());
        let result = apply_child
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!((result.applied, result.failed), (1, 0));

        let wrong_parent = RestructureApply::new(db, parent.clone(), false)
            .with_undo_journal_path(journal.clone());
        let err = wrong_parent.undo_last().unwrap_err();
        assert!(err.to_string().contains("different library root"));
        assert!(dst.exists());
        assert!(journal.exists());

        let undo = apply_child.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (1, 0));
        assert!(src.exists());
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn undo_rejects_a_legacy_rootless_journal() {
        let root = undo_fixture_root("undo-owner-legacy");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("photo.jpg");
        let dst = root.join("Photos").join("photo.jpg");
        std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
        std::fs::write(&dst, b"JPEG").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &dst.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let mut entry = serde_json::json!({
            "file_id": 1,
            "from": dst.to_string_lossy(),
            "to": src.to_string_lossy()
        })
        .to_string();
        entry.push('\n');
        std::fs::write(&journal, entry).unwrap();

        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone());
        let err = apply.undo_last().unwrap_err();
        assert!(err.to_string().contains("predates exact library-root ownership"));
        assert!(dst.exists());
        assert!(journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn header_only_owned_undo_journal_is_removed_as_no_work() {
        let root = undo_fixture_root("undo-header-only");
        std::fs::create_dir_all(&root).unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        drop(UndoJournal::open_replacing(Some(journal.clone()), &root).unwrap());

        let result = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone())
            .undo_last()
            .unwrap();
        assert_eq!((result.applied, result.failed), (0, 0));
        assert!(!journal.exists(), "empty owned journal must not survive as phantom Undo");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancelled_undo_reports_retryable_incomplete_result() {
        let root = undo_fixture_root("undo-cancel-retry");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("photo.jpg");
        let dst = root.join("Photos").join("photo.jpg");
        std::fs::write(&src, b"JPEG").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let apply = RestructureApply::new(db.clone(), root.clone(), false)
            .with_undo_journal_path(journal.clone());
        let result = apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!((result.applied, result.failed), (1, 0));

        let cancel = Arc::new(AtomicBool::new(true));
        let cancelled = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone())
            .with_cancel(cancel)
            .undo_last()
            .unwrap();
        assert_eq!((cancelled.applied, cancelled.failed), (0, 0));
        assert!(cancelled.cancelled);
        assert_eq!(cancelled.remaining, Some(1));
        assert!(dst.exists());
        assert!(journal.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancel_arriving_after_completed_undo_does_not_preserve_a_stale_journal() {
        let root = undo_fixture_root("undo-cancel-after-replay");
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("photo.jpg");
        let dst = root.join("Photos").join("photo.jpg");
        std::fs::write(&src, b"JPEG").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));
        let journal = root.join("undo.ndjson");
        let cancel = Arc::new(AtomicBool::new(false));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(journal.clone())
            .with_cancel(cancel.clone())
            .with_cancel_after_undo_replay();
        let result = apply
            .apply(&[move_fixture(1, &src.to_string_lossy(), &dst.to_string_lossy())])
            .unwrap();
        assert_eq!((result.applied, result.failed), (1, 0));

        let undo = apply.undo_last().unwrap();
        assert_eq!((undo.applied, undo.failed), (1, 0));
        assert!(!undo.cancelled);
        assert!(cancel.load(Ordering::Relaxed));
        assert!(src.exists());
        assert!(!dst.exists());
        assert!(
            !journal.exists(),
            "the completed replay result, not a later raw token race, owns cleanup"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// B3: two distinct sources sharing a basename, funnelled to the same
    /// destination, must BOTH survive — the second is uniquified, never
    /// clobbered. Windows-only: exercises the real handle-relative rename path; the
    /// portable std::fs move path is covered by the not(windows) tests below.
    #[test]
    #[cfg(windows)]
    fn apply_two_same_basename_sources_keeps_both() {
        let root = std::env::temp_dir().join(format!("fileid-apply-both-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let a_dir = root.join("a");
        let b_dir = root.join("b");
        let dest_dir = root.join("Sorted");
        std::fs::create_dir_all(&a_dir).unwrap();
        std::fs::create_dir_all(&b_dir).unwrap();
        let src_a = a_dir.join("IMG_0001.jpg");
        let src_b = b_dir.join("IMG_0001.jpg");
        std::fs::write(&src_a, b"AAAA").unwrap();
        std::fs::write(&src_b, b"BBBB").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &src_a.to_string_lossy());
        insert_file_row(&conn, 2, &src_b.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));

        let apply = RestructureApply::new(db, root.clone(), false);
        let dest = dest_dir.join("IMG_0001.jpg").to_string_lossy().into_owned();
        let moves = vec![
            move_fixture(1, &src_a.to_string_lossy(), &dest),
            move_fixture(2, &src_b.to_string_lossy(), &dest),
        ];
        let res = apply.apply(&moves).unwrap();

        assert_eq!(res.applied, 2, "both moves applied");
        assert_eq!(res.failed, 0);
        let first = dest_dir.join("IMG_0001.jpg");
        let second = dest_dir.join("IMG_0001 (2).jpg");
        assert!(first.exists() && second.exists(), "both files survived under distinct names");
        // No clobber: the two original payloads are both present.
        let mut bodies = std::collections::HashSet::new();
        bodies.insert(std::fs::read(&first).unwrap());
        bodies.insert(std::fs::read(&second).unwrap());
        assert!(bodies.contains(b"AAAA".as_slice()) && bodies.contains(b"BBBB".as_slice()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(windows)]
    fn handle_bound_move_rejects_the_wrong_volume_qualified_identity() {
        let root = std::env::temp_dir().join(format!("fileid-handle-move-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("dest")).unwrap();
        let source = root.join("source.bin");
        let destination = root.join("dest").join("source.bin");
        std::fs::write(&source, b"payload").unwrap();
        let identity = crate::platform::file_identity(&source).unwrap();
        let wrong = crate::platform::FileIdentity {
            volume: identity.volume.wrapping_add(1),
            file: identity.file,
        };

        let canonical_root = canonicalize_safely(&root).unwrap();
        assert!(move_file(&source.to_string_lossy(), &destination, wrong, &canonical_root).is_err());
        assert!(source.exists());
        assert!(!destination.exists());
        move_file(&source.to_string_lossy(), &destination, identity, &canonical_root).unwrap();
        assert!(!source.exists());
        assert_eq!(std::fs::read(&destination).unwrap(), b"payload");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// R3-18: ci_starts_with must fold NON-ASCII case (NTFS is case-insensitive
    /// for accented letters too), or the SEC-5 reparse-point walk breaks early
    /// on a library root with a case-differing accented component. The
    /// component-wise structure must still reject a sibling prefix.
    #[test]
    #[cfg(windows)]
    fn ci_starts_with_folds_non_ascii_and_respects_boundaries() {
        use std::path::Path;
        assert!(
            ci_starts_with(Path::new(r"D:\Photos\CAFÉ\2024"), Path::new(r"D:\Photos\café")),
            "non-ASCII case must fold (NTFS is case-insensitive for accented letters)"
        );
        assert!(
            !ci_starts_with(Path::new(r"D:\PhotosBackup"), Path::new(r"D:\Photos")),
            "a sibling must not prefix-match (component boundaries respected)"
        );
    }

    /// B4: a move whose source no longer matches the live DB row for its
    /// file_id is a stale plan and must be skipped, not executed.
    #[test]
    fn apply_skips_stale_move_when_source_mismatches_db() {
        let root = std::env::temp_dir().join(format!("fileid-apply-stale-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let real = root.join("real.jpg");
        std::fs::write(&real, b"data").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        // The DB says file 1 lives at `real`, but the (stale) plan claims a
        // different source path.
        insert_file_row(&conn, 1, &real.to_string_lossy());
        let db = Arc::new(Mutex::new(conn));

        let apply = RestructureApply::new(db, root.clone(), false);
        let stale_src = root.join("vanished.jpg").to_string_lossy().into_owned();
        let dest = root.join("Sorted").join("x.jpg").to_string_lossy().into_owned();
        let res = apply.apply(&[move_fixture(1, &stale_src, &dest)]).unwrap();

        assert_eq!(res.applied, 0, "stale move must not apply");
        assert_eq!(res.failed, 1);
        assert!(real.exists(), "the real file must be untouched");
        assert!(!root.join("Sorted").join("x.jpg").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn restructure_identity_fails_closed_and_preserves_high_bit_refs() {
        let matching = crate::platform::FileIdentity { volume: 7, file: 100 };
        let high_bit = crate::platform::FileIdentity { volume: 7, file: u64::MAX };
        assert_eq!(identity_matching_db_ref(Some(100), Some(matching)), Some(matching));
        assert_eq!(identity_matching_db_ref(Some(-1), Some(high_bit)), Some(high_bit));
        assert!(identity_matching_db_ref(Some(100), Some(high_bit)).is_none());
        assert!(identity_matching_db_ref(None, Some(matching)).is_none());
        assert!(identity_matching_db_ref(Some(100), None).is_none());
    }

    /// R-#14: a real same-path swap — the DB recorded one file_ref for the planned
    /// file, but a DIFFERENT file now occupies that exact path — must be skipped, not
    /// moved. Windows-only: needs a live NTFS file_ref (the non-Windows
    /// `platform::file_ref` stub returns None, leaving the guard inert — the macOS
    /// engine's inode-based mirror has its own integration test).
    #[test]
    #[cfg(windows)]
    fn apply_skips_move_when_file_ref_swapped() {
        let root = std::env::temp_dir().join(format!("fileid-apply-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("doc.pdf");
        std::fs::write(&src, b"SWAPPED-IN").unwrap();
        // The file actually on disk now. If the volume has no readable ref the guard
        // can't engage — skip the assertion rather than fail spuriously.
        let Some(real_ref) = crate::platform::file_ref(&src) else {
            let _ = std::fs::remove_dir_all(&root);
            return;
        };

        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        // DB row names the SAME path but a DIFFERENT file_ref — the file we planned to
        // move, since replaced on disk by another. `real_ref ^ 1` is guaranteed != real.
        conn.execute(
            "INSERT INTO files (id, path_text, path_hash, size_bytes, scanned_at, kind, extension, failed, file_ref) \
             VALUES (1, ?1, 0, 10, 0.0, 'doc', 'pdf', 0, ?2)",
            params![src.to_string_lossy(), (real_ref ^ 1) as i64],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));

        let apply = RestructureApply::new(db, root.clone(), false);
        let dest = root.join("Sorted").join("doc.pdf").to_string_lossy().into_owned();
        let res = apply.apply(&[move_fixture(1, &src.to_string_lossy(), &dest)]).unwrap();

        assert_eq!(res.applied, 0, "a swapped file must not be moved");
        assert_eq!(res.failed, 1);
        assert!(src.exists(), "the swapped-in file must be left untouched");
        assert!(!root.join("Sorted").join("doc.pdf").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Portable (Linux/macOS) coverage for the std::fs move path: a real move
    /// relocates the file, creates a missing destination parent on demand, and
    /// an occupied destination is refused rather than clobbered — parity with
    /// the Windows MoveFileExW-without-REPLACE_EXISTING contract.
    #[test]
    #[cfg(not(windows))]
    fn move_file_relocates_creates_parent_and_refuses_clobber() {
        let root = std::env::temp_dir().join(format!("fileid-movefile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src = root.join("src.bin");
        std::fs::write(&src, b"PAYLOAD").unwrap();

        // Parent ("nested") does not exist yet — move_file must create it.
        let dst = root.join("nested").join("out.bin");
        let identity = crate::platform::file_identity(&src).unwrap();
        move_file(&src.to_string_lossy(), &dst, identity, &root).expect("move succeeds");
        assert!(!src.exists(), "source removed after a successful move");
        assert_eq!(std::fs::read(&dst).unwrap(), b"PAYLOAD");

        // No clobber: a second move onto the now-occupied destination must fail
        // and leave both the existing file and the new source untouched.
        let src2 = root.join("src2.bin");
        std::fs::write(&src2, b"OTHER").unwrap();
        let identity2 = crate::platform::file_identity(&src2).unwrap();
        assert!(
            move_file(&src2.to_string_lossy(), &dst, identity2, &root).is_err(),
            "an occupied destination must not be clobbered"
        );
        assert_eq!(std::fs::read(&dst).unwrap(), b"PAYLOAD", "existing file preserved");
        assert!(src2.exists(), "source preserved when the move is refused");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn cross_filesystem_move_fails_without_touching_source() {
        use std::os::unix::fs::MetadataExt;

        let source_root = std::env::temp_dir().join(format!(
            "fileid-exdev-source-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        let destination_root = PathBuf::from("/dev/shm").join(format!(
            "fileid-exdev-destination-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        ));
        if std::fs::create_dir_all(&source_root).is_err()
            || std::fs::create_dir_all(&destination_root).is_err()
            || std::fs::metadata(&source_root).unwrap().dev()
                == std::fs::metadata(&destination_root).unwrap().dev()
        {
            let _ = std::fs::remove_dir_all(&source_root);
            let _ = std::fs::remove_dir_all(&destination_root);
            return;
        }

        let source = source_root.join("source.bin");
        let destination = destination_root.join("nested/destination.bin");
        std::fs::write(&source, b"source-payload").unwrap();
        let expected = crate::platform::file_identity(&source).unwrap();
        let tags = vec!["important".to_string()];
        let tags_written = crate::shell::tags::write_tags(&source, &tags).is_ok();

        assert!(move_file(&source.to_string_lossy(), &destination, expected, &source_root).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), b"source-payload");
        assert_eq!(crate::platform::file_identity(&source), Some(expected));
        assert!(!destination.exists());
        if tags_written {
            assert_eq!(crate::shell::tags::read_tags(&source).unwrap(), tags);
        }

        let _ = std::fs::remove_dir_all(source_root);
        let _ = std::fs::remove_dir_all(destination_root);
    }

    /// Portable coverage for the symlink ("use shortcuts instead of moving")
    /// option: the link is created pointing at the original, the parent is made
    /// on demand, and the original is left in place (symlink mode never moves).
    #[test]
    #[cfg(not(windows))]
    fn make_symlink_creates_link_to_original() {
        let root = std::env::temp_dir().join(format!("fileid-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let target = root.join("real.bin");
        std::fs::write(&target, b"REAL").unwrap();
        let link = root.join("links").join("alias.bin");

        let canonical_root = canonicalize_safely(&root).unwrap();
        let identity = crate::platform::file_identity(&target).unwrap();
        assert_eq!(
            make_symlink(
                &target.to_string_lossy(),
                &link,
                identity,
                &canonical_root,
                false,
            )
            .expect("symlink created"),
            SymlinkOutcome::Created
        );
        assert!(target.exists(), "original left in place (symlink mode does not move)");
        let meta = std::fs::symlink_metadata(&link).unwrap();
        assert!(meta.file_type().is_symlink(), "a real symlink was created");
        assert_eq!(std::fs::read(&link).unwrap(), b"REAL", "link resolves to the original payload");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn forward_apply_rejects_sources_outside_selected_root_in_both_modes() {
        let base = undo_fixture_root("outside-source");
        let root = base.join("selected");
        let outside = base.join("outside.jpg");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(&outside, b"outside").unwrap();

        for (index, use_symlinks) in [false, true].into_iter().enumerate() {
            let conn = Connection::open_in_memory().unwrap();
            crate::db::migrations::apply(&conn).unwrap();
            insert_file_row(&conn, 1, &outside.to_string_lossy());
            let db = Arc::new(Mutex::new(conn));
            let destination = root.join(format!("mode-{index}.jpg"));
            let apply = RestructureApply::new(db, root.clone(), use_symlinks)
                .with_undo_journal_path(base.join(format!("undo-{index}.ndjson")));
            let result = apply
                .apply(&[move_fixture(
                    1,
                    &outside.to_string_lossy(),
                    &destination.to_string_lossy(),
                )])
                .unwrap();
            assert_eq!((result.applied, result.failed), (0, 1));
            assert!(outside.exists());
            assert!(!destination.exists());
        }

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn symlink_preview_preserves_prior_real_move_journal() {
        let root = undo_fixture_root("symlink-journal");
        std::fs::create_dir_all(&root).unwrap();
        let journal = root.join("undo.ndjson");
        std::fs::write(&journal, b"prior-real-move-journal\n").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let apply = RestructureApply::new(
            Arc::new(Mutex::new(conn)),
            root.clone(),
            true,
        )
        .with_undo_journal_path(journal.clone());

        let result = apply.apply(&[]).unwrap();

        assert_eq!((result.applied, result.failed), (0, 0));
        assert_eq!(
            std::fs::read(&journal).unwrap(),
            b"prior-real-move-journal\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    fn shortcut_undo_fixture(
        tag: &str,
    ) -> Option<(PathBuf, PathBuf, PathBuf, PathBuf, String)> {
        let root = undo_fixture_root(tag);
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("original.bin");
        let link = root.join("Preview").join("original.bin");
        std::fs::write(&source, b"ORIGINAL").unwrap();
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        let canonical_root = canonicalize_safely(&root).unwrap();
        let source_identity = crate::platform::file_identity(&source).unwrap();
        let manifest_dir = root.join("shortcut-undo");
        let mut manifest =
            ShortcutUndoManifest::create(&manifest_dir, &canonical_root).unwrap();
        match create_recorded_shortcut(
            &mut manifest,
            1,
            &source.to_string_lossy(),
            &link,
            source_identity,
            &canonical_root,
            false,
            false,
        ) {
            Ok(SymlinkOutcome::Created) => {}
            Ok(SymlinkOutcome::AlreadyPresent) => panic!("fixture shortcut unexpectedly existed"),
            Err(ApplyError::Privilege(_)) => {
                let _ = std::fs::remove_dir_all(root);
                return None;
            }
            Err(ApplyError::Other(error)) => panic!("creating fixture shortcut: {error:#}"),
        }
        let token = manifest.finish().unwrap();
        Some((root, manifest_dir, source, link, token))
    }

    fn synthetic_shortcut_manifest(
        root: &Path,
        source: &Path,
        link: &Path,
        link_identity: crate::platform::FileIdentity,
    ) -> (PathBuf, String) {
        let canonical_root = canonicalize_safely(root).unwrap();
        let source_identity = crate::platform::file_identity(source).unwrap();
        let manifest_dir = root.join("shortcut-undo");
        let mut manifest =
            ShortcutUndoManifest::create(&manifest_dir, &canonical_root).unwrap();
        manifest
            .append_committed(
                1,
                &source.to_string_lossy(),
                link,
                None,
                source_identity,
                link_identity,
            )
            .unwrap();
        let token = manifest.finish().unwrap();
        (manifest_dir, token)
    }

    #[test]
    fn shortcut_undo_retry_returns_the_durable_completion_receipt() {
        let root = undo_fixture_root("shortcut-receipt");
        let source = root.join("original.bin");
        let missing_link = root.join("Preview").join("original.bin");
        std::fs::create_dir_all(missing_link.parent().unwrap()).unwrap();
        std::fs::write(&source, b"ORIGINAL").unwrap();
        let source_identity = crate::platform::file_identity(&source).unwrap();
        let (manifest_dir, token) =
            synthetic_shortcut_manifest(&root, &source, &missing_link, source_identity);
        let real_move_journal = root.join("restructure_undo.ndjson");
        std::fs::write(&real_move_journal, b"real-move-journal").unwrap();
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(real_move_journal.clone())
            .with_shortcut_undo_dir(manifest_dir.clone());

        let first = apply.undo_shortcuts(&token).unwrap();
        let retry = apply.undo_shortcuts(&token).unwrap();

        assert_eq!((first.applied, first.failed, first.planned), (1, 0, Some(1)));
        assert_eq!(
            (retry.applied, retry.failed, retry.planned),
            (1, 0, Some(1))
        );
        assert!(!retry.cancelled);
        assert!(shortcut_receipt_path(&manifest_dir, &token)
            .unwrap()
            .exists());
        assert!(!shortcut_manifest_path(&manifest_dir, &token)
            .unwrap()
            .exists());
        assert_eq!(
            std::fs::read(&real_move_journal).unwrap(),
            b"real-move-journal"
        );
        let other_root = undo_fixture_root("shortcut-receipt-other-root");
        std::fs::create_dir_all(&other_root).unwrap();
        let other = RestructureApply::new(
            Arc::new(Mutex::new(Connection::open_in_memory().unwrap())),
            other_root.clone(),
            false,
        )
        .with_shortcut_undo_dir(manifest_dir.clone());
        assert!(other.undo_shortcuts(&token).is_err());
        let _ = std::fs::remove_dir_all(other_root);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_undo_unknown_token_is_not_ambiguous_success() {
        let root = undo_fixture_root("shortcut-unknown-token");
        std::fs::create_dir_all(&root).unwrap();
        let manifest_dir = root.join("shortcut-undo");
        let token = uuid::Uuid::new_v4().to_string();
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir);

        let error = apply.undo_shortcuts(&token).unwrap_err();

        assert!(error.to_string().contains("was not found"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn valid_unterminated_shortcut_manifest_tail_is_not_replayed() {
        let root = undo_fixture_root("shortcut-torn-tail");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("original.bin");
        let link = root.join("Preview").join("original.bin");
        std::fs::write(&source, b"ORIGINAL").unwrap();
        let identity = crate::platform::file_identity(&source).unwrap();
        let canonical_root = canonicalize_safely(&root).unwrap();
        let manifest = ShortcutUndoManifest::create(&root.join("shortcut-undo"), &canonical_root)
            .unwrap();
        let ShortcutUndoManifest {
            mut file,
            path,
            token: _,
            ..
        } = manifest;
        let entry = serde_json::to_vec(&ShortcutUndoEntry {
            file_id: 1,
            source: source.to_string_lossy().into_owned(),
            link: link.to_string_lossy().into_owned(),
            staging_link: None,
            source_identity: identity,
            link_identity: identity,
        })
        .unwrap();
        file.write_all(&entry).unwrap();
        file.sync_all().unwrap();
        drop(file);

        let scan = scan_shortcut_undo_manifest(&path).unwrap().unwrap();

        assert!(scan.spans.is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_undo_rejects_a_regular_file_even_with_its_recorded_identity() {
        let root = undo_fixture_root("shortcut-regular-replacement");
        let source = root.join("original.bin");
        let replacement = root.join("Preview").join("original.bin");
        std::fs::create_dir_all(replacement.parent().unwrap()).unwrap();
        std::fs::write(&source, b"ORIGINAL").unwrap();
        std::fs::write(&replacement, b"USER REPLACEMENT").unwrap();
        let replacement_identity = crate::platform::file_identity(&replacement).unwrap();
        let (manifest_dir, token) =
            synthetic_shortcut_manifest(&root, &source, &replacement, replacement_identity);
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir.clone());

        let result = apply.undo_shortcuts(&token).unwrap();

        assert_eq!((result.applied, result.failed), (0, 1));
        assert_eq!(std::fs::read(&replacement).unwrap(), b"USER REPLACEMENT");
        assert!(shortcut_manifest_path(&manifest_dir, &token)
            .unwrap()
            .exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_manifest_commit_failure_removes_the_just_created_link() {
        let root = undo_fixture_root("shortcut-commit-failure");
        let source = root.join("Incoming").join("original.bin");
        let link = root.join("Preview").join("original.bin");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::fs::write(&source, b"ORIGINAL").unwrap();
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        insert_file_row(&conn, 1, &source.to_string_lossy());
        let manifest_dir = root.join("shortcut-undo");
        let apply = RestructureApply::new(Arc::new(Mutex::new(conn)), root.clone(), true)
            .with_shortcut_undo_dir(manifest_dir.clone())
            .with_fail_next_shortcut_manifest_commit();

        let result = apply
            .apply(&[move_fixture(
                1,
                &source.to_string_lossy(),
                &link.to_string_lossy(),
            )])
            .unwrap();

        if result.privilege_error.is_none() {
            assert_eq!((result.applied, result.failed), (0, 1));
            assert!(result.shortcut_undo_token.is_none());
            assert!(std::fs::symlink_metadata(&link).is_err());
            let manifests = std::fs::read_dir(&manifest_dir)
                .map(|entries| {
                    entries
                        .filter_map(std::result::Result::ok)
                        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "ndjson"))
                        .count()
                })
                .unwrap_or(0);
            assert_eq!(manifests, 0);
        }
        assert!(source.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    fn shortcut_staging_crash_fixture(
        tag: &str,
        record_identity: bool,
        commit: bool,
        publish: bool,
    ) -> Option<(PathBuf, PathBuf, PathBuf, PathBuf, PathBuf, PathBuf, String)> {
        let root = undo_fixture_root(tag);
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("original.bin");
        let final_link = root.join("Preview").join("original.bin");
        std::fs::create_dir_all(final_link.parent().unwrap()).unwrap();
        std::fs::write(&source, b"ORIGINAL").unwrap();
        let canonical_root = canonicalize_safely(&root).unwrap();
        let source_identity = crate::platform::file_identity(&source).unwrap();
        let manifest_dir = root.join("shortcut-undo");
        let mut manifest =
            ShortcutUndoManifest::create(&manifest_dir, &canonical_root).unwrap();
        let mut prepared = manifest
            .prepare_intent(
                1,
                &source.to_string_lossy(),
                &final_link,
                source_identity,
            )
            .unwrap();
        let staged_link = PathBuf::from(&prepared.intent.staging_link);
        match make_symlink(
            &source.to_string_lossy(),
            &staged_link,
            source_identity,
            &canonical_root,
            false,
        ) {
            Ok(SymlinkOutcome::Created) => {}
            Ok(SymlinkOutcome::AlreadyPresent) => panic!("staging fixture unexpectedly existed"),
            Err(ApplyError::Privilege(_)) => {
                let _ = std::fs::remove_dir_all(root);
                return None;
            }
            Err(ApplyError::Other(error)) => panic!("creating staged shortcut: {error:#}"),
        }
        let link_identity = shortcut_link_identity(&staged_link).unwrap();
        if record_identity {
            manifest
                .record_staged_identity(&mut prepared, link_identity)
                .unwrap();
        }
        if commit {
            manifest
                .append_committed(
                    1,
                    &source.to_string_lossy(),
                    &final_link,
                    Some(&staged_link),
                    source_identity,
                    link_identity,
                )
                .unwrap();
        }
        if publish {
            rename_staged_shortcut(
                &staged_link,
                &final_link,
                &source,
                source_identity,
                link_identity,
                &canonical_root,
            )
            .unwrap();
        }
        let intent_path = prepared.path;
        let token = manifest.finish().unwrap();
        Some((
            root,
            manifest_dir,
            source,
            final_link,
            staged_link,
            intent_path,
            token,
        ))
    }

    #[test]
    fn shortcut_recovery_removes_an_intent_created_before_any_link() {
        let root = undo_fixture_root("shortcut-intent-only");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("original.bin");
        let final_link = root.join("Preview").join("original.bin");
        std::fs::create_dir_all(final_link.parent().unwrap()).unwrap();
        std::fs::write(&source, b"ORIGINAL").unwrap();
        let canonical_root = canonicalize_safely(&root).unwrap();
        let source_identity = crate::platform::file_identity(&source).unwrap();
        let manifest_dir = root.join("shortcut-undo");
        let manifest = ShortcutUndoManifest::create(&manifest_dir, &canonical_root).unwrap();
        let prepared = manifest
            .prepare_intent(
                1,
                &source.to_string_lossy(),
                &final_link,
                source_identity,
            )
            .unwrap();
        let token = manifest.finish().unwrap();
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir.clone());

        let first = apply.undo_shortcuts(&token).unwrap();
        let retry = apply.undo_shortcuts(&token).unwrap();

        assert_eq!((first.applied, first.failed, first.planned), (1, 0, Some(1)));
        assert_eq!(
            (retry.applied, retry.failed, retry.planned),
            (first.applied, first.failed, first.planned)
        );
        assert!(!prepared.path.exists());
        assert!(std::fs::symlink_metadata(&final_link).is_err());
        assert!(shortcut_receipt_path(&manifest_dir, &token)
            .unwrap()
            .exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_recovery_removes_identity_bound_staged_link_before_manifest_commit() {
        let Some((root, manifest_dir, _source, final_link, staged_link, intent_path, token)) =
            shortcut_staging_crash_fixture("shortcut-staged-before-commit", true, false, false)
        else {
            return;
        };
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir);

        let result = apply.undo_shortcuts(&token).unwrap();

        assert_eq!((result.applied, result.failed, result.planned), (1, 0, Some(1)));
        assert!(std::fs::symlink_metadata(&staged_link).is_err());
        assert!(!intent_path.exists());
        assert!(std::fs::symlink_metadata(&final_link).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_recovery_replays_committed_staged_link_before_final_rename() {
        let Some((root, manifest_dir, _source, final_link, staged_link, intent_path, token)) =
            shortcut_staging_crash_fixture("shortcut-committed-before-rename", true, true, false)
        else {
            return;
        };
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir);

        let result = apply.undo_shortcuts(&token).unwrap();

        assert_eq!((result.applied, result.failed, result.planned), (2, 0, Some(2)));
        assert!(std::fs::symlink_metadata(&staged_link).is_err());
        assert!(!intent_path.exists());
        assert!(std::fs::symlink_metadata(&final_link).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_recovery_replays_final_link_after_rename_before_intent_removal() {
        let Some((root, manifest_dir, _source, final_link, staged_link, intent_path, token)) =
            shortcut_staging_crash_fixture("shortcut-renamed-before-intent-delete", true, true, true)
        else {
            return;
        };
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir);

        let result = apply.undo_shortcuts(&token).unwrap();

        assert_eq!((result.applied, result.failed, result.planned), (2, 0, Some(2)));
        assert!(std::fs::symlink_metadata(&staged_link).is_err());
        assert!(!intent_path.exists());
        assert!(std::fs::symlink_metadata(&final_link).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_recovery_preserves_staged_link_without_a_durable_identity() {
        let Some((root, manifest_dir, _source, final_link, staged_link, intent_path, token)) =
            shortcut_staging_crash_fixture("shortcut-staged-before-identity", false, false, false)
        else {
            return;
        };
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir);

        let error = apply.undo_shortcuts(&token).unwrap_err();

        assert!(error.to_string().contains("no durably recorded link identity"));
        assert!(shortcut_link_identity(&staged_link).is_ok());
        assert!(intent_path.exists());
        assert!(std::fs::symlink_metadata(&final_link).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_recovery_rejects_a_replaced_staging_directory() {
        let root = undo_fixture_root("shortcut-replaced-staging-dir");
        std::fs::create_dir_all(&root).unwrap();
        let canonical_root = canonicalize_safely(&root).unwrap();
        let manifest_dir = root.join("shortcut-undo");
        let manifest = ShortcutUndoManifest::create(&manifest_dir, &canonical_root).unwrap();
        let staging_dir = manifest.staging_dir.clone();
        let token = manifest.token.clone();
        drop(manifest);
        let preserved = staging_dir.with_extension("preserved");
        std::fs::rename(&staging_dir, &preserved).unwrap();
        std::fs::create_dir(&staging_dir).unwrap();
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir);

        let error = apply.undo_shortcuts(&token).unwrap_err();

        assert!(error.to_string().contains("replaced or is unsafe"));
        assert!(preserved.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_recovery_preserves_an_unexpected_staged_regular_file() {
        let root = undo_fixture_root("shortcut-regular-staged-object");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("original.bin");
        let final_link = root.join("Preview").join("original.bin");
        std::fs::create_dir_all(final_link.parent().unwrap()).unwrap();
        std::fs::write(&source, b"ORIGINAL").unwrap();
        let canonical_root = canonicalize_safely(&root).unwrap();
        let source_identity = crate::platform::file_identity(&source).unwrap();
        let manifest_dir = root.join("shortcut-undo");
        let manifest = ShortcutUndoManifest::create(&manifest_dir, &canonical_root).unwrap();
        let prepared = manifest
            .prepare_intent(
                1,
                &source.to_string_lossy(),
                &final_link,
                source_identity,
            )
            .unwrap();
        let staged_link = PathBuf::from(&prepared.intent.staging_link);
        std::fs::write(&staged_link, b"DO NOT DELETE").unwrap();
        let token = manifest.finish().unwrap();
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir);

        let error = apply.undo_shortcuts(&token).unwrap_err();

        assert!(error.to_string().contains("no durably recorded link identity"));
        assert_eq!(std::fs::read(&staged_link).unwrap(), b"DO NOT DELETE");
        assert!(prepared.path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_recovery_rejects_an_oversized_intent() {
        let root = undo_fixture_root("shortcut-oversized-intent");
        std::fs::create_dir_all(&root).unwrap();
        let source = root.join("original.bin");
        let final_link = root.join("Preview").join("original.bin");
        std::fs::create_dir_all(final_link.parent().unwrap()).unwrap();
        std::fs::write(&source, b"ORIGINAL").unwrap();
        let canonical_root = canonicalize_safely(&root).unwrap();
        let source_identity = crate::platform::file_identity(&source).unwrap();
        let manifest_dir = root.join("shortcut-undo");
        let manifest = ShortcutUndoManifest::create(&manifest_dir, &canonical_root).unwrap();
        let prepared = manifest
            .prepare_intent(
                1,
                &source.to_string_lossy(),
                &final_link,
                source_identity,
            )
            .unwrap();
        let mut file = OpenOptions::new()
            .write(true)
            .open(&prepared.path)
            .unwrap();
        file.set_len(0).unwrap();
        file.write_all(&vec![b'x'; MAX_SHORTCUT_RECORD_BYTES + 1])
            .unwrap();
        file.sync_all().unwrap();
        let token = manifest.finish().unwrap();
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir);

        let error = apply.undo_shortcuts(&token).unwrap_err();

        assert!(error.to_string().contains("bounded regular file"));
        assert!(prepared.path.exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_manifest_scanner_rejects_an_oversized_line() {
        let root = undo_fixture_root("shortcut-oversized-manifest-line");
        std::fs::create_dir_all(&root).unwrap();
        let canonical_root = canonicalize_safely(&root).unwrap();
        let manifest = ShortcutUndoManifest::create(&root.join("shortcut-undo"), &canonical_root)
            .unwrap();
        let ShortcutUndoManifest {
            mut file,
            path,
            staging_dir,
            ..
        } = manifest;
        file.write_all(&vec![b'x'; MAX_SHORTCUT_RECORD_BYTES + 1])
            .unwrap();
        file.write_all(b"\n").unwrap();
        file.sync_all().unwrap();
        drop(file);

        let error = match scan_shortcut_undo_manifest(&path) {
            Ok(_) => panic!("oversized shortcut manifest line was accepted"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("exceeds"));
        assert!(path.exists());
        let _ = std::fs::remove_dir_all(staging_dir.parent().unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancelled_shortcut_undo_can_retry_to_a_durable_success() {
        let root = undo_fixture_root("shortcut-cancel-retry");
        let source = root.join("original.bin");
        let missing_link = root.join("Preview").join("original.bin");
        std::fs::create_dir_all(missing_link.parent().unwrap()).unwrap();
        std::fs::write(&source, b"ORIGINAL").unwrap();
        let identity = crate::platform::file_identity(&source).unwrap();
        let (manifest_dir, token) =
            synthetic_shortcut_manifest(&root, &source, &missing_link, identity);
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let cancelled = RestructureApply::new(db.clone(), root.clone(), false)
            .with_cancel(Arc::new(AtomicBool::new(true)))
            .with_shortcut_undo_dir(manifest_dir.clone())
            .undo_shortcuts(&token)
            .unwrap();
        let retry = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir.clone())
            .undo_shortcuts(&token)
            .unwrap();

        assert!(cancelled.cancelled);
        assert_eq!(cancelled.remaining, Some(1));
        assert_eq!((retry.applied, retry.failed, retry.planned), (1, 0, Some(1)));
        assert!(shortcut_receipt_path(&manifest_dir, &token)
            .unwrap()
            .exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_undo_removes_only_its_links_and_preserves_real_move_journal() {
        let Some((root, manifest_dir, source, link, token)) =
            shortcut_undo_fixture("shortcut-undo")
        else {
            return;
        };
        let real_move_journal = root.join("restructure_undo.ndjson");
        std::fs::write(&real_move_journal, b"real-move-journal").unwrap();
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_undo_journal_path(real_move_journal.clone())
            .with_shortcut_undo_dir(manifest_dir.clone());

        let result = apply.undo_shortcuts(&token).unwrap();

        assert_eq!((result.applied, result.failed), (1, 0));
        assert_eq!(result.planned, Some(1));
        assert!(!result.cancelled);
        assert!(source.exists());
        assert!(std::fs::symlink_metadata(&link).is_err());
        assert_eq!(
            std::fs::read(&real_move_journal).unwrap(),
            b"real-move-journal"
        );
        assert!(!shortcut_manifest_path(&manifest_dir, &token)
            .unwrap()
            .exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_undo_never_deletes_a_replacement_regular_file() {
        let Some((root, manifest_dir, _source, link, token)) =
            shortcut_undo_fixture("shortcut-replaced")
        else {
            return;
        };
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, b"USER REPLACEMENT").unwrap();
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir.clone());

        let result = apply.undo_shortcuts(&token).unwrap();

        assert_eq!((result.applied, result.failed), (0, 1));
        assert_eq!(std::fs::read(&link).unwrap(), b"USER REPLACEMENT");
        assert!(shortcut_manifest_path(&manifest_dir, &token)
            .unwrap()
            .exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_undo_never_deletes_a_recreated_matching_symlink() {
        let Some((root, manifest_dir, source, link, token)) =
            shortcut_undo_fixture("shortcut-recreated")
        else {
            return;
        };
        let original_identity = shortcut_link_identity(&link).unwrap();
        std::fs::remove_file(&link).unwrap();
        let canonical_root = canonicalize_safely(&root).unwrap();
        let source_identity = crate::platform::file_identity(&source).unwrap();
        let recreated_identity = (0..32)
            .find_map(|attempt| {
                let filler = link
                    .parent()
                    .unwrap()
                    .join(format!("identity-filler-{attempt}.bin"));
                std::fs::write(&filler, b"filler").unwrap();
                assert_eq!(
                    make_symlink(
                        &source.to_string_lossy(),
                        &link,
                        source_identity,
                        &canonical_root,
                        false,
                    )
                    .unwrap(),
                    SymlinkOutcome::Created
                );
                let identity = shortcut_link_identity(&link).unwrap();
                if identity == original_identity {
                    std::fs::remove_file(&link).unwrap();
                    None
                } else {
                    Some(identity)
                }
            })
            .expect("filesystem repeatedly reused the deleted shortcut identity");
        assert_ne!(recreated_identity, original_identity);
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir.clone());

        let result = apply.undo_shortcuts(&token).unwrap();

        assert_eq!((result.applied, result.failed), (0, 1));
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(symlink_target_matches(&link, &source).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_undo_never_deletes_a_repointed_symlink() {
        let Some((root, manifest_dir, _source, link, token)) =
            shortcut_undo_fixture("shortcut-repointed")
        else {
            return;
        };
        let other_source = root.join("other.bin");
        std::fs::write(&other_source, b"OTHER").unwrap();
        std::fs::remove_file(&link).unwrap();
        let canonical_root = canonicalize_safely(&root).unwrap();
        let other_identity = crate::platform::file_identity(&other_source).unwrap();
        assert_eq!(
            make_symlink(
                &other_source.to_string_lossy(),
                &link,
                other_identity,
                &canonical_root,
                false,
            )
            .unwrap(),
            SymlinkOutcome::Created
        );
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir);

        let result = apply.undo_shortcuts(&token).unwrap();

        assert_eq!((result.applied, result.failed), (0, 1));
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert!(symlink_target_matches(&link, &other_source).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cancelled_shortcut_undo_keeps_manifest_and_link_for_retry() {
        let Some((root, manifest_dir, _source, link, token)) =
            shortcut_undo_fixture("shortcut-cancel")
        else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(true));
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, root.clone(), false)
            .with_cancel(cancel)
            .with_shortcut_undo_dir(manifest_dir.clone());

        let result = apply.undo_shortcuts(&token).unwrap();

        assert!(result.cancelled);
        assert_eq!((result.applied, result.failed), (0, 0));
        assert_eq!(result.remaining, Some(1));
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert!(shortcut_manifest_path(&manifest_dir, &token)
            .unwrap()
            .exists());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn shortcut_undo_rejects_a_manifest_from_another_library_root() {
        let Some((root, manifest_dir, _source, link, token)) =
            shortcut_undo_fixture("shortcut-root")
        else {
            return;
        };
        let other_root = undo_fixture_root("shortcut-other-root");
        std::fs::create_dir_all(&other_root).unwrap();
        let db = Arc::new(Mutex::new(Connection::open_in_memory().unwrap()));
        let apply = RestructureApply::new(db, other_root.clone(), false)
            .with_shortcut_undo_dir(manifest_dir);

        assert!(apply.undo_shortcuts(&token).is_err());
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(other_root);
    }

    #[test]
    fn apply_requires_an_existing_absolute_library_root() {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::migrations::apply(&conn).unwrap();
        let apply =
            RestructureApply::new(Arc::new(Mutex::new(conn)), PathBuf::from("relative"), false);
        assert!(apply.apply(&[]).is_err());
    }
}
