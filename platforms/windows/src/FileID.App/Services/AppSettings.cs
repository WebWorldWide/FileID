// AppSettings — durable user preferences.
//
// Persisted as JSON at %LOCALAPPDATA%\FileID\app-settings.json. Atomic
// writes via temp-file + File.Move so a crash mid-save doesn't corrupt
// the file. Every new property must default safely so older settings.json
// files load cleanly.

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
        PropertyNameCaseInsensitive = false, // strict casing — case-flips can't smuggle past Sanitize
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        // Unknown fields are intentionally ignored (forward-compatibility
        // for the next schema version); Sanitize() validates every
        // declared field individually so an unknown field can't pollute
        // the in-memory state.
    };

    /// <summary>Last-picked folder root. Absolute path or null if never picked.</summary>
    public string? LastFolderPath { get; set; }

    /// <summary>Friendly display label for LastFolderPath (the leaf folder name on Windows).</summary>
    public string? LastFolderDisplay { get; set; }

    /// <summary>Sidebar visible? Persists across launches like macOS.</summary>
    public bool SidebarVisible { get; set; } = true;

    /// <summary>Active tab id. Stored as the string identifier (matches macOS RawValue persistence).</summary>
    public string ActiveTab { get; set; } = "library";

    /// <summary>Tag kept files after Cleanup auto-trash (macOS Settings.swift toggle).
    /// default flipped to true to match macOS canonical default
    /// (`cleanup.autoTagKeepers` defaults true). Without this, the same
    /// user on different OSes saw different post-cleanup tagging behavior.</summary>
    public bool CleanupAutoTagKept { get; set; } = true;

    /// <summary>Restructure view mode: false = cards, true = tree-diff.</summary>
    public bool RestructureTreeMode { get; set; } = false;

    /// <summary>Restructure folder-granularity: "loose" | "normal" | "tight". The engine
    /// reads FILEID_RESTRUCTURE_GRANULARITY (one knob shifting the cluster cosines —
    /// HDBSCAN min_cluster_size philosophy); EngineClient forwards a non-default value
    /// at spawn, so it applies on the next engine start. Mirrors the macOS
    /// @AppStorage("restructure.granularity"). Sanitize() coerces any other value.</summary>
    public string RestructureGranularity { get; set; } = "normal";

    /// <summary>Library kind filter (image / video / pdf / document / audio / all).</summary>
    public string LibraryKindFilter { get; set; } = "all";

    /// <summary>Hide marked-as-unknown clusters in People (matches macOS PeopleView toggle).</summary>
    public bool PeopleHideUnknown { get; set; } = true;

    /// <summary>
    /// Manual GPU execution provider override. Null = auto-detect (engine
    /// uses RuntimeProbe). Values: "directml", "cuda", "openvino", "qnn",
    /// "cpu". Persists across launches. Read by the engine in
    /// runtime.rs::read_user_ep_override on every session build, so a
    /// change applies to the next scan (already-loaded models keep their
    /// originally-built EP for the current scan's lifetime).
    /// </summary>
    public string? GpuExecutionProviderOverride { get; set; }

    /// <summary>Has the user seen the first-launch Welcome sheet?
    /// Mirrors macOS @AppStorage("welcomeSheetSeen"). True once the
    /// user dismisses the sheet for any reason; the sheet still re-shows
    /// on subsequent launches if any required model is missing.</summary>
    public bool WelcomeSheetSeen { get; set; } = false;

    /// <summary>Item 4: the lay-person "Tagging vs. Deep Analyze" explainer
    /// banner has been dismissed. Mirrors macOS @AppStorage("hideDeepAnalyzeExplainer").
    /// Once true the banner stays hidden across launches.</summary>
    public bool HideDeepAnalyzeExplainer { get; set; } = false;

    /// <summary>Legacy preference retained for settings-file compatibility.
    /// Runtime downloads are now always user-initiated from onboarding or
    /// Settings; this value no longer starts a background network request.</summary>
    public bool DisableAutoInstallCuda { get; set; } = false;

    /// <summary>Legacy preference retained for settings-file compatibility.
    /// The Vulkan runtime now installs only after an explicit VLM install.</summary>
    public bool DisableAutoInstallVulkanRuntime { get; set; } = false;

    /// <summary>Legacy preference retained for settings-file compatibility.
    /// CUDA components now install only from an explicit accelerator action.</summary>
    public bool DisableAutoInstallCudnn { get; set; } = false;

    /// <summary>Legacy preference retained for settings-file compatibility.
    /// OpenVINO now installs only from an explicit accelerator action.</summary>
    public bool DisableAutoInstallOpenVino { get; set; } = false;

    /// <summary>Persisted Deep Analyze VLM model — the model the Deep Analyze
    /// tab uses for full caption + smart-rename + tags. Auto-tagging during
    /// scans uses RAM++ (CLIP scene tags as fallback); this is the opt-in
    /// higher-quality path. Accepted values mirror registry.rs ids
    /// (qwen2_5_vl_7b, gemma_3_4b, mistral_small_3_2); Sanitize() coerces
    /// anything else to the default qwen2_5_vl_7b. The non-commercial
    /// qwen2_5_vl_3b (Qwen Research License) was removed.</summary>
    public string SelectedVlmModelKind { get; set; } = "qwen2_5_vl_7b";

    /// <summary>Distinguishes a deliberate model pick from the historical Qwen
    /// default. Hardware refreshes may update an automatic recommendation, but
    /// never replace a model the user explicitly selected.</summary>
    public bool SelectedVlmModelWasUserChosen { get; set; }

    /// <summary>Folders excluded from scanning. Absolute paths; the engine
    /// prunes them from the walk and purges already-cataloged rows under
    /// them at scan start (plus immediately via purgeExcluded when one is
    /// added). Sanitize() drops malformed entries, dedupes
    /// case-insensitively, and caps the list.</summary>
    public List<string> ExcludedFolders { get; set; } = new();

    /// <summary>Folders excluded from the whole-library Deep Analyze pass
    /// (deepAnalyzeAll with no fileIDs). Separate from ExcludedFolders —
    /// deliberately: a folder can be fine to catalog/tag/search but too
    /// slow or private to run the VLM over. Sent fresh with every
    /// deepAnalyzeAll; an explicit selection (Analyze Selected) is never
    /// filtered by this list. Same sanitization as ExcludedFolders.</summary>
    public List<string> DeepAnalyzeExcludedFolders { get; set; } = new();

    /// <summary>Show the "Review changes before closing?" prompt when the
    /// session change log still has undoable entries at window close.
    /// Cleared by the dialog's "Don't ask me again" checkbox.</summary>
    public bool ConfirmCloseOnPendingChanges { get; set; } = true;

    /// <summary>Schema version of this settings.json. Fresh installs start at
    /// the current version so one-time Sanitize migrations only ever touch
    /// older files (and can't clobber a fresh user's first deliberate pick).</summary>
    public int SchemaVersion { get; set; } = CurrentSchemaVersion;

    /// <summary>Whitelist of execution-provider tags the engine accepts.
    /// Matches the Rust ExecutionProvider enum in `runtime.rs`. Anything
    /// outside this set is silently coerced to null (auto-detect) so a
    /// tampered settings.json can't influence DLL search via this field.</summary>
    private static readonly HashSet<string> AllowedEpOverrides =
        new(StringComparer.OrdinalIgnoreCase)
        { "auto", "cuda", "tensorrt", "directml", "openvino", "qnn", "cpu" };

    /// <summary>VLM model ids the engine's registry.rs knows how to
    /// install. Sanitize() coerces any other value to the safe default
    /// so a tampered settings.json can't smuggle an arbitrary
    /// model_kind into the auto-chain deepAnalyzeAll call.</summary>
    private static readonly HashSet<string> AllowedVlmKinds =
        new(StringComparer.OrdinalIgnoreCase)
        { "qwen2_5_vl_7b", "gemma_3_4b", "mistral_small_3_2" };

    /// <summary>Restructure granularity values the engine accepts (anything else is the
    /// calibrated default). Mirrors the macOS AppSettings.restructureGranularityValues.</summary>
    private static readonly HashSet<string> AllowedGranularities =
        new(StringComparer.Ordinal) { "loose", "normal", "tight" };

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

    /// <summary>Current schema version this build understands. Bumped only on
    /// incompatible field renames or one-time value migrations. Sanitize()
    /// clamps loaded values to this. v2: SmolVLM became the default tagger.
    /// v3: tagging/Deep-Analyze split — SelectedVlmModelKind is the Deep
    /// Analyze model. v4: SmolVLM removed — CLIP scene tags are the canonical
    /// auto-tagger. v5: non-commercial qwen2_5_vl_3b removed (Qwen Research
    /// License) — RAM++ is the auto-tagger and Qwen2.5-VL-7B (Apache) is the
    /// default Deep Analyze model; any leftover 3B value migrates to 7B.
    /// v6: ExcludedFolders + ConfirmCloseOnPendingChanges added.
    /// v7: DeepAnalyzeExcludedFolders added.</summary>
    private const int CurrentSchemaVersion = 7;

    /// <summary>Tamper bound for ExcludedFolders / DeepAnalyzeExcludedFolders
    /// — a hand-edited settings.json can't make every scan or Deep Analyze
    /// run drag a giant exclusion list through IPC. Matches the schema's
    /// deepAnalyzeAll.excludedFolders maxItems.</summary>
    private const int MaxExcludedFolders = 256;

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
        if (!AllowedGranularities.Contains(s.RestructureGranularity))
        {
            DebugLog.Warn($"AppSettings: RestructureGranularity '{s.RestructureGranularity}' is not a recognized value; coercing to 'normal'.");
            s.RestructureGranularity = "normal";
        }
        // One-time migration: SmolVLM was removed in v4. Any stored "smolvlm"
        // is now an invalid model_kind; migrate straight to the current default
        // (qwen2_5_vl_7b; the intermediate 3B was also removed in v5).
        if (s.SchemaVersion < 4
            && string.Equals(s.SelectedVlmModelKind, "smolvlm", StringComparison.OrdinalIgnoreCase))
        {
            DebugLog.Info("AppSettings: migrating Deep Analyze model smolvlm → qwen2_5_vl_7b (SmolVLM removed).");
            s.SelectedVlmModelKind = "qwen2_5_vl_7b";
        }
        // v5: the non-commercial Qwen2.5-VL-3B (Qwen Research License) was
        // dropped for Mistral-Small-3.2 + Qwen-7B (both Apache). Migrate any
        // persisted 3B pick to the 7B default. (The AllowedVlmKinds clamp below
        // would coerce it regardless; this emits a traceable log line.)
        if (s.SchemaVersion < 5
            && string.Equals(s.SelectedVlmModelKind, "qwen2_5_vl_3b", StringComparison.OrdinalIgnoreCase))
        {
            DebugLog.Info("AppSettings: migrating Deep Analyze model qwen2_5_vl_3b → qwen2_5_vl_7b (schema v5; non-commercial 3B removed).");
            s.SelectedVlmModelKind = "qwen2_5_vl_7b";
        }

        // clamp SchemaVersion to a known range. A corrupt or
        // malicious settings.json could otherwise set 999, and a future
        // migration path that branches on version could behave unsafely.
        if (s.SchemaVersion < 0 || s.SchemaVersion > CurrentSchemaVersion)
        {
            DebugLog.Warn($"AppSettings: SchemaVersion {s.SchemaVersion} out of supported range [0, {CurrentSchemaVersion}]; coercing to {CurrentSchemaVersion}.");
            s.SchemaVersion = CurrentSchemaVersion;
        }
        // Advance any older-but-in-range schema to current so the one-time
        // migrations above don't re-run (which could clobber a later
        // deliberate re-pick once it's persisted).
        if (s.SchemaVersion < CurrentSchemaVersion)
        {
            s.SchemaVersion = CurrentSchemaVersion;
        }
        // Bound other ranges defensively.
        if (string.IsNullOrWhiteSpace(s.ActiveTab)) s.ActiveTab = "library";
        if (string.IsNullOrWhiteSpace(s.LibraryKindFilter)) s.LibraryKindFilter = "all";
        if (string.IsNullOrWhiteSpace(s.SelectedVlmModelKind)
            || !AllowedVlmKinds.Contains(s.SelectedVlmModelKind))
        {
            s.SelectedVlmModelKind = "qwen2_5_vl_7b";
        }
        s.ExcludedFolders = SanitizeExcludedFolders(s.ExcludedFolders);
        s.DeepAnalyzeExcludedFolders = SanitizeExcludedFolders(s.DeepAnalyzeExcludedFolders);
    }

    /// <summary>Drop null/whitespace/relative/invalid entries, trim trailing
    /// separators, dedupe case-insensitively (NTFS), cap the list. Also used
    /// by the Settings UI to normalize a freshly picked folder.</summary>
    internal static List<string> SanitizeExcludedFolders(IEnumerable<string>? raw)
    {
        var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        var result = new List<string>();
        foreach (var entry in raw ?? Array.Empty<string>())
        {
            if (result.Count >= MaxExcludedFolders) break;
            if (string.IsNullOrWhiteSpace(entry)) continue;
            var trimmed = entry.Trim().TrimEnd('\\', '/');
            if (trimmed.Length == 0) continue;
            bool valid;
            try
            {
                valid = Path.IsPathFullyQualified(trimmed)
                    && trimmed.IndexOfAny(Path.GetInvalidPathChars()) < 0;
            }
            catch
            {
                valid = false;
            }
            if (!valid)
            {
                DebugLog.Warn("AppSettings: dropping malformed excluded folder entry.");
                continue;
            }
            if (seen.Add(trimmed)) result.Add(trimmed);
        }
        return result;
    }

    // debounce + offload. The previous implementation ran every
    // UI-thread property setter (ActiveTab, SidebarVisible, FolderPath…)
    // through a synchronous WriteAllBytes + File.Move chain. On rapid
    // changes (tab spam, sidebar toggle, scroll-driven kind-filter
    // changes) that produced a visible UI stutter of 5-15 ms per change.
    // Now: setters call Save() to bump a debounce timer; the actual
    // write fires 200 ms after the LAST setter on a thread-pool thread
    // and is serialized through a SemaphoreSlim so concurrent debounced
    // saves and the synchronous SaveImmediately path can't race.
    private static readonly SemaphoreSlim s_writeGate = new(1, 1);
    private static readonly TimeSpan SaveDebounce = TimeSpan.FromMilliseconds(200);
    private static CancellationTokenSource? s_pendingSaveCts;

    public void Save()
    {
        // Cancel any pending save and replace with a new one. The async
        // worker observes the new token; if it gets cancelled before the
        // delay elapses, no IO happens.
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

    /// <summary>Synchronous flush. Use at shutdown to make sure the
    /// pending debounced save actually lands on disk before exit.</summary>
    public void SaveImmediately()
    {
        try
        {
            // Cancel any debounced save — the synchronous write supersedes.
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
        // Snapshot at the moment Save() was called so a setter mutating the
        // original mid-debounce doesn't corrupt the write. MemberwiseClone
        // copies primitives by value but shares reference-typed members —
        // clone the list explicitly or a mid-debounce Add/Remove mutates
        // the snapshot being serialized.
        var clone = (AppSettings)MemberwiseClone();
        clone.ExcludedFolders = new List<string>(ExcludedFolders);
        clone.DeepAnalyzeExcludedFolders = new List<string>(DeepAnalyzeExcludedFolders);
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
            // Atomic write: temp file + File.Move. Avoids partial files on crash.
            await File.WriteAllBytesAsync(tmp, bytes).ConfigureAwait(false);
            File.Move(tmp, AppPaths.SettingsPath, overwrite: true);
        }
        finally
        {
            s_writeGate.Release();
        }
    }
}
