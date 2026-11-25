use std::path::{Path, PathBuf};

use fileid_engine::ipc::CommandPayload;

fn exclusions_at(path: &Path) -> Result<Vec<PathBuf>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(format!("Couldn't read the folders not to describe: {error}")),
    };
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|_| {
            "Couldn't read the folders not to describe: the settings file is damaged.".to_owned()
        })?;
    let Some(value) = map.get("deepAnalyzeExcludedFolders") else {
        return Ok(vec![]);
    };
    let folders = value
        .as_array()
        .filter(|folders| folders.len() <= 256)
        .ok_or("Couldn't read the folders not to describe: expected up to 256 full folder paths.")?;
    folders
        .iter()
        .map(|value| {
            let raw = value.as_str().ok_or("Invalid excluded folder.")?;
            let folder = Path::new(raw);
            if !folder.is_absolute() {
                return Err("Folders not to describe must be full paths. Fix them in Settings, then try again.".into());
            }
            folder.canonicalize().map_err(|_| {
                "A folder not to describe can't be found. Reconnect it or remove it in Settings, then try again.".into()
            })
        })
        .collect()
}

fn check_explicit_target(
    payload: &CommandPayload,
    exclusions: &[PathBuf],
    db: &Path,
) -> Result<(), String> {
    if exclusions.is_empty() {
        return Ok(());
    }
    let conn = fileid_engine::db::open_read(db).map_err(|error| error.to_string())?;
    let paths: Vec<String> = match payload {
        CommandPayload::DeepAnalyzeFile(request) => vec![conn
            .query_row(
                "SELECT path_text FROM files WHERE id=?1",
                [request.file_id],
                |row| row.get(0),
            )
            .map_err(|_| "Choose an existing indexed file ID.".to_owned())?],
        CommandPayload::DeepAnalyzeFolder(request) => {
            let folder = Path::new(&request.path_prefix)
                .canonicalize()
                .map_err(|_| "Choose an existing absolute folder.".to_owned())?;
            if exclusions
                .iter()
                .any(|excluded| folder.starts_with(excluded) || excluded.starts_with(&folder))
            {
                return Err("This folder overlaps one of the folders not to describe.".into());
            }
            let mut stmt = conn
                .prepare("SELECT path_text FROM files WHERE substr(path_text,1,length(?1))=?1")
                .map_err(|error| error.to_string())?;
            let rows = stmt
                .query_map([&request.path_prefix], |row| row.get(0))
                .map_err(|error| error.to_string())?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?
        }
        _ => return Ok(()),
    };
    for raw in paths {
        if inside_exclusion(&raw, exclusions)? {
            return Err("This is inside one of the folders not to describe.".into());
        }
    }
    Ok(())
}

/// A file that is gone cannot be inside a folder; only a real I/O failure means the drive is away.
fn inside_exclusion(raw: &str, exclusions: &[PathBuf]) -> Result<bool, String> {
    match Path::new(raw).canonicalize() {
        Ok(path) => Ok(exclusions.iter().any(|excluded| path.starts_with(excluded))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err("A scanned folder is offline. Reconnect it, then try again.".to_owned()),
    }
}

pub(super) fn prepare(mut payload: CommandPayload) -> Result<CommandPayload, String> {
    if !matches!(
        payload,
        CommandPayload::DeepAnalyzeAll(_)
            | CommandPayload::DeepAnalyzeFolder(_)
            | CommandPayload::DeepAnalyzeFile(_)
    ) {
        return Ok(payload);
    }
    let settings = fileid_engine::paths::app_settings_path().map_err(|error| error.to_string())?;
    let exclusions = exclusions_at(&settings)?;
    match &mut payload {
        CommandPayload::DeepAnalyzeAll(request) => {
            request.excluded_folders = (!exclusions.is_empty()).then(|| {
                exclusions
                    .iter()
                    .map(|path| path.to_string_lossy().into_owned())
                    .collect()
            });
        }
        _ => {
            let db = fileid_engine::paths::db_path().map_err(|error| error.to_string())?;
            check_explicit_target(&payload, &exclusions, &db)?;
        }
    }
    Ok(payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_file_does_not_block_a_run_but_an_excluded_one_does() {
        let dir = std::env::temp_dir().join(format!("fileid-deep-scope-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let excluded = dir.canonicalize().unwrap();
        let present = dir.join("a.jpg");
        std::fs::write(&present, b"x").unwrap();
        let exclusions = vec![excluded];
        assert_eq!(
            inside_exclusion(dir.join("gone.jpg").to_str().unwrap(), &exclusions),
            Ok(false)
        );
        assert_eq!(
            inside_exclusion(present.to_str().unwrap(), &exclusions),
            Ok(true)
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn invalid_exclusion_settings_fail_closed() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fileid-deep-exclusions-{}-{stamp}",
            std::process::id()
        ));
        std::fs::write(&path, r#"{"deepAnalyzeExcludedFolders":["relative"]}"#).unwrap();
        assert!(exclusions_at(&path).is_err());
        std::fs::write(&path, b"not json").unwrap();
        assert!(exclusions_at(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
