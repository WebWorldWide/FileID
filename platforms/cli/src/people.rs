//! People come from the engine's face clustering, so this is empty until a full scan with face models has run.

use anyhow::Result;
use fileid_engine::read::people::active_people;

use crate::context::{Ctx, print_json, terminal_text, truncate};

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
    println!("{}", header(ctx));
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

fn header(ctx: &Ctx) -> String {
    format!(
        "  {} {} {} {}",
        ctx.bold(&format!("{:<6}", "id")),
        ctx.bold(&format!("{:<28}", "name")),
        ctx.bold(&format!("{:>6}", "faces")),
        ctx.bold(&format!("{:>6}", "files"))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colored_header_columns_line_up_with_plain_rows() {
        let strip = |s: &str| s.replace("\x1b[1m", "").replace("\x1b[0m", "");
        let mut ctx = Ctx {
            json: false,
            quiet: false,
            color: false,
            color_allowed: false,
            db: std::path::PathBuf::new(),
            db_explicit: false,
        };
        let plain = header(&ctx);
        ctx.color = true;
        let colored = header(&ctx);
        assert_ne!(plain, colored);
        assert_eq!(strip(&colored), plain);
        assert_eq!(
            plain,
            format!("  {:<6} {:<28} {:>6} {:>6}", "id", "name", "faces", "files")
        );
    }
}
