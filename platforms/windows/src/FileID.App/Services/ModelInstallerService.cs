// ModelInstallerService — per-model install state for the Welcome sheet.
// PRIVACY: never makes a network call. Only sends IPC commands; the engine is the sole network surface.

using System.ComponentModel;
using System.Runtime.CompilerServices;
using System.Threading;
using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Dispatching;

namespace FileID.Services;

internal sealed class ModelInstallerService : INotifyPropertyChanged
{
    // The engine writes a flat or content-hashed completion marker in `%LOCALAPPDATA%\FileID\Models\.sentinels\` for each model bundle.
    private static readonly string[] ClipSentinelIds = { "mobileclip_s2", "clip_text" };
    private static readonly string[] ArcfaceSentinelIds = { "arcface" };
    private static readonly string[] DeepVlmSentinelIds = { "qwen2_5_vl_7b", "qwen3_vl_4b", "qwen3_vl_8b", "gemma_3_4b", "mistral_small_3_2" };
    private static readonly string[] RamPlusSentinelIds = { "ram_plus" };
    private static readonly string[] FaceMeshSentinelIds = { "face_mesh" };
    private static readonly string[] AcceleratorSentinelIds = { "ort_cuda_x64", "ort_openvino_x64" };

    private static readonly TimeSpan WaitForReadyTimeout = TimeSpan.FromSeconds(75);

    private static readonly TimeSpan NoProgressTimeout = TimeSpan.FromSeconds(60);

    public static ModelInstallerService Instance { get; } = new();

    public ModelSlot Clip { get; }
    public ModelSlot Arcface { get; }
    /// <summary>Optional; when absent the engine falls back to CLIP scene tags, so it is NOT (yet) a gate on AllInstalled.</summary>
    public ModelSlot RamPlus { get; }
    /// <summary>Never installed by Install all and never a gate on AllInstalled.</summary>
    public ModelSlot FaceMesh { get; }
    public ModelSlot DeepVlm { get; }
    public ModelSlot Accelerator { get; }

    public bool AcceleratorIsRealInstall
    {
        get => _acceleratorIsRealInstall;
        private set => Set(ref _acceleratorIsRealInstall, value);
    }
    private bool _acceleratorIsRealInstall;

    private int _installAllInFlight; // 0 = idle, 1 = in flight

    // UI dispatcher captured from a guaranteed UI-thread entry point (the public install methods, before their first ConfigureAwait(false)).
    private Microsoft.UI.Dispatching.DispatcherQueue? _uiDispatcher;

    private string _deepVlmModelKind = "qwen2_5_vl_7b";

    // captured on the UI thread at ctor time (the singleton is first touched during app startup on the UI thread — the same thread on which it constructs the ModelSlots below, which rely on the same capture).
    private readonly Microsoft.UI.Dispatching.DispatcherQueue? _ui;

    private ModelInstallerService()
    {
        _ui = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        Clip = new ModelSlot(
            displayLabel: "CLIP ViT-B/32",
            approxBytes: 220UL * 1024 * 1024,
            // The engine's pre-scan check requires both sentinels.
            installAction: async () =>
            {
                ClearCancelMarks("mobileclip_s2", "clip_text");
                await PrewarmAsync("mobileclip_s2").ConfigureAwait(false);
                await PrewarmAsync("clip_text").ConfigureAwait(false);
            }, uiDispatcher: _ui);
        Arcface = new ModelSlot(
            displayLabel: "Face models (YuNet + SFace)",
            approxBytes: 39UL * 1024 * 1024,
            installAction: () =>
            {
                ClearCancelMarks("arcface_default");
                return PrewarmAsync("arcface_default");
            }, uiDispatcher: _ui);
        RamPlus = new ModelSlot(
            displayLabel: "RAM++ image tagger",
            approxBytes: 925_600_000UL,
            installAction: () =>
            {
                ClearCancelMarks("ram_plus");
                return PrewarmAsync("ram_plus");
            }, uiDispatcher: _ui);
        FaceMesh = new ModelSlot(
            displayLabel: "Face Mesh eye model",
            approxBytes: 4_920_995UL,
            installAction: () =>
            {
                ClearCancelMarks("face_mesh");
                return PrewarmAsync("face_mesh");
            }, uiDispatcher: _ui);
        DeepVlm = new ModelSlot(
            displayLabel: "Qwen2.5-VL 7B",
            approxBytes: 6_100_000_000UL,
            installAction: async () =>
            {
                ClearCancelMarks(_deepVlmModelKind);
                // Persist the hardware-recommended Deep Analyze model so the Deep Analyze tab + manual auto-chain use what the user just downloaded.
                PersistSelectedVlmModelKind(_deepVlmModelKind);
                await PrewarmAsync(_deepVlmModelKind).ConfigureAwait(false);
            }, uiDispatcher: _ui);
        // Until then, the row shows "Detecting GPU…" so the user knows it's waiting.
        Accelerator = new ModelSlot(
            displayLabel: "Bundled GPU acceleration",
            approxBytes: 0,
            installAction: () => Task.CompletedTask,
            uiDispatcher: _ui);
        Accelerator.Message = "Scan models run on the CPU with tract, the same on every platform.";

        Clip.PropertyChanged += OnSlotPropertyChanged;
        Arcface.PropertyChanged += OnSlotPropertyChanged;
        RamPlus.PropertyChanged += OnSlotPropertyChanged;
        FaceMesh.PropertyChanged += OnSlotPropertyChanged;
        DeepVlm.PropertyChanged += OnSlotPropertyChanged;
        Accelerator.PropertyChanged += OnSlotPropertyChanged;

        SeedFromSentinels();
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;
        UpdateAcceleratorForVendor(EngineClient.Instance.Info?.Hardware?.GpuVendor);
    }

    private void UpdateAcceleratorForVendor(string? gpuVendor)
    {
        var vendor = (gpuVendor ?? string.Empty).ToLowerInvariant();
        Accelerator.DisplayLabel = string.IsNullOrEmpty(vendor)
            ? "Bundled GPU acceleration"
            : $"Bundled acceleration ({vendor})";
        Accelerator.Message = "Scan models run on the CPU with tract, the same on every platform. Describe Files uses Vulkan, or CUDA if you install it.";
        Accelerator.Status = ModelInstallStatus.Installed;
        Accelerator.Fraction = 1.0;
    }

    public void Reset()
    {
        EngineClient.Instance.PropertyChanged -= OnEngineClientChanged;
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;

        FailIfDownloading(Clip, "Engine restarted — please retry.");
        FailIfDownloading(Arcface, "Engine restarted — please retry.");
        FailIfDownloading(RamPlus, "Engine restarted — please retry.");
        FailIfDownloading(FaceMesh, "Engine restarted — please retry.");
        FailIfDownloading(DeepVlm, "Engine restarted — please retry.");
        FailIfDownloading(Accelerator, "Engine restarted — please retry.");

        SeedFromSentinels();
    }

    private static void FailIfDownloading(ModelSlot slot, string reason)
    {
        if (slot.Status == ModelInstallStatus.Downloading)
        {
            DebugLog.Info($"[INSTALL] Reset(): flipping {slot.DisplayLabel} from Downloading to Failed ({reason})");
            slot.Fail(reason);
        }
    }

    /// <summary>Engine handles the three downloads concurrently via tokio::spawn (main.rs:266-278).</summary>
    public async Task InstallAllAsync()
    {
        // Captured here, on the UI thread, before any ConfigureAwait(false).
        _uiDispatcher ??= Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        if (Interlocked.CompareExchange(ref _installAllInFlight, 1, 0) != 0)
        {
            DebugLog.Info("[INSTALL] InstallAllAsync already in flight; ignoring duplicate request");
            return;
        }
        try
        {
            // The three TryInstallAsync calls race for EngineClient._writeLock when their IPC commands serialize; whichever loses both races looked frozen to the user until its engine "Queued" event finally landed.
            var now = DateTime.UtcNow;
            var slotsToInstall = new List<ModelSlot> { Clip, Arcface, RamPlus, DeepVlm };
            if (IncludeAcceleratorInInstallAll())
            {
                slotsToInstall.Add(Accelerator);
            }
            // Never reset its shared slot or dispatch the welcome recommendation in parallel: progress/cancel tracking holds only one model kind.
            slotsToInstall.RemoveAll(slot => slot.Status is ModelInstallStatus.Installed or ModelInstallStatus.Downloading);
            foreach (var slot in slotsToInstall)
            {
                slot.ResetForRetry();
                slot.Status = ModelInstallStatus.Downloading;
                slot.Message = "Queued — starting download…";
                slot.LastProgressAt = now;
                slot.CurrentModelKind = PrimaryKindFor(slot);
            }

            var tasks = new List<Task>(slotsToInstall.Count);
            foreach (var slot in slotsToInstall)
            {
                tasks.Add(TryInstallAsync(slot));
            }
            await Task.WhenAll(tasks).ConfigureAwait(false);
        }
        finally
        {
            Interlocked.Exchange(ref _installAllInFlight, 0);
        }
    }

    private static async Task TryInstallAsync(ModelSlot slot)
    {
        if (slot.Status == ModelInstallStatus.Installed) return;
        try
        {
            await slot.InstallAsync().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[INSTALL] {slot.DisplayLabel} install threw inside InstallAllAsync: {ex.Message}");
            slot.Fail(ex.Message);
        }
    }

    /// <summary>Yes only when: - vendor is NVIDIA (so the row actually represents a real installable pack rather than a non-NVIDIA pseudo-Installed placeholder), AND - the slot is NotInstalled or Failed (not already installed and not already mid-download).</summary>
    private bool IncludeAcceleratorInInstallAll()
    {
        var vendor = (EngineClient.Instance.Info?.Hardware?.GpuVendor ?? string.Empty).ToLowerInvariant();
        if (vendor != "nvidia") return false;
        return Accelerator.Status is ModelInstallStatus.NotInstalled
                                 or ModelInstallStatus.Failed;
    }

    public Task CancelAllAsync() => EngineClient.Instance.CancelPrewarmAsync();

    /// <summary>The explicit Install/Retry click is the ONLY place a kind's cancel mark is erased.</summary>
    private static void ClearCancelMarks(params string[] kinds)
    {
        foreach (var k in kinds)
        {
            EngineClient.Instance.ClearPrewarmCancelMark(k);
        }
    }

    private static readonly string[][] SlotKindGroups =
    {
        new[] { "mobileclip_s2", "clip_text" },        // CLIP slot
        new[] { "cudnn_runtime_x64", "ort_cuda_x64" }, // Accelerator (GPU pack) slot
    };

    /// <summary>InstallAllAsync's pre-stamp uses this so a Downloading row is never kind-less; for a multi-kind slot CancelModelAsync expands it back to the full SlotKindGroups group, so the first kind cancels the whole slot.</summary>
    private string? PrimaryKindFor(ModelSlot slot)
    {
        if (ReferenceEquals(slot, Clip)) return "mobileclip_s2";
        if (ReferenceEquals(slot, Arcface)) return "arcface_default";
        if (ReferenceEquals(slot, RamPlus)) return "ram_plus";
        if (ReferenceEquals(slot, FaceMesh)) return "face_mesh";
        if (ReferenceEquals(slot, DeepVlm)) return _deepVlmModelKind;
        if (ReferenceEquals(slot, Accelerator)) return "cudnn_runtime_x64";
        return null;
    }

    /// <summary>Cancel a single slot's in-flight download(s) (the per-row Cancel button), so cancelling one row no longer aborts every other concurrent install.</summary>
    public Task CancelModelAsync(string? modelKind)
    {
        if (modelKind is null)
        {
            DebugLog.Warn("[INSTALL] CancelModelAsync(null) — slot has no CurrentModelKind; ignoring rather than cancelling every download.");
            return Task.CompletedTask;
        }
        var group = SlotKindGroups.FirstOrDefault(g => g.Contains(modelKind));
        if (group is null) return EngineClient.Instance.CancelPrewarmAsync(modelKind);
        return Task.WhenAll(group.Select(k => EngineClient.Instance.CancelPrewarmAsync(k)));
    }

    /// <summary>Optional Qwen3 choices never become automatic recommendations.</summary>
    public void UpdateDeepVlmRecommendation(double ramGB, ulong vramMB, string? gpuVendor)
    {
        if (DeepVlm.Status == ModelInstallStatus.Downloading
            || DeepVlm.Status == ModelInstallStatus.Installed)
        {
            return;
        }
        bool wants7b = ramGB >= 16.0 || vramMB >= 8000;
        string kind = wants7b ? "qwen2_5_vl_7b" : "gemma_3_4b";
        if (_deepVlmModelKind == kind) return;
        _deepVlmModelKind = kind;
        if (wants7b)
        {
            DeepVlm.DisplayLabel = "Qwen2.5-VL 7B";
            DeepVlm.ApproxBytes = 6_100_000_000UL;
        }
        else
        {
            DeepVlm.DisplayLabel = "Gemma 3 4B";
            DeepVlm.ApproxBytes = 3_351_000_000UL;
        }
        DebugLog.Info($"[INSTALL] Deep Analyze recommendation: {DeepVlm.DisplayLabel} (RAM={ramGB:F1} GB, VRAM={vramMB} MB, GPU={gpuVendor ?? "?"})");
    }

    /// <summary>Persist the Deep Analyze model the user just chose to install so the Deep Analyze tab + the manual auto-chain pass use the same weights.</summary>
    private static void PersistSelectedVlmModelKind(string kind)
    {
        try
        {
            // Route through the shared singleton, NOT a fresh AppSettings.Load(): a fresh instance shares the static debounce CTS, so its Save() cancels the singleton's pending write and persists a snapshot loaded from disk that lacks the singleton's in-memory changes (lost update).
            var s = FileID.ViewModels.AppViewModel.Instance.Settings;
            if (s.SelectedVlmModelKind == kind) return;
            s.SelectedVlmModelKind = kind;
            s.Save();
            DebugLog.Info($"[INSTALL] persisted SelectedVlmModelKind={kind} (welcome Deep Analyze pick)");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[INSTALL] PersistSelectedVlmModelKind threw: " + ex.Message);
        }
    }

    private bool _allInstalled;
    public bool AllInstalled
    {
        get => _allInstalled;
        private set => Set(ref _allInstalled, value);
    }

    private bool _coreModelsInstalled;
    /// <summary>Core sub-1 GB models (CLIP + RAM++ + ArcFace) — gates the Welcome sheet RE-SHOW decision only.</summary>
    public bool CoreModelsInstalled
    {
        get => _coreModelsInstalled;
        private set => Set(ref _coreModelsInstalled, value);
    }

    private bool _isBusy;
    public bool IsBusy
    {
        get => _isBusy;
        private set => Set(ref _isBusy, value);
    }

    private void RecomputeAggregates()
    {
        CoreModelsInstalled =
            Clip.Status == ModelInstallStatus.Installed
            && Arcface.Status == ModelInstallStatus.Installed
            && RamPlus.Status == ModelInstallStatus.Installed;
        AllInstalled =
            CoreModelsInstalled
            && DeepVlm.Status == ModelInstallStatus.Installed;
        IsBusy =
            Clip.Status == ModelInstallStatus.Downloading
            || Arcface.Status == ModelInstallStatus.Downloading
            || RamPlus.Status == ModelInstallStatus.Downloading
            || DeepVlm.Status == ModelInstallStatus.Downloading;
    }

    private void OnSlotPropertyChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnSlotPropertyChanged), () => OnSlotPropertyChangedCore(sender, e));

    private void OnSlotPropertyChangedCore(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(ModelSlot.Status)) return;
        RecomputeAggregates();
    }

    /// <summary>Only sets Installed for slots that already have a `.fileid-installed` marker; never overrides Downloading or Failed (downloads in flight are owned by the engine event stream).</summary>
    public void SeedFromSentinels()
    {
        SeedSlot(Clip, ClipSentinelIds, requireAll: true);
        SeedSlot(Arcface, ArcfaceSentinelIds);
        SeedSlot(RamPlus, RamPlusSentinelIds);
        SeedSlot(FaceMesh, FaceMeshSentinelIds);
        SeedSlot(DeepVlm, DeepVlmSentinelIds);
        RecomputeAggregates();
    }

    /// <summary>Alias for callers that just want a sentinel re-check without caring about the engine-event side of the state machine (MainWindow startup, DeepAnalyzeView model-install panel).</summary>
    public void Refresh() => SeedFromSentinels();

    private static void SeedSlot(ModelSlot slot, string[] candidateIds, bool requireAll = false)
    {
        if (slot.Status == ModelInstallStatus.Downloading
            || slot.Status == ModelInstallStatus.Failed)
        {
            return;
        }
        if (requireAll)
        {
            foreach (var id in candidateIds)
            {
                if (!SentinelInstalled(id))
                {
                    slot.Status = ModelInstallStatus.NotInstalled;
                    return;
                }
            }
            slot.Status = ModelInstallStatus.Installed;
            return;
        }
        foreach (var id in candidateIds)
        {
            if (SentinelInstalled(id))
            {
                slot.Status = ModelInstallStatus.Installed;
                return;
            }
        }
        slot.Status = ModelInstallStatus.NotInstalled;
    }

    /// <summary>Gates on engine readiness BEFORE flipping Status to Downloading, so a click that loses the startup race surfaces a clean "Engine not ready" error rather than a Downloading flicker followed by a swallowed exception.</summary>
    private async Task PrewarmAsync(string modelKind)
    {
        // Captured on the UI thread before the first ConfigureAwait(false).
        _uiDispatcher ??= Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        var slot = SlotFor(modelKind);
        if (slot is null)
        {
            DebugLog.Warn($"[INSTALL] PrewarmAsync('{modelKind}') — no slot routes for this id");
            return;
        }
        DebugLog.Info($"[INSTALL] PrewarmAsync('{modelKind}') called. priorStatus={slot.Status}");

        if (!await ModelLicenseGate.EnsureAcceptedAsync([modelKind]).ConfigureAwait(true))
        {
            DebugLog.Info($"[INSTALL] {modelKind} install cancelled because its license terms were declined.");
            slot.ResetForRetry();
            return;
        }

        // If the engine never reaches Ready, the slot stays in its prior state and the user sees a clear error message instead of a misleading spinner.
        try
        {
            await EngineClient.Instance.WaitForReadyAsync(WaitForReadyTimeout).ConfigureAwait(false);
        }
        catch (TimeoutException ex)
        {
            // A timeout means the engine is still spinning up (cold-start model load on a slow disk / ARM box), NOT that it crashed — tell the user to wait a moment and retry rather than implying a failure.
            DebugLog.Warn($"[INSTALL] WaitForReadyAsync timed out for '{modelKind}': {ex.Message}");
            slot.Fail("FileID is still getting ready. Try again in a moment.");
            return;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[INSTALL] WaitForReadyAsync threw for '{modelKind}': {ex.Message}");
            slot.Fail("FileID is still getting ready. Try again in a moment.");
            return;
        }

        // A per-row Cancel during the wait above only sets the app-side mark — its CancelPrewarm IPC faults because the engine isn't Ready, so the engine never hears about it.
        if (EngineClient.Instance.IsPrewarmCancelled(modelKind))
        {
            DebugLog.Info($"[INSTALL] {modelKind} was cancelled while waiting for engine Ready; skipping dispatch.");
            slot.ResetForRetry();
            return;
        }

        // only reset state if this slot wasn't already pre-stamped to Downloading by InstallAllAsync.
        if (slot.Status != ModelInstallStatus.Downloading)
        {
            slot.ResetForRetry();
            slot.Status = ModelInstallStatus.Downloading;
            slot.Message = "Starting…";
            slot.LastProgressAt = DateTime.UtcNow;
        }
        slot.CurrentModelKind = modelKind;
        DebugLog.Info($"[INSTALL] {modelKind} status set to Downloading; sending IPC...");

        try
        {
            await EngineClient.Instance.PrewarmModelAsync(modelKind, clearCancelMark: false).ConfigureAwait(false);
            DebugLog.Info($"[INSTALL] {modelKind} prewarmModel IPC sent; awaiting progress events.");
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[INSTALL] PrewarmModelAsync('{modelKind}') threw: {ex.Message}");
            slot.Fail(ex.Message);
            return;
        }

        ScheduleNoProgressWatchdog(slot, modelKind);
    }

    private void ScheduleNoProgressWatchdog(ModelSlot slot, string modelKind, CancellationToken ct = default)
    {
        var sentAt = DateTime.UtcNow;
        var ui = _uiDispatcher ?? _ui;
        _ = Task.Run(async () =>
        {
            try
            {
                await Task.Delay(NoProgressTimeout, ct).ConfigureAwait(false);
                // cancellation check after the delay — if the user cancelled the install during the watchdog window, don't surface a "no response" error on top of a clean cancellation flow.
                if (ct.IsCancellationRequested) return;
                // Read-only check off-thread is fine (status/timestamp are primitives + DateTime; no torn-read risk on x64/ARM64).
                if (slot.Status != ModelInstallStatus.Downloading) return;
                if (slot.LastProgressAt > sentAt || _lastAnyProgressAt > sentAt) return;
                DebugLog.Warn($"[INSTALL] {modelKind} no-progress watchdog firing (no events in {NoProgressTimeout.TotalSeconds:0}s)");
                if (ui is not null)
                {
                    ui.TryEnqueue(() => slot.Fail("No response from engine — try again."));
                }
                else
                {
                    // Refuse to fail the slot when we can't marshal; log and let the engine's own error event (if any) drive the eventual transition.
                    DebugLog.Warn($"[INSTALL] {modelKind} watchdog: no UI dispatcher; skipping slot.Fail to avoid off-thread PropertyChanged.");
                }
            }
            catch (OperationCanceledException)
            {
            }
            catch (Exception ex)
            {
                DebugLog.Warn($"[INSTALL] no-progress watchdog threw: {ex.Message}");
            }
        }, ct);
    }

    private ModelSlot? SlotFor(string? modelKind)
    {
        switch (modelKind)
        {
            case "mobileclip_s2":
            case "clip_image":
            case "clip_text":
                return Clip;
            case "arcface_default":
            case "arcface_iresnet50":
            case "arcface_mobileface":
                return Arcface;
            case "qwen2_5_vl_7b":
            case "qwen3_vl_4b":
            case "qwen3_vl_8b":
            case "gemma_3_4b":
            case "mistral_small_3_2":
            case "mistral-small-3.2":
                return DeepVlm;
            case "ram_plus":
            case "ram-plus":
                return RamPlus;
            case "face_mesh":
                return FaceMesh;
            case "ort_cuda_x64":
            case "cudnn_runtime_x64":
            case "ort_openvino_x64":
                return Accelerator;
            default:
                return null;
        }
    }

    private static bool IsAutoInstallerOnly(string? modelKind)
    {
        return modelKind is "llama_runtime_x64"
            or "llama_runtime_cuda_x64"
            or "llama_runtime_vulkan_x64";
    }

    private ModelSlot? SlotForErrorPath(string? path)
    {
        if (string.IsNullOrEmpty(path)) return null;
        if (path.Contains("MobileCLIP", StringComparison.OrdinalIgnoreCase)) return Clip;
        if (path.Contains("arcface", StringComparison.OrdinalIgnoreCase)) return Arcface;
        if (path.Contains("Qwen", StringComparison.OrdinalIgnoreCase)
            || path.Contains("Gemma", StringComparison.OrdinalIgnoreCase)
            || path.Contains("Mistral", StringComparison.OrdinalIgnoreCase))
        {
            return DeepVlm;
        }
        if (path.Contains("ram_plus", StringComparison.OrdinalIgnoreCase)) return RamPlus;
        if (path.Contains("face_mesh", StringComparison.OrdinalIgnoreCase)) return FaceMesh;
        return null;
    }

    private int _progressEventCount;

    /// <summary>The no-progress watchdog (static) reads this so an active download on one slot keeps every slot's watchdog from false-failing under multi-download contention.</summary>
    private static DateTime _lastAnyProgressAt = DateTime.MinValue;

    private void OnEngineClientChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("ModelInstallerService.OnEngineClientChanged", () =>
        {
            if (e.PropertyName == nameof(EngineClient.ModelDownloadProgress))
            {
                HandleProgress(EngineClient.Instance.ModelDownloadProgress);
                return;
            }
            if (e.PropertyName == nameof(EngineClient.LastError))
            {
                DebugLog.Info($"[ENGINE-SUB:ModelInstallerService] {e.PropertyName}");
                HandleEngineError(EngineClient.Instance.LastError);
                return;
            }
            if (e.PropertyName == nameof(EngineClient.Info))
            {
                DebugLog.Info($"[ENGINE-SUB:ModelInstallerService] {e.PropertyName}");
                var info = EngineClient.Instance.Info;
                if (info is not null)
                {
                    UpdateDeepVlmRecommendation(
                        info.PhysicalMemoryGB,
                        info.Hardware?.VramMb ?? 0,
                        info.Hardware?.GpuVendor);
                    UpdateAcceleratorForVendor(info.Hardware?.GpuVendor);
                }
                return;
            }
        });

    private void HandleProgress(ModelDownloadProgress? p)
    {
        if (p is null) return;
        _lastAnyProgressAt = DateTime.UtcNow;
        var n = Interlocked.Increment(ref _progressEventCount);
        if (n <= 5 || n % 50 == 0 || p.Fraction >= 0.999)
        {
            DebugLog.Info($"[INSTALL] OnEngineClientChanged #{n}: {p.ModelKind} {p.Fraction:P0} bytes={p.BytesDone}/{p.TotalBytes}");
        }
        var slot = SlotFor(p.ModelKind);
        if (slot is null)
        {
            if (IsAutoInstallerOnly(p.ModelKind))
            {
                DebugLog.Debug($"[INSTALL] runtime-pack progress (no welcome-sheet slot): {p.ModelKind} {p.Fraction:P0}");
            }
            else
            {
                DebugLog.Warn($"[INSTALL] no slot for model_kind '{p.ModelKind}' — progress event dropped.");
            }
            return;
        }
        var sentinelIds = SentinelIdsFor(slot);
        slot.Apply(p, () => ReferenceEquals(slot, DeepVlm)
            ? SentinelInstalled(p.ModelKind)
            : SentinelExistsForAnyOf(sentinelIds));
    }

    private void HandleEngineError(EngineError? error)
    {
        if (error is null) return;
        var kind = error.Kind ?? string.Empty;
        var isInstallError =
            kind == "model_download_failed"
            || kind == "model_download_disk_full"
            || kind == "download_tls_pin_failed"
            || kind == "zip_extract_failed"
            || kind == "pack_not_available"
            // A stale engine that doesn't know a model_kind, or one that can't resolve its models dir, stamps the originating model id on the error.
            || kind == "unknown_model"
            || kind == "models_dir_unavailable"
            // sentinel_dir_create_failed / sentinel_write_failed / sentinel_rename_failed: the bytes all landed but the engine couldn't register the install marker, and it emits NO terminal progress event after these — without routing them the row spins at ~100% "Downloading" forever with no Retry.
            || kind.StartsWith("sentinel_", StringComparison.OrdinalIgnoreCase)
            || kind.StartsWith("prewarm_", StringComparison.OrdinalIgnoreCase);
        if (!isInstallError) return;

        // prewarm_cancelled is user-initiated: not a failure (no red Retry toast), but the slot must still LEAVE Downloading or it sticks at "Cancelling…" forever — wedging IsBusy/AllInstalled (and the Install-all gate).
        if (kind == "prewarm_cancelled")
        {
            if (!string.IsNullOrEmpty(error.ModelKind))
            {
                SlotFor(error.ModelKind)?.ResetForRetry();
            }
            return;
        }

        var slot = !string.IsNullOrEmpty(error.ModelKind)
            ? SlotFor(error.ModelKind)
            : SlotForErrorPath(error.Path);
        if (slot is null)
        {
            DebugLog.Warn($"[INSTALL] engine error '{kind}' has no routable slot (modelKind={error.ModelKind ?? "<null>"}, path={PathRedactor.Redact(error.Path)})");
            return;
        }
        if (string.Equals(error.ModelKind, "ort_openvino_x64", StringComparison.OrdinalIgnoreCase))
        {
            DebugLog.Info($"[INSTALL] OpenVINO pack unavailable ({kind}); staying on DirectML. {error.Message}");
            if (Accelerator.Status == ModelInstallStatus.Downloading)
            {
                UpdateAcceleratorForVendor("intel");
            }
            return;
        }
        DebugLog.Info($"[INSTALL] engine error → {slot.DisplayLabel}.Fail(): {error.Message}");
        slot.Fail(EngineErrorText.Plain(error.Message, "The download didn't finish. Check your internet connection and try again."));
    }

    private static string[] SentinelIdsFor(ModelSlot slot)
    {
        if (ReferenceEquals(slot, Instance.Clip)) return ClipSentinelIds;
        if (ReferenceEquals(slot, Instance.Arcface)) return ArcfaceSentinelIds;
        if (ReferenceEquals(slot, Instance.RamPlus)) return RamPlusSentinelIds;
        if (ReferenceEquals(slot, Instance.FaceMesh)) return FaceMeshSentinelIds;
        if (ReferenceEquals(slot, Instance.DeepVlm)) return DeepVlmSentinelIds;
        if (ReferenceEquals(slot, Instance.Accelerator)) return AcceleratorSentinelIds;
        return Array.Empty<string>();
    }

    private static bool SentinelExistsForAnyOf(string[] candidateIds)
    {
        foreach (var id in candidateIds)
        {
            if (SentinelInstalled(id)) return true;
        }
        return false;
    }

    /// <summary>Only report a model installed after its sentinel and required artifacts are present.</summary>
    private static bool SentinelInstalled(string modelId) => SentinelProbe.Installed(modelId);

    public event PropertyChangedEventHandler? PropertyChanged;

    private void Set<T>(ref T field, T value, [CallerMemberName] string? propertyName = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value)) return;
        field = value;
        // x:Bind reads these aggregates on the UI thread, so PropertyChanged must fire there.
        var handler = PropertyChanged;
        if (handler is null) return;
        var args = new PropertyChangedEventArgs(propertyName);
        if (_ui is null || _ui.HasThreadAccess)
        {
            handler(this, args);
        }
        else
        {
            _ui.TryEnqueue(() => handler(this, args));
        }
    }
}
