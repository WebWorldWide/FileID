// ModelInstallerService — per-model install state for the Welcome sheet.
//
// 1:1 port of the state shape used by macOS WelcomeSheet.swift +
// CLIPModelInstaller.swift + ArcFaceModelInstaller.swift. Each model
// tracks: status (NotInstalled / Downloading / Installed / Failed),
// fraction, bytes done / total, an EMA bytes-per-second, ETA seconds.
//
// Engine progress events are authoritative when a download is in flight.
// Sentinel files (`.fileid-installed`) are consulted at startup to seed
// Installed state for previously-completed models AND verified at the
// 100% transition so a buggy engine path can't lie to the user.
//
// PRIVACY: never makes a network call. Only sends IPC commands; the
// engine is the sole network surface.

using System.ComponentModel;
using System.IO;
using System.Runtime.CompilerServices;
using System.Threading;
using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Dispatching;

namespace FileID.Services;

internal sealed class ModelInstallerService : INotifyPropertyChanged
{
    // Sentinel model-id constants. The engine writes one sentinel file
    // per installed model bundle at `%LOCALAPPDATA%\FileID\Models\.sentinels\
    // {model.id}.installed` (atomic temp+rename; see engine main.rs
    // handle_prewarm_model). The id strings here MUST match `Model.id`
    // in engine/src/models/registry.rs.
    //
    // Static field init runs in source order, so these MUST be declared
    // before Instance — its ctor calls SeedFromSentinels which reads them.
    //
    // CLIP needs BOTH the image encoder (mobileclip_s2) and the text encoder
    // (clip_text) — they're separate model_kinds in the engine's registry
    // because they download from different paths in the Xenova mobileclip_s2
    // HuggingFace repo. The pre-scan validation in main.rs::handle_start_scan
    // requires both sentinels, so the slot's "Installed" state must reflect
    // that. The DeepVlm slot is the optional Deep Analyze model — hardware-
    // tiered Qwen / Gemma; any of 3B / 7B / Gemma satisfies the slot. ArcFace
    // stays a single-sentinel "any-of".
    private static readonly string[] ClipSentinelIds = { "mobileclip_s2", "clip_text" };
    private static readonly string[] ArcfaceSentinelIds = { "arcface" };
    private static readonly string[] DeepVlmSentinelIds = { "qwen2_5_vl_7b", "gemma_3_4b", "mistral_small_3_2" };
    // RAM++ — the in-scan multi-label tagger. Single-sentinel "any-of".
    private static readonly string[] RamPlusSentinelIds = { "ram_plus" };
    // one-button GPU acceleration pack on the welcome sheet.
    // The engine's `cudnn_runtime_x64` registry arm covers NVIDIA. Other
    // vendors stay no-op (DirectML is bundled with ORT and is the
    // production path on AMD/Intel/Qualcomm).
    private static readonly string[] AcceleratorSentinelIds = { "cudnn_runtime_x64" };

    /// <summary>Time the engine has to reach Ready before an Install
    /// click gives up and surfaces "Engine not ready" to the user.</summary>
    private static readonly TimeSpan WaitForReadyTimeout = TimeSpan.FromSeconds(30);

    /// <summary>Time after which a Downloading slot with no progress
    /// events gets flipped to Failed. Mirrors macOS WelcomeSheet's
    /// "stuck install" guard. B2: raised 30 → 60 s because under
    /// multi-download contention (welcome "Install all" + the background
    /// auto-installers) one model's bytes can legitimately stall &gt;30 s
    /// while another saturates the link — and the watchdog now also
    /// consults <see cref="_lastAnyProgressAt"/> so any active download
    /// keeps every slot's watchdog alive.</summary>
    private static readonly TimeSpan NoProgressTimeout = TimeSpan.FromSeconds(60);

    public static ModelInstallerService Instance { get; } = new();

    public ModelSlot Clip { get; }
    public ModelSlot Arcface { get; }
    /// <summary>RAM++ — the primary in-scan image tagger (4585-tag multi-label
    /// ONNX). Optional; when absent the engine falls back to CLIP scene tags,
    /// so it is NOT (yet) a gate on <see cref="AllInstalled"/>.</summary>
    public ModelSlot RamPlus { get; }
    /// <summary>Deep Analyze model — hardware-tiered Qwen2.5-VL 7B / Gemma 3 4B
    /// / Mistral-Small 3.2. Installing persists AppSettings.SelectedVlmModelKind
    /// so the Deep Analyze tab picks the freshly-installed model by default.</summary>
    public ModelSlot DeepVlm { get; }
    /// <summary> one-button GPU acceleration pack. On NVIDIA the
    /// Install action downloads cuDNN; on AMD/Intel/Qualcomm/CPU the slot
    /// is pre-marked Installed with an explanatory Message (DirectML is
    /// already the optimal path). The welcome sheet renders the row
    /// adaptive to the detected vendor (set in UpdateAcceleratorForVendor).</summary>
    public ModelSlot Accelerator { get; }

    /// <summary> true only if a real cuDNN sentinel exists on
    /// disk. Distinguishes "user installed cuDNN" from "non-NVIDIA, slot
    /// set to Installed because DirectML is already optimal". Drives the
    /// welcome sheet's badge + button visibility (no "Installed" badge
    /// for non-NVIDIA — they didn't install anything).</summary>
    public bool AcceleratorIsRealInstall
    {
        get => _acceleratorIsRealInstall;
        private set => Set(ref _acceleratorIsRealInstall, value);
    }
    private bool _acceleratorIsRealInstall;

    private int _installAllInFlight; // 0 = idle, 1 = in flight

    /// <summary>Deep Analyze model the DeepVlm welcome row installs. Tiered to
    /// the machine by UpdateDeepVlmRecommendation (Gemma 3 4B on weak boxes vs
    /// Qwen2.5-VL 7B on capable ones). Read at click time by the slot's
    /// installAction; mirrors the Deep Analyze tab default
    /// (AppSettings.SelectedVlmModelKind).</summary>
    private string _deepVlmModelKind = "qwen2_5_vl_7b";

    private ModelInstallerService()
    {
        Clip = new ModelSlot(
            displayLabel: "MobileCLIP-S2",
            approxBytes: 220UL * 1024 * 1024,
            // Install both halves of CLIP: the image encoder (mobileclip_s2)
            // and the text encoder (clip_text). The engine's pre-scan check
            // requires both sentinels. Sequential so per-row progress UI
            // stays sane; the second prewarm short-circuits at the engine if
            // its files + sentinel are already on disk.
            installAction: async () =>
            {
                await PrewarmAsync("mobileclip_s2").ConfigureAwait(false);
                await PrewarmAsync("clip_text").ConfigureAwait(false);
            });
        Arcface = new ModelSlot(
            displayLabel: "ArcFace MobileFace",
            approxBytes: 14UL * 1024 * 1024,
            installAction: () => PrewarmAsync("arcface_default"));
        RamPlus = new ModelSlot(
            displayLabel: "RAM++ image tagger",
            // ~882 MB fp16 ONNX (bakes the frozen tag-description embeddings in).
            approxBytes: 925_600_000UL,
            installAction: () => PrewarmAsync("ram_plus"));
        DeepVlm = new ModelSlot(
            displayLabel: "Qwen2.5-VL 7B",
            approxBytes: 6_100_000_000UL,
            installAction: async () =>
            {
                // Persist the hardware-recommended Deep Analyze model so the
                // Deep Analyze tab + manual auto-chain use what the user just
                // downloaded.
                PersistSelectedVlmModelKind(_deepVlmModelKind);
                await PrewarmAsync(_deepVlmModelKind).ConfigureAwait(false);
            });
        // GPU Acceleration Pack. Display label + Message are
        // adaptive — UpdateAcceleratorForVendor() refreshes them as soon
        // as the engine reports detected hardware. Until then, the row
        // shows "Detecting GPU…" so the user knows it's waiting.
        Accelerator = new ModelSlot(
            displayLabel: "GPU Acceleration Pack",
            approxBytes: 430UL * 1024 * 1024,
            installAction: () => PrewarmAsync("cudnn_runtime_x64"));
        Accelerator.Message = "Detecting GPU…";

        Clip.PropertyChanged += OnSlotPropertyChanged;
        Arcface.PropertyChanged += OnSlotPropertyChanged;
        RamPlus.PropertyChanged += OnSlotPropertyChanged;
        DeepVlm.PropertyChanged += OnSlotPropertyChanged;
        Accelerator.PropertyChanged += OnSlotPropertyChanged;

        SeedFromSentinels();
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;
        // If engine has already published Info (raced our ctor), apply now.
        UpdateAcceleratorForVendor(EngineClient.Instance.Info?.Hardware?.GpuVendor);
    }

    /// <summary> adapt the Accelerator slot to the detected GPU
    /// vendor. NVIDIA → installable cuDNN pack. Anything else → already-
    /// optimal Status=Installed with an explanatory Message. Called on
    /// engine Info changes + at construction time.</summary>
    private void UpdateAcceleratorForVendor(string? gpuVendor)
    {
        // If user already installed cuDNN earlier, sentinel-seed already
        // flipped to Installed. Don't downgrade that.
        if (Accelerator.Status == ModelInstallStatus.Installed
            && SentinelExistsForAnyOf(AcceleratorSentinelIds))
        {
            Accelerator.Message = "cuDNN active — ~15% faster scanning enabled.";
            return;
        }
        var vendor = (gpuVendor ?? string.Empty).ToLowerInvariant();
        switch (vendor)
        {
            case "nvidia":
                Accelerator.DisplayLabel = "GPU Acceleration Pack (NVIDIA)";
                Accelerator.Message = "Unlocks ~15% faster scanning on NVIDIA GPUs (~430 MB).";
                if (Accelerator.Status != ModelInstallStatus.Downloading
                    && Accelerator.Status != ModelInstallStatus.Installed)
                {
                    Accelerator.Status = ModelInstallStatus.NotInstalled;
                }
                break;
            case "amd":
                Accelerator.DisplayLabel = "GPU Acceleration (AMD)";
                Accelerator.Message = "DirectML is already optimal for your AMD GPU — no install needed.";
                Accelerator.Status = ModelInstallStatus.Installed;
                Accelerator.Fraction = 1.0;
                break;
            case "intel":
                Accelerator.DisplayLabel = "GPU Acceleration (Intel)";
                Accelerator.Message = "DirectML is already optimal for your Intel GPU — no install needed.";
                Accelerator.Status = ModelInstallStatus.Installed;
                Accelerator.Fraction = 1.0;
                break;
            case "qualcomm":
                Accelerator.DisplayLabel = "GPU Acceleration (Snapdragon)";
                Accelerator.Message = "DirectML + QNN already optimal for your Snapdragon GPU.";
                Accelerator.Status = ModelInstallStatus.Installed;
                Accelerator.Fraction = 1.0;
                break;
            case "none":
                Accelerator.DisplayLabel = "GPU Acceleration";
                Accelerator.Message = "No GPU detected — scanning will run on CPU.";
                Accelerator.Status = ModelInstallStatus.Installed;
                Accelerator.Fraction = 1.0;
                break;
            case "":
                Accelerator.DisplayLabel = "GPU Acceleration Pack";
                Accelerator.Message = "Detecting GPU…";
                break;
            default:
                Accelerator.DisplayLabel = "GPU Acceleration";
                Accelerator.Message = "DirectML is the production path on your GPU.";
                Accelerator.Status = ModelInstallStatus.Installed;
                Accelerator.Fraction = 1.0;
                break;
        }
    }

    /// <summary>
    /// Re-attach the EngineClient.PropertyChanged handler. Called by
    /// EngineClient at the start of each spawn so a stale subscription
    /// against an orphaned EngineClient doesn't keep firing — and so a
    /// download that was in flight when the engine crashed is correctly
    /// flipped to Failed (otherwise the row would spin forever).
    /// </summary>
    public void Reset()
    {
        EngineClient.Instance.PropertyChanged -= OnEngineClientChanged;
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;

        // Any in-flight download owned by the now-dead engine is
        // unreachable — flip to Failed so the user sees a Retry button
        // instead of a permanent spinner.
        FailIfDownloading(Clip, "Engine restarted — please retry.");
        FailIfDownloading(Arcface, "Engine restarted — please retry.");
        FailIfDownloading(DeepVlm, "Engine restarted — please retry.");

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

    /// <summary>
    /// Install every not-yet-installed model in parallel — matches macOS
    /// WelcomeSheet.swift:146-151 which fires all three install actions
    /// from a single Install-all click. Engine handles the three downloads
    /// concurrently via tokio::spawn (main.rs:266-278). Per-slot try/catch
    /// keeps each error scoped so a CLIP failure can't abort ArcFace + VLM.
    /// Double-click is a no-op (Interlocked gate).
    /// </summary>
    public async Task InstallAllAsync()
    {
        if (Interlocked.CompareExchange(ref _installAllInFlight, 1, 0) != 0)
        {
            DebugLog.Info("[INSTALL] InstallAllAsync already in flight; ignoring duplicate request");
            return;
        }
        try
        {
            // pre-stamp every not-yet-installed slot to
            // Downloading + "Queued — starting download…" BEFORE awaiting.
            // The three TryInstallAsync calls race for EngineClient._writeLock
            // when their IPC commands serialize; whichever loses both races
            // looked frozen to the user until its engine "Queued" event finally
            // landed. Pre-stamping makes the UI flip identical for all three
            // rows the instant the user clicks Install all, regardless of
            // which IPC write wins. The engine's F1 Queued event then arrives
            // and overwrites with the same caption — no visible flicker.
            // LastProgressAt also resets so the no-progress watchdog (30 s)
            // doesn't false-fire while the slowest row waits for its IPC turn.
            //
            // include Accelerator (cuDNN) when it's a real install
            // candidate. Previously Install All omitted the Accelerator
            // entirely, so NVIDIA users who clicked Install All got the
            // three ML models but no cuDNN — the welcome sheet UX implied
            // "this button installs everything on the page" but it didn't.
            // The IncludeAcceleratorInInstallAll() helper returns true only
            // for NVIDIA + NotInstalled/Failed; non-NVIDIA slots stay
            // pseudo-Installed and are skipped naturally.
            var now = DateTime.UtcNow;
            // CLIP is included — it powers semantic search and emits scene tags.
            var slotsToInstall = new List<ModelSlot> { Clip, Arcface, RamPlus, DeepVlm };
            if (IncludeAcceleratorInInstallAll())
            {
                slotsToInstall.Add(Accelerator);
            }
            foreach (var slot in slotsToInstall)
            {
                if (slot.Status == ModelInstallStatus.Installed) continue;
                slot.ResetForRetry();
                slot.Status = ModelInstallStatus.Downloading;
                slot.Message = "Queued — starting download…";
                slot.LastProgressAt = now;
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

    /// <summary>decide whether Install All should attempt the
    /// Accelerator (cuDNN) pack. Yes only when:
    ///   - vendor is NVIDIA (so the row actually represents a real
    ///     installable pack rather than a non-NVIDIA pseudo-Installed
    ///     placeholder), AND
    ///   - the slot is NotInstalled or Failed (not already installed
    ///     and not already mid-download).
    /// All other cases short-circuit. Without this gate, an AMD/Intel/
    /// Snapdragon machine would re-enter the install pipeline on every
    /// Install All click even though their Accelerator row is
    /// pseudo-Installed (DirectML optimal).</summary>
    private bool IncludeAcceleratorInInstallAll()
    {
        var vendor = (EngineClient.Instance.Info?.Hardware?.GpuVendor ?? string.Empty).ToLowerInvariant();
        if (vendor != "nvidia") return false;
        return Accelerator.Status is ModelInstallStatus.NotInstalled
                                 or ModelInstallStatus.Failed;
    }

    public Task CancelAllAsync() => EngineClient.Instance.CancelPrewarmAsync();

    /// <summary>Deep Analyze model recommendation for the welcome-sheet DeepVlm
    /// row, tiered to the machine: a roomy box (≥16 GB RAM or a discrete GPU
    /// with ≥8 GB VRAM) gets Qwen 2.5-VL 7B for the best captions; everything
    /// else gets the 3B (the smallest Qwen — ~3.2 GB download, ~3.5 GB RAM).
    /// Does NOT persist the choice — that happens when the user actually
    /// installs the row (PersistSelectedVlmModelKind), so a model the user
    /// explicitly picked in the Deep Analyze tab is never stomped. No-op once
    /// the row is mid-flight or installed.</summary>
    public void UpdateDeepVlmRecommendation(double ramGB, ulong vramMB, string? gpuVendor)
    {
        if (DeepVlm.Status == ModelInstallStatus.Downloading
            || DeepVlm.Status == ModelInstallStatus.Installed)
        {
            return;
        }
        bool wants7b = ramGB >= 16.0 || vramMB >= 8000;
        // Capable boxes get Qwen2.5-VL-7B (Apache); weak boxes get the lighter
        // Gemma-3-4B. The non-commercial Qwen-3B was removed.
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

    /// <summary>Persist the Deep Analyze model the user just chose to install so
    /// the Deep Analyze tab + the manual auto-chain pass use the same weights.
    /// Only writes when the value actually changes (avoids needless disk I/O).</summary>
    private static void PersistSelectedVlmModelKind(string kind)
    {
        try
        {
            var s = AppSettings.Load();
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

    private bool _isBusy;
    public bool IsBusy
    {
        get => _isBusy;
        private set => Set(ref _isBusy, value);
    }

    private void RecomputeAggregates()
    {
        // RAM++ is the primary in-scan tagger and is now hosted on
        // Web-World-Wide/ram-plus-onnx (WS5 upload landed), so it gates
        // onboarding completion alongside CLIP/ArcFace/DeepVlm. (If RAM++ is
        // ever missing at runtime, tagging still degrades to CLIP scene-tags.)
        AllInstalled =
            Clip.Status == ModelInstallStatus.Installed
            && Arcface.Status == ModelInstallStatus.Installed
            && RamPlus.Status == ModelInstallStatus.Installed
            && DeepVlm.Status == ModelInstallStatus.Installed;
        IsBusy =
            Clip.Status == ModelInstallStatus.Downloading
            || Arcface.Status == ModelInstallStatus.Downloading
            || RamPlus.Status == ModelInstallStatus.Downloading
            || DeepVlm.Status == ModelInstallStatus.Downloading;
    }

    private void OnSlotPropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(ModelSlot.Status)) return;
        RecomputeAggregates();
    }

    /// <summary>
    /// Seed initial state from on-disk sentinels. Only sets Installed for
    /// slots that already have a `.fileid-installed` marker; never
    /// overrides Downloading or Failed (downloads in flight are owned by
    /// the engine event stream).
    /// </summary>
    public void SeedFromSentinels()
    {
        SeedSlot(Clip, ClipSentinelIds, requireAll: true);
        SeedSlot(Arcface, ArcfaceSentinelIds);
        SeedSlot(RamPlus, RamPlusSentinelIds);
        SeedSlot(DeepVlm, DeepVlmSentinelIds);
        // Accelerator slot — only flip to Installed if the
        // sentinel exists. Otherwise leave it as
        // UpdateAcceleratorForVendor decided (NotInstalled for NVIDIA,
        // Installed-with-message for non-NVIDIA / CPU).
        if (SentinelExistsForAnyOf(AcceleratorSentinelIds))
        {
            Accelerator.Status = ModelInstallStatus.Installed;
            Accelerator.Fraction = 1.0;
            Accelerator.Message = "cuDNN active — ~15% faster scanning enabled.";
            AcceleratorIsRealInstall = true;
        }
        RecomputeAggregates();
    }

    /// <summary>Alias for callers that just want a sentinel re-check
    /// without caring about the engine-event side of the state machine
    /// (MainWindow startup, DeepAnalyzeView model-install panel).</summary>
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

    /// <summary>
    /// Drive an install for a single model. Gates on engine readiness
    /// BEFORE flipping Status to Downloading, so a click that loses the
    /// startup race surfaces a clean "Engine not ready" error rather
    /// than a Downloading flicker followed by a swallowed exception.
    /// </summary>
    private async Task PrewarmAsync(string modelKind)
    {
        var slot = SlotFor(modelKind);
        if (slot is null)
        {
            DebugLog.Warn($"[INSTALL] PrewarmAsync('{modelKind}') — no slot routes for this id");
            return;
        }
        DebugLog.Info($"[INSTALL] PrewarmAsync('{modelKind}') called. priorStatus={slot.Status}");

        // Wait for engine Ready before touching slot.Status. If the engine
        // never reaches Ready, the slot stays in its prior state and the
        // user sees a clear error message instead of a misleading spinner.
        try
        {
            await EngineClient.Instance.WaitForReadyAsync(WaitForReadyTimeout).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[INSTALL] WaitForReadyAsync threw for '{modelKind}': {ex.Message}");
            slot.Fail("Engine not ready: " + ex.Message);
            return;
        }

        // only reset state if this slot wasn't already
        // pre-stamped to Downloading by InstallAllAsync. Re-running
        // ResetForRetry after the pre-stamp would blank Fraction/Message
        // mid-flight if the engine's first progress event happens to
        // arrive between pre-stamp and PrewarmAsync entry.
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
            await EngineClient.Instance.PrewarmModelAsync(modelKind).ConfigureAwait(false);
            DebugLog.Info($"[INSTALL] {modelKind} prewarmModel IPC sent; awaiting progress events.");
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[INSTALL] PrewarmModelAsync('{modelKind}') threw: {ex.Message}");
            slot.Fail(ex.Message);
            return;
        }

        // No-progress watchdog: 30 s after the IPC send, if Status is still
        // Downloading and no progress event has landed, fail with a clear
        // message. Mirrors macOS WelcomeSheet's "stuck install" handling.
        ScheduleNoProgressWatchdog(slot, modelKind);
    }

    private static void ScheduleNoProgressWatchdog(ModelSlot slot, string modelKind, CancellationToken ct = default)
    {
        var sentAt = DateTime.UtcNow;
        // Capture the UI dispatcher at schedule time. The watchdog runs on
        // a thread-pool thread (Task.Run) but slot.Fail mutates state that
        // x:Bind UI elements observe — those updates have to land on the
        // UI thread or downstream PropertyChanged handlers may touch
        // FrameworkElements off-thread.
        var ui = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        _ = Task.Run(async () =>
        {
            try
            {
                await Task.Delay(NoProgressTimeout, ct).ConfigureAwait(false);
                // cancellation check after the delay — if the
                // user cancelled the install during the watchdog window,
                // don't surface a "no response" error on top of a clean
                // cancellation flow.
                if (ct.IsCancellationRequested) return;
                // Read-only check off-thread is fine (status/timestamp are
                // primitives + DateTime; no torn-read risk on x64/ARM64).
                if (slot.Status != ModelInstallStatus.Downloading) return;
                // B2: this slot OR any other download progressed after we
                // scheduled → the engine is alive; don't false-fail.
                if (slot.LastProgressAt > sentAt || _lastAnyProgressAt > sentAt) return;
                DebugLog.Warn($"[INSTALL] {modelKind} no-progress watchdog firing (no events in {NoProgressTimeout.TotalSeconds:0}s)");
                if (ui is not null)
                {
                    ui.TryEnqueue(() => slot.Fail("No response from engine — try again."));
                }
                else
                {
                    // previously fell through to calling slot.Fail()
                    // directly on the thread-pool thread, which raises
                    // PropertyChanged off the UI thread → x:Bind UI hit
                    // off-thread → potential FrameworkElement violation.
                    // Refuse to fail the slot when we can't marshal; log
                    // and let the engine's own error event (if any) drive
                    // the eventual transition.
                    DebugLog.Warn($"[INSTALL] {modelKind} watchdog: no UI dispatcher; skipping slot.Fail to avoid off-thread PropertyChanged.");
                }
            }
            catch (OperationCanceledException)
            {
                // Cancellation is a normal terminating condition.
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
            case "gemma_3_4b":
            case "mistral_small_3_2":
            case "mistral-small-3.2":
                return DeepVlm;
            case "ram_plus":
            case "ram-plus":
                return RamPlus;
            // cuDNN routes to the welcome-sheet Accelerator slot.
            case "cudnn_runtime_x64":
                return Accelerator;
            default:
                return null;
        }
    }

    /// <summary>model_kinds the engine auto-installs at startup
    /// (LlamaRuntime + variants). These flow through ModelDownloadProgress
    /// events the welcome sheet doesn't have rows for; previously each one
    /// emitted a "no slot — progress event dropped" warn that flooded
    /// app.log. Demote them to a single debug line here. The auto-
    /// installer services (LlamaRuntimeAutoInstaller, CudaAutoInstaller)
    /// handle these progress events through their own paths.</summary>
    private static bool IsAutoInstallerOnly(string? modelKind)
    {
        return modelKind is "llama_runtime_x64"
            or "llama_runtime_cuda_x64"
            or "llama_runtime_vulkan_x64";
    }

    /// <summary>Fallback slot lookup by error path. Only used when the
    /// engine's error event carries no model_kind (legacy emitters, or
    /// non-model errors that still have a path). Path-substring matching
    /// is intentionally narrow — we DON'T match on substrings like "cuda"
    /// because pack paths and model paths can collide. The "in-flight
    /// fallback" that used to live here was the root cause of D-track
    /// cross-wiring (CUDA pack 404 + MobileCLIP in flight → MobileCLIP
    /// row showed cuda.zip error) and has been removed.</summary>
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
        return null;
    }

    private int _progressEventCount;

    /// <summary>B2: wall-clock UTC of the most recent progress event for ANY
    /// model. The no-progress watchdog (static) reads this so an active
    /// download on one slot keeps every slot's watchdog from false-failing
    /// under multi-download contention.</summary>
    private static DateTime _lastAnyProgressAt = DateTime.MinValue;

    private void OnEngineClientChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("ModelInstallerService.OnEngineClientChanged", () =>
        {
            if (e.PropertyName == nameof(EngineClient.ModelDownloadProgress))
            {
                // No [ENGINE-SUB] line here — HandleProgress already logs
                // throttled "[INSTALL] OnEngineClientChanged #N" entries
                // that serve the same purpose; double-logging would flood
                // app.log during a model install.
                HandleProgress(EngineClient.Instance.ModelDownloadProgress);
                return;
            }
            if (e.PropertyName == nameof(EngineClient.LastError))
            {
                DebugLog.Debug($"[ENGINE-SUB:ModelInstallerService] {e.PropertyName}");
                HandleEngineError(EngineClient.Instance.LastError);
                return;
            }
            if (e.PropertyName == nameof(EngineClient.Info))
            {
                DebugLog.Debug($"[ENGINE-SUB:ModelInstallerService] {e.PropertyName}");
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
        // B2: any download making progress means the engine is alive. The
        // no-progress watchdog consults this so one model going briefly
        // silent under multi-download contention isn't false-failed while
        // another model is actively streaming bytes. Set for EVERY progress
        // event, including the slot-less auto-installer runtime packs.
        _lastAnyProgressAt = DateTime.UtcNow;
        var n = Interlocked.Increment(ref _progressEventCount);
        if (n <= 5 || n % 50 == 0 || p.Fraction >= 0.999)
        {
            DebugLog.Info($"[INSTALL] OnEngineClientChanged #{n}: {p.ModelKind} {p.Fraction:P0} bytes={p.BytesDone}/{p.TotalBytes}");
        }
        var slot = SlotFor(p.ModelKind);
        if (slot is null)
        {
            // well-known auto-installer model_kinds are routed
            // through their own services (LlamaRuntimeAutoInstaller,
            // CudaAutoInstaller); demote the no-slot log so app.log
            // isn't flooded during their auto-install progress streams.
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
        slot.Apply(p, () => SentinelExistsForAnyOf(sentinelIds));
    }

    private void HandleEngineError(EngineError? error)
    {
        if (error is null) return;
        // Only route install-related errors. Other engine errors (e.g.
        // scan_failed, ipc_decode_failed) belong to other surfaces.
        var kind = error.Kind ?? string.Empty;
        var isInstallError =
            kind == "model_download_failed"
            || kind == "zip_extract_failed"
            || kind == "pack_not_available"
            || kind.StartsWith("prewarm_", StringComparison.OrdinalIgnoreCase);
        if (!isInstallError) return;

        // prewarm_cancelled is user-initiated; don't surface as a failure.
        if (kind == "prewarm_cancelled") return;

        // D-track fix: route by error.ModelKind first. The engine now stamps
        // every install-failure event with the originating model id, so we
        // don't need to infer it from the path string. SlotForErrorPath is
        // kept as a fallback for legacy emitters / non-model errors that
        // still carry a path.
        var slot = !string.IsNullOrEmpty(error.ModelKind)
            ? SlotFor(error.ModelKind)
            : SlotForErrorPath(error.Path);
        if (slot is null)
        {
            DebugLog.Warn($"[INSTALL] engine error '{kind}' has no routable slot (modelKind={error.ModelKind ?? "<null>"}, path={error.Path ?? "<null>"})");
            return;
        }
        DebugLog.Info($"[INSTALL] engine error → {slot.DisplayLabel}.Fail(): {error.Message}");
        slot.Fail(error.Message);
    }

    private static string[] SentinelIdsFor(ModelSlot slot)
    {
        if (ReferenceEquals(slot, Instance.Clip)) return ClipSentinelIds;
        if (ReferenceEquals(slot, Instance.Arcface)) return ArcfaceSentinelIds;
        if (ReferenceEquals(slot, Instance.RamPlus)) return RamPlusSentinelIds;
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

    /// <summary>Probe for the engine's canonical install marker at
    /// `%LOCALAPPDATA%\FileID\Models\.sentinels\{id}.installed`. Engine
    /// writes the file atomically (tmp+rename) only after every file in
    /// the bundle has landed successfully, so file presence is sufficient
    /// — no need for the defensive "is the dir empty?" check we used to
    /// do under the legacy per-model-dir sentinel layout.</summary>
    private static bool SentinelInstalled(string modelId)
    {
        try
        {
            return File.Exists(Path.Combine(AppPaths.ModelsDir, ".sentinels", $"{modelId}.installed"));
        }
        catch { return false; }
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void Set<T>(ref T field, T value, [CallerMemberName] string? propertyName = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value)) return;
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
    }
}
