//! Google Takeout keeps a photo's capture time in a JSON file beside it, not in the photo.

use std::io::Read;
use std::path::Path;

const SIDECAR_MAX_BYTES: u64 = 1024 * 1024;
const EARLIEST_CAPTURE_UNIX: f64 = 631_152_000.0;

/// `photoTakenTime` from `<name>.json` or `<name>.supplemental-metadata.json` beside the photo.
pub(crate) fn capture_unix(photo: &Path) -> Option<f64> {
    let name = photo.file_name()?.to_str()?;
    [format!("{name}.supplemental-metadata.json"), format!("{name}.json")]
        .into_iter()
        .map(|sidecar| photo.with_file_name(sidecar))
        .find_map(|sidecar| photo_taken_time(&sidecar))
}

fn photo_taken_time(sidecar: &Path) -> Option<f64> {
    let mut text = String::new();
    std::fs::File::open(sidecar).ok()?.take(SIDECAR_MAX_BYTES).read_to_string(&mut text).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    let stamp = &json["photoTakenTime"]["timestamp"];
    let seconds = stamp.as_str().and_then(|text| text.trim().parse::<f64>().ok()).or_else(|| stamp.as_f64())?;
    let latest = chrono::Utc::now().timestamp() as f64 + super::FUTURE_DATE_SLACK_SECONDS;
    (EARLIEST_CAPTURE_UNIX..=latest).contains(&seconds).then_some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("fileid-takeout-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn photo_taken_time_is_read_from_either_sidecar_name() {
        let dir = folder("names");
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        std::fs::write(dir.join("a.png.supplemental-metadata.json"), r#"{"photoTakenTime":{"timestamp":"1546300800"}}"#).unwrap();
        std::fs::write(dir.join("b.png"), b"x").unwrap();
        std::fs::write(dir.join("b.png.json"), r#"{"photoTakenTime":{"timestamp":"1262304000","formatted":"x"}}"#).unwrap();
        assert_eq!(capture_unix(&dir.join("a.png")), Some(1_546_300_800.0));
        assert_eq!(capture_unix(&dir.join("b.png")), Some(1_262_304_000.0));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_broken_or_implausible_sidecar_gives_no_date() {
        let dir = folder("bad");
        for (name, body) in [("c.png.json", "not json"), ("d.png.json", r#"{"photoTakenTime":{"timestamp":"0"}}"#), ("e.png.json", r#"{"title":"e"}"#)] {
            std::fs::write(dir.join(name), body).unwrap();
        }
        for photo in ["c.png", "d.png", "e.png", "f.png"] {
            assert_eq!(capture_unix(&dir.join(photo)), None, "{photo}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }
}
