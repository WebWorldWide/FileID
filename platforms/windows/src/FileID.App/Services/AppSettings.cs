// User preferences persisted to %LOCALAPPDATA%\FileID\app-settings.json. Every property must default safely so older files load.

using System;
using System.IO;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;
using System.Threading.Tasks;

namespace FileID.Services;

internal sealed class AppSettings
{
    private static readonly JsonSerializerOptions s_jsonOptions = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        PropertyNameCaseInsensitive = false,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        // Unknown fields are ignored for forward compatibility; Sanitize() validates every declared field.
    };

    /// <summary>Absolute path or null if never picked.</summary>
    public string? LastFolderPath { get; set; }

    /// <summary>Friendly display label for LastFolderPath (the leaf folder name on Windows).</summary>
    public string? LastFolderDisplay { get; set; }

    public bool SidebarVisible { get; set; } = true;

    public string ActiveTab { get; set; } = "library";

    public bool CleanupAutoTagKept { get; set; } = true;

    public bool RestructureTreeMode { get; set; } = false;

    public long? LibraryScopeRootId { get; set; }

    public string LibraryKindFilter { get; set; } = "all";

    public bool PeopleHideUnknown { get; set; } = false;

    public string? GpuExecutionProviderOverride { get; set; }

    public bool WelcomeSheetSeen { get; set; } = false;

    public bool HideDeepAnalyzeExplainer { get; set; } = false;

    public bool DisableAutoInstallCuda { get; set; } = false;

    public bool DisableAutoInstallVulkanRuntime { get; set; } = false;

    public bool DisableAutoInstallCudnn { get; set; } = false;

    public bool DisableAutoInstallOpenVino { get; set; } = false;

    public string SelectedVlmModelKind { get; set; } = "qwen2_5_vl_7b";

    public string NamingStyle { get; set; } = ViewModels.NameStylePolicy.DefaultStyle;

    public int NamingMaxLength { get; set; } = ViewModels.NameStylePolicy.DefaultMaxLength;

    public bool NamingIncludePeople { get; set; } = ViewModels.NameStylePolicy.DefaultIncludePeople;

    public bool NamingIncludeEvent { get; set; } = ViewModels.NameStylePolicy.DefaultIncludeEvent;

    public bool NamingIncludeDate { get; set; } = ViewModels.NameStylePolicy.DefaultIncludeDate;

    public bool NamingLearn { get; set; } = ViewModels.NameStylePolicy.DefaultLearn;

    public string LavaMode { get; set; } = LavaModeFull;

    public string TextSize { get; set; } = "standard";

    public string RestructureGranularity { get; set; } = RestructureGranularityDefault;

    public List<string> DeepAnalyzeExcludedFolders { get; set; } = [];

    /// <summary>Windows Chat answers from stored evidence only until a local chat model ships.</summary>
    public bool ChatUseModel { get; set; } = true;

    public const string LavaModeFull = "full";
    public const string LavaModeOff = "off";
    public const string RestructureGranularityDefault = "normal";
    public const int MaxExcludedFolders = 256;

    public static readonly IReadOnlyList<string> RestructureGranularityValues = ["loose", "normal", "tight"];
    public static readonly IReadOnlyList<string> TextSizeValues = ["small", "standard", "large", "larger"];

    public bool BackgroundMotionOn => LavaMode != LavaModeOff;

    public string? RestructureGranularityForEngine =>
        RestructureGranularity != RestructureGranularityDefault && RestructureGranularityValues.Contains(RestructureGranularity)
            ? RestructureGranularity
            : null;

    /// <summary>Drops blank and relative entries, trims trailing separators, dedupes (case-insensitive, like NTFS) and caps the list at the schema's 256.</summary>
    public static List<string> SanitizeExcludedFolders(IEnumerable<string>? raw)
    {
        var result = new List<string>();
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (var entry in raw ?? [])
        {
            if (result.Count >= MaxExcludedFolders) break;
            var folder = entry?.Trim() ?? "";
            if (!Path.IsPathFullyQualified(folder)) continue;
            while (folder.Length > 3 && (folder[^1] == '\\' || folder[^1] == '/')) folder = folder[..^1];
            if (seen.Add(folder)) result.Add(folder);
        }
        return result;
    }

    /// <summary>Fresh installs start at the current version so the one-time migrations in Sanitize only touch older files.</summary>
    public int SchemaVersion { get; set; } = CurrentSchemaVersion;

    /// <summary>Only "cpu" changes engine behaviour; the older provider names stay accepted so existing settings files round-trip.</summary>
    private static readonly HashSet<string> AllowedEpOverrides =
        new(StringComparer.OrdinalIgnoreCase)
        { "auto", "cuda", "tensorrt", "directml", "openvino", "qnn", "cpu" };

    /// <summary>Sanitize() coerces any other value to the default so a tampered settings.json can't inject an arbitrary model_kind into deepAnalyzeAll.</summary>
    private static readonly HashSet<string> AllowedVlmKinds =
        new(StringComparer.OrdinalIgnoreCase)
        { "qwen2_5_vl_7b", "qwen3_vl_4b", "qwen3_vl_8b", "gemma_3_4b", "mistral_small_3_2" };

    /// <summary>True if kind is a VLM model_kind the engine can install: a curated kind, or a catalog GGUF id picked in the model browser (the engine only resolves ids in its validated catalog).</summary>
    public static bool IsAllowedVlmKind(string? kind) =>
        kind is { } k && (AllowedVlmKinds.Contains(k) || ViewModels.ModelCatalogPresentation.IsCatalogModelKind(k));

    public static bool IsCuratedVlmKind(string? kind) => kind is { } k && AllowedVlmKinds.Contains(k);

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
            // A corrupt settings file falls back to defaults.
            DebugLog.Warn("AppSettings.Load failed: " + ex.Message);
        }
        return new AppSettings();
    }

    /// <summary>Bumped only on incompatible field renames or one-time value migrations.</summary>
    private const int CurrentSchemaVersion = 5;

    /// <summary>Validates every loaded field. The execution-provider override must be in the allowed set so a DLL path can't be injected.</summary>
    private static void Sanitize(AppSettings s)
    {
        if (s.LibraryScopeRootId is <= 0) s.LibraryScopeRootId = null;
        if (s.GpuExecutionProviderOverride is { } v
            && !AllowedEpOverrides.Contains(v))
        {
            DebugLog.Warn($"AppSettings: GpuExecutionProviderOverride '{v}' is not a recognized value; coercing to null (auto-detect).");
            s.GpuExecutionProviderOverride = null;
        }
        if (s.SchemaVersion < 4
            && string.Equals(s.SelectedVlmModelKind, "smolvlm", StringComparison.OrdinalIgnoreCase))
        {
            DebugLog.Info("AppSettings: migrating Deep Analyze model smolvlm → qwen2_5_vl_7b (SmolVLM removed).");
            s.SelectedVlmModelKind = "qwen2_5_vl_7b";
        }
        if (s.SchemaVersion < 5
            && string.Equals(s.SelectedVlmModelKind, "qwen2_5_vl_3b", StringComparison.OrdinalIgnoreCase))
        {
            DebugLog.Info("AppSettings: migrating Deep Analyze model qwen2_5_vl_3b → qwen2_5_vl_7b (schema v5; non-commercial 3B removed).");
            s.SelectedVlmModelKind = "qwen2_5_vl_7b";
        }

        // A tampered version number could send a future version-branching migration down the wrong path.
        if (s.SchemaVersion < 0 || s.SchemaVersion > CurrentSchemaVersion)
        {
            DebugLog.Warn($"AppSettings: SchemaVersion {s.SchemaVersion} out of supported range [0, {CurrentSchemaVersion}]; coercing to {CurrentSchemaVersion}.");
            s.SchemaVersion = CurrentSchemaVersion;
        }
        // Advance to current so the one-time migrations above never re-run over a later deliberate pick.
        if (s.SchemaVersion < CurrentSchemaVersion)
        {
            s.SchemaVersion = CurrentSchemaVersion;
        }
        if (string.IsNullOrWhiteSpace(s.ActiveTab)) s.ActiveTab = "library";
        if (string.IsNullOrWhiteSpace(s.LibraryKindFilter)) s.LibraryKindFilter = "all";
        if (string.IsNullOrWhiteSpace(s.SelectedVlmModelKind)
            || !IsAllowedVlmKind(s.SelectedVlmModelKind))
        {
            s.SelectedVlmModelKind = "qwen2_5_vl_7b";
        }
        if (s.LavaMode != LavaModeOff) s.LavaMode = LavaModeFull;
        if (!TextSizeValues.Contains(s.TextSize)) s.TextSize = "standard";
        if (!RestructureGranularityValues.Contains(s.RestructureGranularity)) s.RestructureGranularity = RestructureGranularityDefault;
        s.DeepAnalyzeExcludedFolders = SanitizeExcludedFolders(s.DeepAnalyzeExcludedFolders);
        s.NamingStyle = ViewModels.NameStylePolicy.SanitizeStyle(s.NamingStyle);
        s.NamingMaxLength = ViewModels.NameStylePolicy.ClampLength(s.NamingMaxLength);
    }

    private static readonly SemaphoreSlim s_writeGate = new(1, 1);
    // Save() debounces: the write fires 200 ms after the last setter on a thread-pool thread, keeping rapid changes off the UI thread. s_writeGate serializes debounced saves with SaveImmediately.
    private static readonly TimeSpan SaveDebounce = TimeSpan.FromMilliseconds(200);
    private static CancellationTokenSource? s_pendingSaveCts;

    public void Save()
    {
        // A save cancelled before its delay elapses does no IO.
        var newCts = new CancellationTokenSource();
        var prior = Interlocked.Exchange(ref s_pendingSaveCts, newCts);
        try { prior?.Cancel(); prior?.Dispose(); } catch { /* swallow */ }
        var snapshot = CloneForWrite();
        _ = Task.Run(async () =>
        {
            try
            {
                await Task.Delay(SaveDebounce, newCts.Token).ConfigureAwait(false);
                await WriteAsync(snapshot).ConfigureAwait(false);
            }
            catch (OperationCanceledException) { /* superseded */ }
            catch (Exception ex)
            {
                DebugLog.Warn("AppSettings.Save (debounced) failed: " + ex.Message);
            }
        });
    }

    /// <summary>Synchronous flush for shutdown, so a pending debounced save reaches disk.</summary>
    public void SaveImmediately()
    {
        try
        {
            var prior = Interlocked.Exchange(ref s_pendingSaveCts, null);
            try { prior?.Cancel(); prior?.Dispose(); } catch { /* swallow */ }
            var snapshot = CloneForWrite();
            WriteAsync(snapshot).GetAwaiter().GetResult();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("AppSettings.SaveImmediately failed: " + ex.Message);
        }
    }

    private AppSettings CloneForWrite()
    {
        // Shallow copy suffices; the snapshot pins state at Save() time so a setter during the debounce can't tear the write.
        var clone = (AppSettings)MemberwiseClone();
        clone.DeepAnalyzeExcludedFolders = [.. DeepAnalyzeExcludedFolders];
        return clone;
    }

    private static async Task WriteAsync(AppSettings snapshot)
    {
        await s_writeGate.WaitAsync().ConfigureAwait(false);
        try
        {
            AppPaths.EnsureDirectories();
            var bytes = JsonSerializer.SerializeToUtf8Bytes(snapshot, s_jsonOptions);
            var tmp = AppPaths.SettingsPath + ".tmp";
            // Temp file + move so a crash mid-save leaves no partial file.
            await File.WriteAllBytesAsync(tmp, bytes).ConfigureAwait(false);
            File.Move(tmp, AppPaths.SettingsPath, overwrite: true);
        }
        finally
        {
            s_writeGate.Release();
        }
    }
}
