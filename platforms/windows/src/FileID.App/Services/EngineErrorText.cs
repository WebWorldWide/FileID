namespace FileID.Services;

/// <summary>Plain titles and sentences for the errors FileID's helper
/// reports, keyed on the error kind. Mirrors the Mac app's
/// <c>errorTitle</c> / <c>errorText</c> (Sidebar.swift) and adds the kinds
/// only the Windows helper sends.</summary>
internal static class EngineErrorText
{
    public const string TryAgain = "Try again. If it keeps happening, quit and reopen FileID.";

    public static string Title(string? kind) => kind switch
    {
        "discovery_partial" => "Some files were left out",
        "empty_folder" => "No files to scan",
        "scan_failed" => "The scan stopped",
        "scan_already_running" => "A scan is already running",
        "db_unavailable" or "db_failed" or "db_open_failed" or "db_integrity_check_failed"
            or "plan_restructure_db_unavailable" => "The library couldn't be opened",
        "db_newer_than_engine" => "This library needs a newer FileID",
        "folder_access_denied" or "bookmark_unavailable" or "list_roots_failed" => "FileID can't reach that folder",
        "command_decode_failed" => "FileID couldn't do that",
        "plan_restructure_failed" => "The folder plan didn't finish",
        "apply_restructure" => "The folder move didn't finish",
        "undo_restructure" => "Putting folders back didn't finish",
        "orphan_sweep_failed" or "purge_excluded_failed" => "Cleaning up the library didn't finish",
        "model_load_failed" => "An AI model didn't load",
        "restructure_busy" or "restructure_already_running" or "plan_restructure_library_busy" => "A folder move is already running",
        "engine_exited" or "engine_output_error" => "FileID's helper stopped",
        "ipc_frame_too_large" => "Too much to show at once",
        "unknown_model" or "prewarm_failed" or "models_not_installed" or "vlm_model_missing" or "llama_cpp_missing"
            or "models_dir_unavailable" or "cuda_dll_registration_failed" => "The AI model couldn't be used",
        "model_download_failed" or "download_tls_pin_failed" or "zip_extract_failed" => "A download didn't finish",
        "gpu_device_removed" => "The graphics card stopped responding",
        "deep_analyze_failed" => "Describing files stopped",
        "deep_analyze_already_running" => "Files are already being described",
        "face_clustering_failed" or "face_clustering_aborted" or "face_clustering_start_failed"
            or "face_cluster_persist_failed" => "Grouping faces didn't finish",
        "face_clustering_busy" => "Faces are already being grouped",
        "find_merge_suggestions_failed" => "Looking for matching people didn't finish",
        _ => "Something went wrong"
    };

    /// <summary>The sentence under the title. Known kinds get fixed wording;
    /// anything else shows the helper's own message unless it reads like a
    /// log line.</summary>
    public static string Text(string? kind, string? message) => kind switch
    {
        "scan_failed" => "Press Scan again. Files already read are skipped.",
        "scan_already_running" or "deep_analyze_already_running" or "face_clustering_busy" => "Wait for it to finish, then try again.",
        "db_unavailable" or "db_failed" or "db_open_failed" or "db_integrity_check_failed" or "plan_restructure_db_unavailable"
            => "Quit and reopen FileID. If it keeps happening, use Start over in the folder menu.",
        "db_newer_than_engine" => "This library was made by a newer FileID. Update FileID.",
        "folder_access_denied" or "bookmark_unavailable" or "list_roots_failed" => "Choose the folder again so FileID can open it.",
        "command_decode_failed" => TryAgain,
        "plan_restructure_failed" => "Open Organize Folders and try again, or pick a smaller folder.",
        "orphan_sweep_failed" or "purge_excluded_failed" => "Nothing was lost. Scan again to finish tidying up.",
        "model_load_failed" or "prewarm_failed" or "unknown_model" or "models_not_installed" or "vlm_model_missing"
            or "llama_cpp_missing" or "models_dir_unavailable" => "Open Settings and install the model again.",
        "model_download_failed" or "download_tls_pin_failed" or "zip_extract_failed"
            => "Check your internet connection, then open Settings and try the download again.",
        "restructure_busy" or "restructure_already_running" or "plan_restructure_library_busy" => "Wait for it to finish, then try again.",
        "ipc_frame_too_large" => "That folder plan is too large to show. Try a smaller folder.",
        "gpu_device_removed" => "FileID's helper is restarting. Press Scan again when it is ready.",
        "engine_exited" or "engine_output_error" => "FileID restarts it by itself. If it stays off, quit and reopen FileID.",
        "face_clustering_failed" or "face_clustering_aborted" or "face_clustering_start_failed" or "face_cluster_persist_failed"
            => "Open People and press Refresh to try again.",
        "find_merge_suggestions_failed" => "Open People and try Suggested merges again in a moment.",
        _ => Plain(message, TryAgain)
    };

    /// <summary>The helper's own sentence when it reads like one, otherwise
    /// <paramref name="fallback"/>.</summary>
    public static string Plain(string? message, string fallback) =>
        LooksTechnical(message) ? fallback : message!.Trim();

    private static readonly string[] s_technicalMarkers =
    {
        "error domain", "sqlite", "hresult", "errno", "0x", "::", "panicked", "unwrap", "serde", "fatal", "exception",
        "stack", "thread", "os error", " ipc", "engine", "database", "catalog", "embedding", "onnx", "directml", "cuda",
        "llama", "gguf", "pid ", "/users/", "/volumes/", ":\\", "(code", "code=", "some(", "none)", "{", "}", "_", "\\",
    };

    public static bool LooksTechnical(string? message)
    {
        if (string.IsNullOrWhiteSpace(message) || message.Length > 220) return true;
        foreach (var marker in s_technicalMarkers)
        {
            if (message.Contains(marker, StringComparison.OrdinalIgnoreCase)) return true;
        }
        return false;
    }
}
