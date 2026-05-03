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
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
    };

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

    /// <summary>Schema version of this settings.json. Bumped only on incompatible field renames.</summary>
    public int SchemaVersion { get; set; } = 1;

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

    public void Save()
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
