// AppSettings — durable user preferences. The Windows analog of the macOS
// `AppStorage` / `UserDefaults` keys.
//
// Persisted as JSON at %LOCALAPPDATA%\FileID\app-settings.json. Atomic
// writes via temp-file + File.Move so a crash mid-save doesn't corrupt
// the file.
//
// Mirror of macOS Core/AppSettings.swift. Phase 1 ships a small set of
// keys; subsequent phases append new properties (each new prop must
// default safely so older settings.json files load cleanly).

using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace FileID.Services;

internal sealed class AppSettings
{
    private static readonly JsonSerializerOptions s_jsonOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = false, // strict casing — case-flips can't smuggle past Sanitize
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        // Unknown fields are intentionally ignored (forward-compatibility
        // for the next schema version); Sanitize() validates every
        // declared field individually so an unknown field can't pollute
        // the in-memory state.
    };

    /// <summary>Single-writer gate for Save(). Multiple property setters
    /// on AppViewModel (ActiveTab, SidebarVisible, FolderPath, ...) all
    /// call Save() synchronously on the UI thread. Without this lock,
    /// rapid changes (user spam-clicking tabs) race File.WriteAllBytes +
    /// File.Move on settings.json — second Save can clobber first
    /// Save's bytes. The atomic-write protects against crashes, not
    /// concurrent writers.</summary>
    private static readonly object s_saveLock = new();

    /// <summary>Last-picked folder root. Absolute path or null if never picked.</summary>
    public string? LastFolderPath { get; set; }

    /// <summary>Friendly display label for LastFolderPath (the leaf folder name on Windows).</summary>
    public string? LastFolderDisplay { get; set; }

    /// <summary>Sidebar visible? Persists across launches like macOS.</summary>
    public bool SidebarVisible { get; set; } = true;

    /// <summary>Active tab id. Stored as the string identifier (matches macOS RawValue persistence).</summary>
    public string ActiveTab { get; set; } = "library";

    /// <summary>Tag kept files after Cleanup auto-trash (macOS Settings.swift toggle).</summary>
    public bool CleanupAutoTagKept { get; set; } = false;

    /// <summary>Restructure view mode: false = cards, true = tree-diff.</summary>
    public bool RestructureTreeMode { get; set; } = false;

    /// <summary>Library kind filter (image / video / pdf / document / audio / all).</summary>
    public string LibraryKindFilter { get; set; } = "all";

    /// <summary>Hide marked-as-unknown clusters in People (matches macOS PeopleView toggle).</summary>
    public bool PeopleHideUnknown { get; set; } = false;

    /// <summary>
    /// Manual GPU execution provider override. Null = auto-detect (engine
    /// uses RuntimeProbe). Values: "directml", "cuda", "openvino", "qnn",
    /// "cpu". Persists across launches; engine consumes this when the real
    /// ML inference path lands (Phase 2.6).
    /// </summary>
    public string? GpuExecutionProviderOverride { get; set; }

    /// <summary>Has the user seen the first-launch Welcome sheet?
    /// Mirrors macOS @AppStorage("welcomeSheetSeen"). True once the
    /// user dismisses the sheet for any reason; the sheet still re-shows
    /// on subsequent launches if any required model is missing.</summary>
    public bool WelcomeSheetSeen { get; set; } = false;

    /// <summary>Schema version of this settings.json. Bumped only on incompatible field renames.</summary>
    public int SchemaVersion { get; set; } = 1;

    /// <summary>Whitelist of execution-provider tags the engine accepts.
    /// Matches the Rust ExecutionProvider enum in `runtime.rs`. Anything
    /// outside this set is silently coerced to null (auto-detect) so a
    /// tampered settings.json can't influence DLL search via this field.</summary>
    private static readonly HashSet<string> AllowedEpOverrides =
        new(StringComparer.OrdinalIgnoreCase)
        { "auto", "cuda", "tensorrt", "directml", "openvino", "qnn", "cpu" };

    public static AppSettings Load()
    {
        try
        {
            if (File.Exists(AppPaths.SettingsPath))
            {
                var bytes = File.ReadAllBytes(AppPaths.SettingsPath);
                var loaded = JsonSerializer.Deserialize<AppSettings>(bytes, s_jsonOptions);
                if (loaded is not null)
                {
                    Sanitize(loaded);
                    return loaded;
                }
            }
        }
        catch (Exception ex)
        {
            // Corrupt settings file shouldn't brick the app. Log + start fresh.
            DebugLog.Warn("AppSettings.Load failed: " + ex.Message);
        }
        return new AppSettings();
    }

    /// <summary>Defensive cleanup of fields a malicious settings.json
    /// could otherwise smuggle through. Currently scrubs the EP override
    /// (rejects anything outside the canonical enum so DLL paths can't
    /// be injected). Add new validations here as fields are added.</summary>
    private static void Sanitize(AppSettings s)
    {
        if (s.GpuExecutionProviderOverride is { } v
            && !AllowedEpOverrides.Contains(v))
        {
            DebugLog.Warn($"AppSettings: GpuExecutionProviderOverride '{v}' is not a recognized value; coercing to null (auto-detect).");
            s.GpuExecutionProviderOverride = null;
        }
    }

    public void Save()
    {
        // Serialize the entire write through s_saveLock so concurrent
        // setters can't race File.Move. Brief lock — typical save is
        // < 5 ms (small JSON, single SSD write).
        lock (s_saveLock)
        {
            try
            {
                AppPaths.EnsureDirectories();
                var bytes = JsonSerializer.SerializeToUtf8Bytes(this, s_jsonOptions);
                // Atomic write: temp file + File.Move. Avoids partial files on crash.
                var tmp = AppPaths.SettingsPath + ".tmp";
                File.WriteAllBytes(tmp, bytes);
                File.Move(tmp, AppPaths.SettingsPath, overwrite: true);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("AppSettings.Save failed: " + ex.Message);
            }
        }
    }
}
