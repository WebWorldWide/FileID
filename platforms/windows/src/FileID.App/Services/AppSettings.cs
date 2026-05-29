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

    /// <summary>Library kind filter (image / video / pdf / document / audio / all).</summary>
    public string LibraryKindFilter { get; set; } = "all";

    /// <summary>Hide marked-as-unknown clusters in People (matches macOS PeopleView toggle).</summary>
    public bool PeopleHideUnknown { get; set; } = false;

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

    /// <summary>Opt out of the silent CUDA llama.cpp install that fires
    /// when the engine reports an NVIDIA GPU. False (the default) means
    /// auto-install is enabled — Deep Analyze gets the 15-25% faster
    /// CUDA build without the user finding a hidden Settings button.
    /// True disables the auto-install entirely.</summary>
    public bool DisableAutoInstallCuda { get; set; } = false;

    /// <summary>Default false (auto-install enabled). On engine-ready,
    /// the Vulkan llama.cpp runtime is fetched silently so Deep Analyze
    /// works without the user finding the Install button. Vulkan ships
    /// on every GPU vendor — no NVIDIA gate. True disables the
    /// auto-install entirely (manual install still available via the
    /// engine's PrewarmModel IPC if a user-facing button is added).</summary>
    public bool DisableAutoInstallVulkanRuntime { get; set; } = false;

    /// <summary>Default false (auto-install enabled). On engine-ready
    /// AND NVIDIA hardware, cuDNN is fetched from NVIDIA's public CDN
    /// (developer.download.nvidia.com) so the ORT CUDA EP can replace
    /// DirectML for scanning (~10-15% throughput on RTX-class). True
    /// disables; users can fall back to system-installed CUDA Toolkit
    /// + cuDNN if they prefer the BYO path.</summary>
    public bool DisableAutoInstallCudnn { get; set; } = false;

    /// <summary>Persisted Deep Analyze VLM model — the model the Deep Analyze
    /// tab uses for full caption + smart-rename + tags. Auto-tagging during
    /// scans uses RAM++ (CLIP scene tags as fallback); this is the opt-in
    /// higher-quality path. Accepted values mirror registry.rs ids
    /// (qwen2_5_vl_7b, gemma_3_4b, mistral_small_3_2); Sanitize() coerces
    /// anything else to the default qwen2_5_vl_7b. The non-commercial
    /// qwen2_5_vl_3b (Qwen Research License) was removed.</summary>
    public string SelectedVlmModelKind { get; set; } = "qwen2_5_vl_7b";

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

    /// <summary>True if <paramref name="kind"/> is a VLM model_kind the engine
    /// can install. The Deep Analyze card guards use this to reject removed /
    /// non-commercial models (e.g. the dropped qwen2_5_vl_3b).</summary>
    public static bool IsAllowedVlmKind(string? kind) =>
        kind is { } k && AllowedVlmKinds.Contains(k);

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
    /// default Deep Analyze model; any leftover 3B value migrates to 7B.</summary>
    private const int CurrentSchemaVersion = 5;

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
        // Settings is value-shaped (all primitive properties). A shallow
        // copy is enough for serialization — and importantly, the snapshot
        // captures the state at the moment Save() was called so a setter
        // mutating the original mid-debounce doesn't corrupt the write.
        return (AppSettings)MemberwiseClone();
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
