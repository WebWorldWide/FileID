use std::path::{Path, PathBuf};

use fileid_engine::ipc::CommandPayload;

fn exclusions_at(path: &Path) -> Result<Vec<PathBuf>, String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(format!("Cannot validate Deep Analyze exclusions: {error}")),
    };
    let map: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(&bytes)
        .map_err(|_| "Cannot validate Deep Analyze exclusions: settings are malformed.".to_owned())?;
    let Some(value) = map.get("deepAnalyzeExcludedFolders") else {
        return Ok(vec![]);
    };
    let folders = value
        .as_array()
        .filter(|folders| folders.len() <= 256)
        .ok_or("Cannot validate Deep Analyze exclusions: expected up to 256 absolute folders.")?;
    folders
        .iter()
        .map(|value| {
            let raw = value.as_str().ok_or("Invalid excluded folder.")?;
            let folder = Path::new(raw);
            if !folder.is_absolute() {
                return Err("Excluded folders must be absolute; repair Settings before analyzing.".into());
            }
            folder.canonicalize().map_err(|_| {
                "Cannot resolve an excluded folder; restore it or repair Settings before analyzing.".into()
            })
        })
        .collect()
}

fn check_explicit_target(
    payload: &CommandPayload,
    exclusions: &[PathBuf],
    db: &Path,
) -> Result<(), String> {
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
                return Err("This folder overlaps a saved Deep Analyze exclusion.".into());
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
        let path = Path::new(&raw)
            .canonicalize()
            .map_err(|_| "An indexed target is offline; restore it before analyzing.".to_owned())?;
        if exclusions.iter().any(|excluded| path.starts_with(excluded)) {
            return Err("The target is inside a saved Deep Analyze exclusion.".into());
        }
    }
    Ok(())
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
    fn invalid_exclusion_settings_fail_closed() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("fileid-deep-exclusions-{}-{stamp}", std::process::id()));
        std::fs::write(&path, r#"{"deepAnalyzeExcludedFolders":["relative"]}"#).unwrap();
        assert!(exclusions_at(&path).is_err());
        std::fs::write(&path, b"not json").unwrap();
        assert!(exclusions_at(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
