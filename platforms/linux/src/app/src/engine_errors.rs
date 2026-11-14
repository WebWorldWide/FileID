// Plain titles and sentences for the helper's error kinds, mirroring the Mac
// app's `Sidebar.errorTitle` / `errorText`. Codes, model ids and raw messages
// belong in a tooltip (`detail`), never in primary text.

pub fn title(kind: &str) -> &'static str {
    match kind {
        "discovery_partial" => "Some files were left out",
        "empty_folder" => "No files to scan",
        "scan_failed" => "The scan stopped",
        "db_unavailable" | "db_failed" => "The library couldn't be opened",
        "db_newer_than_engine" => "This library needs a newer FileID",
        "folder_access_denied" | "bookmark_unavailable" => "FileID can't reach that folder",
        "command_decode_failed" => "FileID couldn't do that",
        "plan_restructure_failed" => "The folder plan didn't finish",
        "orphan_sweep_failed" | "purge_excluded_failed" => "Cleaning up the library didn't finish",
        "model_load_failed" => "An AI model didn't load",
        "restructure_busy" | "restructure_already_running" => "A folder move is already running",
        "engine_exited" | "engine_disconnected" => "FileID's helper stopped",
        "ipc_frame_too_large" | "oversized_frame" => "Too much to show at once",
        "unknown_model" | "prewarm_failed" => "The AI model couldn't be used",
        "model_download_failed" | "download_tls_pin_failed" => "The download didn't finish",
        "model_download_disk_full" => "Not enough disk space",
        "scan_already_running" => "A scan is already running",
        _ => "Something went wrong",
    }
}

pub fn text(kind: &str, message: &str) -> String {
    let fixed = match kind {
        "scan_failed" => "Press Scan again. Files already read are skipped.",
        "db_unavailable" | "db_failed" => "Quit and reopen FileID. If it keeps happening, use Start over in the folder menu.",
        "db_newer_than_engine" => "This library was made by a newer FileID. Update FileID.",
        "folder_access_denied" | "bookmark_unavailable" => "Choose the folder again so FileID can open it.",
        "command_decode_failed" => "Try again. If it keeps happening, quit and reopen FileID.",
        "plan_restructure_failed" => "Open Organize Folders and try again, or pick a smaller folder.",
        "orphan_sweep_failed" | "purge_excluded_failed" => "Nothing was lost. Scan again to finish tidying up.",
        "model_load_failed" | "prewarm_failed" | "unknown_model" => "Open Settings and install the model again.",
        "restructure_busy" | "restructure_already_running" | "scan_already_running" => {
            "Wait for it to finish, then try again."
        }
        "ipc_frame_too_large" | "oversized_frame" => "That folder plan is too large to show. Try a smaller folder.",
        "engine_exited" | "engine_disconnected" => "It restarts on its own. If this stays, quit and reopen FileID.",
        "model_download_failed" => "Check your internet connection, then press Retry.",
        "download_tls_pin_failed" => "The download couldn't be checked as safe, so it was stopped. Try again later.",
        "model_download_disk_full" => "Free some space on this computer, then press Retry.",
        _ if looks_technical(message) => "Try again. If it keeps happening, quit and reopen FileID.",
        _ => return message.to_owned(),
    };
    fixed.to_owned()
}

/// Title and sentence on one line, for places with a single label.
pub fn sentence(kind: &str, message: &str) -> String {
    let title = title(kind);
    let text = text(kind, message);
    if text.is_empty() { title.to_owned() } else { format!("{title}. {text}") }
}

/// What a tooltip may carry: the code, the model id and the raw message.
pub fn detail(kind: &str, message: &str, model_kind: Option<&str>) -> String {
    let mut detail = kind.to_owned();
    if let Some(model) = model_kind.filter(|model| !model.is_empty()) {
        detail.push_str(&format!(" ({model})"));
    }
    if !message.is_empty() {
        detail.push_str(": ");
        detail.push_str(message);
    }
    detail
}

pub fn looks_technical(message: &str) -> bool {
    if message.is_empty() || message.chars().count() > 220 {
        return true;
    }
    const MARKERS: &[&str] = &[
        "error domain", "sqlite", "errno", "0x", "::", "fatal", "exception", "stack", "thread", "posix", " ipc",
        "engine", "database", "catalog", "embedding", "onnx", "mlx", "pid ", "/home/", "/users/", "/volumes/",
        "/mnt/", "/media/", "(code", "code=", "os error", "panicked", "serde", "{", "}", "_", "\\", "rusqlite",
        "llama",
    ];
    let lowered = message.to_lowercase();
    MARKERS.iter().any(|marker| lowered.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_kinds_get_the_mac_wording() {
        assert_eq!(title("scan_failed"), "The scan stopped");
        assert_eq!(text("scan_failed", "walkdir: os error 13"), "Press Scan again. Files already read are skipped.");
        assert_eq!(title("db_failed"), title("db_unavailable"));
        assert_eq!(title("restructure_busy"), "A folder move is already running");
        assert_eq!(text("model_load_failed", "x"), "Open Settings and install the model again.");
        assert_eq!(title("no_such_kind"), "Something went wrong");
    }

    #[test]
    fn unknown_kinds_show_a_plain_message_but_never_a_log_line() {
        assert_eq!(text("whatever", "That folder is empty."), "That folder is empty.");
        for raw in [
            "",
            "SQLITE_BUSY: database is locked",
            "No such file or directory (os error 2)",
            "failed to open /home/sam/Pictures/a.jpg",
            "thread 'main' panicked at src/lib.rs",
            "model_kind mobileclip_s2 missing",
            "ort::Error { code: 3 }",
        ] {
            assert_eq!(
                text("whatever", raw),
                "Try again. If it keeps happening, quit and reopen FileID.",
                "{raw:?} reached primary text"
            );
        }
        assert!(looks_technical(&"a".repeat(221)));
    }

    #[test]
    fn primary_text_never_carries_codes_or_model_ids() {
        for kind in [
            "scan_failed", "db_unavailable", "model_load_failed", "prewarm_failed", "unknown_model",
            "model_download_failed", "model_download_disk_full", "oversized_frame", "engine_disconnected",
            "restructure_already_running", "something_new",
        ] {
            let shown = sentence(kind, "mobileclip_s2: ort::Error code=7");
            assert!(!shown.contains('_') && !shown.contains("::") && !shown.contains("mobileclip"), "{shown}");
            assert!(!shown.to_lowercase().contains("engine"), "{shown}");
        }
    }

    #[test]
    fn the_tooltip_keeps_the_technical_detail() {
        assert_eq!(
            detail("model_load_failed", "ort::Error code=7", Some("mobileclip_s2")),
            "model_load_failed (mobileclip_s2): ort::Error code=7"
        );
        assert_eq!(detail("scan_failed", "", None), "scan_failed");
    }
}
