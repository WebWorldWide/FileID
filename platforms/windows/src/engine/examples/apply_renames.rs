// Standalone Cargo example to apply 94,980 proposed renames safely on the user's hard drive.
// Compile and run with: cargo run --example apply_renames --release

use std::path::{Path, PathBuf};
use rusqlite::params;

struct ProposedRename {
    id: i64,
    path_text: String,
    proposed_name: String,
}

fn is_case_only_rename(src: &Path, dst: &Path) -> bool {
    let src_lower = src.to_string_lossy().to_lowercase();
    let dst_lower = dst.to_string_lossy().to_lowercase();
    src_lower == dst_lower
}

fn rename_file_safe(src: &Path, dst: &Path) -> std::io::Result<()> {
    if let Err(e) = std::fs::rename(src, dst) {
        if is_case_only_rename(src, dst) {
            // Case-only rename on Windows NTFS can fail directly if it doesn't recognize
            // the casing change. Try via a temporary path.
            let tmp_name = format!("rename_{}.tmp", uuid::Uuid::new_v4());
            let tmp_path = src.with_file_name(tmp_name);
            
            std::fs::rename(src, &tmp_path)?;
            if let Err(err) = std::fs::rename(&tmp_path, dst) {
                let _ = std::fs::rename(&tmp_path, src);
                return Err(err);
            }
            Ok(())
        } else {
            Err(e)
        }
    } else {
        Ok(())
    }
}

fn main() -> anyhow::Result<()> {
    let local_app_data = std::env::var("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let user_profile = std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\Users\\adamm".to_string());
            PathBuf::from(user_profile).join("AppData").join("Local")
        });
    let db_path = local_app_data.join("FileID").join("fileid.sqlite");
    
    println!("Connecting to database at: {}", db_path.display());
    if !db_path.exists() {
        anyhow::bail!("Database file does not exist!");
    }
    
    let mut conn = rusqlite::Connection::open(&db_path)?;
    
    let items: Vec<ProposedRename> = {
        let mut stmt = conn.prepare(
            "SELECT id, path_text, vlm_proposed_name FROM files WHERE vlm_proposed_name IS NOT NULL AND vlm_proposed_name != ''"
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(ProposedRename {
                id: row.get(0)?,
                path_text: row.get(1)?,
                proposed_name: row.get(2)?,
            })
        })?.filter_map(Result::ok).collect();
        rows
    };
    
    let total = items.len();
    println!("Found {} pending proposed renames to apply.", total);
    if total == 0 {
        println!("Nothing to rename. Exiting.");
        return Ok(());
    }
    
    let mut succeeded = 0;
    let mut failed = 0;
    let mut skipped = 0;
    let mut collisions = 0;
    
    let batch_size = 1000;
    let chunks = items.chunks(batch_size);
    
    println!("Starting rename operations...");
    
    for (chunk_idx, chunk) in chunks.enumerate() {
        let tx = conn.transaction()?;
        {
            let mut update_stmt = tx.prepare(
                "UPDATE files SET path_text = ?1, path_hash = ?2, path_search = ?3, vlm_proposed_name = NULL WHERE id = ?4"
            )?;
            
            for item in chunk {
                let src_path = Path::new(&item.path_text);
                if !src_path.exists() {
                    skipped += 1;
                    continue;
                }
                
                let dir = match src_path.parent() {
                    Some(d) => d,
                    None => {
                        failed += 1;
                        eprintln!("File ID {} has no parent directory: {}", item.id, item.path_text);
                        continue;
                    }
                };
                
                let ext = src_path.extension()
                    .map(|e| format!(".{}", e.to_string_lossy()))
                    .unwrap_or_default();
                
                let initial_dst_name = format!("{}{}", item.proposed_name, ext);
                let initial_dst = dir.join(&initial_dst_name);
                
                let mut final_dst = initial_dst.clone();
                
                // If destination exists, and it's not a case-only rename of the same file, resolve collision
                if final_dst.exists() && !is_case_only_rename(src_path, &final_dst) {
                    collisions += 1;
                    let mut suffix = 1;
                    while final_dst.exists() {
                        let collision_name = format!("{}_{}{}", item.proposed_name, suffix, ext);
                        final_dst = dir.join(collision_name);
                        suffix += 1;
                    }
                }
                
                // Perform rename on disk
                if let Err(e) = rename_file_safe(src_path, &final_dst) {
                    failed += 1;
                    eprintln!("Failed to rename '{}' -> '{}': {}", src_path.display(), final_dst.display(), e);
                    continue;
                }
                
                // Move tags sidecar if it exists
                fileid_engine::shell::tags::move_sidecar(src_path, &final_dst);
                
                // Calculate db columns
                let dest_text = final_dst.to_string_lossy().to_string();
                let dest_hash = fileid_engine::util::path_safety::stable_path_hash(&dest_text);
                let dest_search = fileid_engine::pipeline::dbwriter::nfc_path_search(&dest_text);
                
                // Update database
                if let Err(e) = update_stmt.execute(params![dest_text, dest_hash, dest_search, item.id]) {
                    failed += 1;
                    eprintln!("Failed to update DB for ID {}: {}", item.id, e);
                    // Attempt to rollback disk rename to keep consistent
                    let _ = rename_file_safe(&final_dst, src_path);
                    continue;
                }
                
                succeeded += 1;
            }
        }
        tx.commit()?;
        
        let processed = (chunk_idx + 1) * batch_size;
        let pct = (processed as f64 / total as f64 * 100.0).min(100.0);
        println!(
            "Progress: {:.1}% | Processed {}/{} | Succeeded: {} | Failed: {} | Skipped: {} | Collisions: {}",
            pct,
            processed.min(total),
            total,
            succeeded,
            failed,
            skipped,
            collisions
        );
    }
    
    println!("\nRename operation completed!");
    println!("-----------------------------");
    println!("Total checked:  {}", total);
    println!("Succeeded:      {}", succeeded);
    println!("Failed:         {}", failed);
    println!("Missing/Skipped:{}", skipped);
    println!("Collisions resolved: {}", collisions);
    
    Ok(())
}
