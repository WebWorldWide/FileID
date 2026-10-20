//! `fileid people` — list person clusters (id, display name, face count).
//! Read-only. People are produced by the engine's face detection + clustering,
//! so this is empty until a full engine scan (with face models) has run.

use anyhow::Result;
use fileid_engine::read::people::active_people;

use crate::context::{print_json, terminal_text, truncate, Ctx};

pub fn run(ctx: &Ctx) -> Result<()> {
    ctx.require_db_exists()?;
    let conn = fileid_engine::db::open_read(&ctx.db)?;
    let people = active_people(&conn, None)?;

    if ctx.json {
        let arr: Vec<serde_json::Value> = people
            .iter()
            .map(|p| {
                serde_json::json!({
                    "id": p.id,
                    "name": p.name,
                    "isUnknown": p.is_unknown,
                    "faceCount": p.faces,
                    "fileCount": p.files,
                })
            })
            .collect();
        print_json(&serde_json::json!({
            "command": "people",
            "count": arr.len(),
            "people": arr,
        }));
        return Ok(());
    }

    if people.is_empty() {
        println!("No person clusters.");
        ctx.progress(&format!(
            "  {}",
            ctx.dim("people come from face clustering — run a full engine scan with face models")
        ));
        return Ok(());
    }
    println!("{} person cluster(s):", people.len());
    println!(
        "  {:<6} {:<28} {:>6} {:>6}",
        ctx.bold("id"),
        ctx.bold("name"),
        ctx.bold("faces"),
        ctx.bold("files")
    );
    for p in &people {
        println!(
            "  {:<6} {:<28} {:>6} {:>6}",
            p.id,
            truncate(&terminal_text(&p.name), 28),
            p.faces,
            p.files
        );
    }
    Ok(())
}
