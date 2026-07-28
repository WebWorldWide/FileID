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
using System.Runtime.InteropServices;
using System.Runtime.CompilerServices;
using System.Threading;
using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Dispatching;

namespace FileID.Services;

internal sealed class ModelInstallerService : INotifyPropertyChanged
{
    // Sentinel model-id constants. The engine writes one sentinel file per
    // installed model bundle under `%LOCALAPPDATA%\FileID\Models\.sentinels\`
    // as either `{model.id}.installed` or a content-hashed
    // `{model.id}-{hash}.installed` (atomic temp+rename; see engine main.rs
    // handle_prewarm_model). `SentinelInstalled` matches BOTH forms. The id
    // strings here MUST match `Model.id` in engine/src/models/registry.rs.
    //
    // Static field init runs in source order, so these MUST be declared
    // before Instance — its ctor calls SeedFromSentinels which reads them.
    //
    // CLIP needs BOTH the image encoder (mobileclip_s2) and the text encoder
    // (clip_text) — they're separate model_kinds in the engine's registry
    // because they download from different paths in the Xenova mobileclip_s2
    // HuggingFace repo. The pre-scan validation in main.rs::handle_start_scan
    // requires both sentinels, so the slot's "Installed" state must reflect
    // that. The DeepVlm slot is the selected Deep Analyze model plus its
    // llama.cpp runtime; a different VLM sentinel must never make the selected
    // model look installed. ArcFace stays a single-sentinel "any-of".
    private static readonly string[] ClipSentinelIds = { "mobileclip_s2", "clip_text" };
    private static readonly string[] ArcfaceSentinelIds = { "arcface" };
    private static readonly string[] DeepVlmSentinelIds = { "qwen2_5_vl_7b", "gemma_3_4b", "mistral_small_3_2" };
    // RAM++ — the in-scan multi-label tagger. Single-sentinel "any-of".
    private static readonly string[] RamPlusSentinelIds = { "ram_plus" };
    private static readonly string[] WhisperSentinelIds = { "whisper" };
    private static readonly string[] BgeSentinelIds = { "bge_text" };
    // Candidate completion sentinels for the one-button acceleration row.
    // Exact requirements are vendor-specific in _acceleratorInstallKinds:
    // NVIDIA needs cuDNN + ORT CUDA + the CUDA llama runtime; Intel needs the
    // OpenVINO pack; AMD/ARM64 use built-in DirectML and download nothing.
    private static readonly string[] AcceleratorSentinelIds = { "ort_cuda_x64", "ort_openvino_x64" };

    /// <summary>Time the engine has to reach Ready before an Install
    /// click gives up and surfaces "Engine not ready" to the user. Raised
    /// 30 → 75 s: cold-start model loading on slow disks / ARM64 can still be
    /// legitimately initializing well past 30 s, and failing that early left
    /// only a Retry that immediately failed again.</summary>
    private static readonly TimeSpan WaitForReadyTimeout = TimeSpan.FromSeconds(75);

    /// <summary>Time after which a Downloading slot with no progress
    /// events gets flipped to Failed. Mirrors macOS WelcomeSheet's
    /// "stuck install" guard. B2: raised 30 → 60 s because under
    /// multi-download contention (welcome "Install all") one model's bytes
    /// can legitimately stall &gt;30 s
    /// while another saturates the link — and the watchdog now also
    /// consults <see cref="_lastAnyProgressAt"/> so any active download
    /// keeps every slot's watchdog alive.</summary>
    private static readonly TimeSpan NoProgressTimeout = TimeSpan.FromSeconds(60);

    public static ModelInstallerService Instance { get; } = new();

    public ModelSlot Clip { get; }
    public ModelSlot Arcface { get; }
    /// <summary>RAM++ — the primary in-scan image tagger (4585-tag multi-label
    /// ONNX). When absent the engine falls back to CLIP scene tags, but the
    /// onboarding "Install all" completion state includes it.</summary>
    public ModelSlot RamPlus { get; }
    /// <summary>Deep Analyze model — hardware-tiered Qwen2.5-VL 7B / Gemma 3 4B
    /// / Mistral-Small 3.2. Installing persists AppSettings.SelectedVlmModelKind
    /// so the Deep Analyze tab picks the freshly-installed model by default.</summary>
    public ModelSlot DeepVlm { get; }
    /// <summary>Whisper speech transcription for Deep Analyze (whisper.cpp CPU pack +
    /// the multilingual ggml-base model — MIT). Optional: when absent, audio with no
    /// descriptive metadata title keeps its original name instead of a transcript-based
    /// one. It is visible and included in onboarding completion.</summary>
    public ModelSlot Whisper { get; }
    /// <summary>BGE-small document text embedder — powers content-based document clustering
    /// in restructure (a doc clusters by what it SAYS, not its filename). Optional: when
    /// absent, documents cluster by filename. It is visible and included in
    /// onboarding completion.</summary>
    public ModelSlot Bge { get; }
    /// <summary>One-button GPU acceleration pack. NVIDIA installs the complete
    /// CUDA scanning + Deep Analyze stack; Intel offers OpenVINO; AMD/ARM64 use
    /// built-in DirectML. The welcome sheet adapts to detected hardware.</summary>
    public ModelSlot Accelerator { get; }

    /// <summary>True only when every artifact for the detected vendor's real
    /// acceleration pack is installed. Distinguishes a downloaded pack from a
    /// no-download built-in DirectML path.</summary>
    public bool AcceleratorIsRealInstall
    {
        get => _acceleratorIsRealInstall;
        private set => Set(ref _acceleratorIsRealInstall, value);
    }
    private bool _acceleratorIsRealInstall;

    public bool AcceleratorRestartRequired
    {
        get => _acceleratorRestartRequired;
        private set => Set(ref _acceleratorRestartRequired, value);
    }
    private bool _acceleratorRestartRequired;

    private int _installAllInFlight; // 0 = idle, 1 = in flight

    // UI dispatcher captured from a guaranteed UI-thread entry point (the
    // public install methods, before their first ConfigureAwait(false)). The
    // no-progress watchdog used to call DispatcherQueue.GetForCurrentThread()
    // from a thread-pool continuation, which returns null — so it could never
    // marshal slot.Fail() and a genuinely stuck download never surfaced.
    private Microsoft.UI.Dispatching.DispatcherQueue? _uiDispatcher;

    /// <summary>Deep Analyze model the welcome row installs. The recommendation
    /// ladder is Gemma (light), Qwen (balanced), then Mistral (high-end), with
    /// RAM, available memory, VRAM, architecture, and free disk all considered.</summary>
    private string _deepVlmModelKind = VlmRecommendation.Qwen;
    public string DeepVlmModelKind
    {
        get => _deepVlmModelKind;
        private set => Set(ref _deepVlmModelKind, value);
    }

    private string _deepVlmRecommendation = "Detecting memory, GPU, and free space…";
    public string DeepVlmRecommendation
    {
        get => _deepVlmRecommendation;
        private set => Set(ref _deepVlmRecommendation, value);
    }

    private string[] _acceleratorInstallKinds = { "cudnn_runtime_x64", "ort_cuda_x64", "llama_runtime_cuda_x64" };
    private bool _deepVlmSelectionIsUserInitiated;

    // APP-2: captured on the UI thread at ctor time (the singleton is first
    // touched during app startup on the UI thread — the same thread on which
    // it constructs the ModelSlots below, which rely on the same capture). The
    // no-progress watchdog must marshal slot.Fail back to the UI thread, but it
    // runs after `await ...ConfigureAwait(false)`, where
    // DispatcherQueue.GetForCurrentThread() returns null — so it uses this
    // captured reference rather than the ambient (null) one.
    private readonly Microsoft.UI.Dispatching.DispatcherQueue? _ui;

    private ModelInstallerService()
    {
        _ui = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        Clip = new ModelSlot(
            displayLabel: "CLIP ViT-B/32",
            approxBytes: 607_269_262UL,
            // Install both halves of CLIP: the image encoder (mobileclip_s2)
            // and the text encoder (clip_text). The engine's pre-scan check
            // requires both sentinels. Sequential so per-row progress UI
            // stays sane; the second prewarm short-circuits at the engine if
            // its files + sentinel are already on disk.
            installAction: async () =>
            {
                ClearCancelMarks("mobileclip_s2", "clip_text");
                await PrewarmAsync("mobileclip_s2").ConfigureAwait(false);
                await PrewarmAsync("clip_text").ConfigureAwait(false);
            });
        Arcface = new ModelSlot(
            displayLabel: "Face models (YuNet + SFace)",
            approxBytes: 39UL * 1024 * 1024,
            installAction: () =>
            {
                ClearCancelMarks("arcface_default");
                return PrewarmAsync("arcface_default");
            });
        RamPlus = new ModelSlot(
            displayLabel: "RAM++ image tagger",
            // ~882 MB fp16 ONNX (bakes the frozen tag-description embeddings in).
            approxBytes: 925_600_000UL,
            installAction: () =>
            {
                ClearCancelMarks("ram_plus");
                return PrewarmAsync("ram_plus");
            });
        Whisper = new ModelSlot(
            displayLabel: "Speech transcription (Whisper)",
            // ~5 MB whisper.cpp CPU pack + ~148 MB ggml-base model.
            approxBytes: 154UL * 1024 * 1024,
            installAction: () =>
            {
                ClearCancelMarks("whisper");
                return PrewarmAsync("whisper");
            });
        Bge = new ModelSlot(
            displayLabel: "Document understanding (BGE)",
            // ~135 MB BGE-small ONNX + a small vocab.
            approxBytes: 135UL * 1024 * 1024,
            installAction: () =>
            {
                ClearCancelMarks("bge_text");
                return PrewarmAsync("bge_text");
            });
        DeepVlm = new ModelSlot(
            displayLabel: "Qwen2.5-VL 7B",
            approxBytes: 6_100_000_000UL,
            installAction: () => InstallDeepVlmAsync(_deepVlmModelKind));
        // GPU Acceleration Pack. Display label + Message are
        // adaptive — UpdateAcceleratorForVendor() refreshes them as soon
        // as the engine reports detected hardware. Until then, the row
        // shows "Detecting GPU…" so the user knows it's waiting.
        Accelerator = new ModelSlot(
            displayLabel: "GPU Acceleration Pack",
            // ORT CUDA provider + cuDNN + CUDA llama.cpp/cudart.
            approxBytes: 1_394_000_000UL,
            // Install cuDNN AND the ORT CUDA provider. The provider
            // (ort_cuda_x64) goes LAST because it's the completion gate
            // (AcceleratorSentinelIds): finishing it last means its 100% is the
            // final event, so the slot lands cleanly on Installed instead of
            // flickering Installed→Downloading→Installed, and a cuDNN failure
            // can't leave the slot wrongly "Installed". A prewarm short-circuits
            // at the engine if files + sentinel are already on disk. The engine's
            // cuda_provider_present() + ORT_DYLIB_PATH pinning light up the CUDA
            // EP once the provider lands.
            installAction: InstallAcceleratorAsync);
        Accelerator.Message = "Detecting GPU…";

        Clip.PropertyChanged += OnSlotPropertyChanged;
        Arcface.PropertyChanged += OnSlotPropertyChanged;
        RamPlus.PropertyChanged += OnSlotPropertyChanged;
        DeepVlm.PropertyChanged += OnSlotPropertyChanged;
        Accelerator.PropertyChanged += OnSlotPropertyChanged;
        Whisper.PropertyChanged += OnSlotPropertyChanged;
        Bge.PropertyChanged += OnSlotPropertyChanged;

        SeedFromSentinels();
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;
        // If engine has already published Info (raced our ctor), apply now.
        UpdateAcceleratorForVendor(EngineClient.Instance.Info?.Hardware?.GpuVendor);
    }

    public async Task InstallDeepVlmAsync(string modelKind)
    {
        if (!await ModelLicenseGate.EnsureAcceptedAsync([modelKind]).ConfigureAwait(true))
        {
            return;
        }
        if (!CanInstallVlm(modelKind))
        {
            throw new InvalidOperationException($"{VlmRecommendation.DisplayName(modelKind)} does not fit this PC safely.");
        }
        SelectDeepVlmModel(modelKind, userInitiated: true);
        DeepVlm.ResetForRetry();
        DeepVlm.Status = ModelInstallStatus.Downloading;
        DeepVlm.Message = "Preparing the local vision runtime…";
        DeepVlm.CurrentModelKind = modelKind;
        ClearCancelMarks("llama_runtime_x64", modelKind);
        await PrewarmPrerequisiteAsync("llama_runtime_x64").ConfigureAwait(false);
        await PrewarmAsync(modelKind).ConfigureAwait(false);
    }

    public void SelectDeepVlmModel(string modelKind, bool userInitiated = true)
    {
        if (!VlmRecommendation.IsSupported(modelKind))
        {
            return;
        }
        if (DeepVlm.Status == ModelInstallStatus.Downloading
            || (userInitiated && !CanRunVlm(modelKind)))
        {
            return;
        }
        if (userInitiated)
        {
            _deepVlmSelectionIsUserInitiated = true;
        }
        DeepVlmModelKind = modelKind;
        DeepVlm.DisplayLabel = VlmRecommendation.DisplayName(modelKind);
        DeepVlm.ApproxBytes = VlmRecommendation.DownloadBytes(modelKind);
        DeepVlm.CurrentModelKind = modelKind;
        if (SentinelInstalled(modelKind))
        {
            if (SentinelInstalled("llama_runtime_x64"))
            {
                DeepVlm.Status = ModelInstallStatus.Installed;
                DeepVlm.Fraction = 1.0;
            }
            else
            {
                DeepVlm.ResetForRetry();
                DeepVlm.Message = "Vision weights are present; install the local llama.cpp runtime to finish setup.";
                DeepVlm.CurrentModelKind = modelKind;
            }
        }
        else
        {
            DeepVlm.ResetForRetry();
            DeepVlm.CurrentModelKind = modelKind;
        }
        if (userInitiated)
        {
            PersistSelectedVlmModelKind(modelKind);
        }
    }

    public bool CanRunVlm(string modelKind)
    {
        var profile = VlmRecommendation.CurrentProfile();
        return profile.TotalRamGb <= 0 || VlmRecommendation.CanRun(modelKind, profile);
    }

    public bool CanInstallVlm(string modelKind)
    {
        if (!CanRunVlm(modelKind)) return false;
        if (SentinelInstalled(modelKind)) return true;
        return VlmRecommendation.HasDiskFor(
            modelKind,
            VlmRecommendation.CurrentProfile().FreeDiskBytes);
    }

    private async Task InstallAcceleratorAsync()
    {
        var kinds = _acceleratorInstallKinds;
        if (!await ModelLicenseGate.EnsureAcceptedAsync(kinds).ConfigureAwait(true))
        {
            return;
        }
        ClearCancelMarks(kinds);
        foreach (var kind in kinds)
        {
            await PrewarmAsync(kind).ConfigureAwait(false);
        }
    }

    /// <summary> adapt the Accelerator slot to the detected GPU
    /// vendor. NVIDIA → installable cuDNN pack. Anything else → already-
    /// optimal Status=Installed with an explanatory Message. Called on
    /// engine Info changes + at construction time.</summary>
    private void UpdateAcceleratorForVendor(string? gpuVendor)
    {
        if (RuntimeInformation.ProcessArchitecture != Architecture.X64)
        {
            _acceleratorInstallKinds = [];
            Accelerator.DisplayLabel = "GPU Acceleration (ARM64)";
            Accelerator.Message = "DirectML is built in. Optional CUDA and OpenVINO packs are x64-only and are not offered on this ARM64 build.";
            Accelerator.Status = ModelInstallStatus.Installed;
            Accelerator.Fraction = 1.0;
            AcceleratorIsRealInstall = false;
            AcceleratorRestartRequired = false;
            RecomputeAggregates();
            return;
        }

        var vendor = (gpuVendor ?? string.Empty).ToLowerInvariant();
        switch (vendor)
        {
            case "nvidia":
                _acceleratorInstallKinds = ["cudnn_runtime_x64", "ort_cuda_x64", "llama_runtime_cuda_x64"];
                Accelerator.DisplayLabel = "GPU Acceleration Pack (NVIDIA)";
                Accelerator.ApproxBytes = 1_394_000_000UL;
                Accelerator.Message = "Installs CUDA scanning and Deep Analyze runtimes — up to 3-5x faster ML inference vs DirectML.";
                if (AcceleratorInstallComplete())
                {
                    Accelerator.Status = ModelInstallStatus.Installed;
                    Accelerator.Fraction = 1.0;
                    AcceleratorIsRealInstall = true;
                    Accelerator.Message = "CUDA acceleration is installed for scanning and Deep Analyze. Restart FileID after a new install to activate it.";
                }
                else if (Accelerator.Status != ModelInstallStatus.Downloading)
                {
                    Accelerator.Status = ModelInstallStatus.NotInstalled;
                    Accelerator.Fraction = 0;
                    AcceleratorIsRealInstall = false;
                }
                break;
            case "amd":
                _acceleratorInstallKinds = [];
                AcceleratorIsRealInstall = false;
                Accelerator.DisplayLabel = "GPU Acceleration (AMD)";
                Accelerator.Message = "DirectML is already optimal for your AMD GPU — no install needed.";
                Accelerator.Status = ModelInstallStatus.Installed;
                Accelerator.Fraction = 1.0;
                break;
            case "intel":
                _acceleratorInstallKinds = ["ort_openvino_x64"];
                Accelerator.DisplayLabel = "GPU Acceleration (Intel)";
                Accelerator.ApproxBytes = 41_300_000UL;
                Accelerator.Message = "Optional OpenVINO runtime for vendor-tuned Intel GPU inference. DirectML remains the fallback.";
                if (SentinelInstalled("ort_openvino_x64"))
                {
                    Accelerator.Status = ModelInstallStatus.Installed;
                    Accelerator.Fraction = 1.0;
                    AcceleratorIsRealInstall = true;
                    Accelerator.Message = "OpenVINO acceleration is installed. Restart FileID after a new install to activate it.";
                }
                else if (Accelerator.Status != ModelInstallStatus.Downloading)
                {
                    Accelerator.Status = ModelInstallStatus.NotInstalled;
                    Accelerator.Fraction = 0;
                    AcceleratorIsRealInstall = false;
                }
                break;
            case "qualcomm":
                _acceleratorInstallKinds = [];
                AcceleratorIsRealInstall = false;
                Accelerator.DisplayLabel = "GPU Acceleration (Snapdragon)";
                // QNN's SDK is proprietary (can't redistribute under commercial-
                // clean), so we never host it — the NPU is used only if the
                // device already provides QNN; otherwise DirectML.
                Accelerator.Message = "Snapdragon — DirectML active; the Hexagon NPU (QNN) is used automatically if your device provides it.";
                Accelerator.Status = ModelInstallStatus.Installed;
                Accelerator.Fraction = 1.0;
                break;
            case "none":
                _acceleratorInstallKinds = [];
                AcceleratorIsRealInstall = false;
                Accelerator.DisplayLabel = "GPU Acceleration";
                Accelerator.Message = "No GPU detected — scanning will run on CPU.";
                Accelerator.Status = ModelInstallStatus.Installed;
                Accelerator.Fraction = 1.0;
                break;
            case "":
                _acceleratorInstallKinds = [];
                AcceleratorIsRealInstall = false;
                Accelerator.DisplayLabel = "GPU Acceleration Pack";
                Accelerator.Message = "Detecting GPU…";
                break;
            default:
                _acceleratorInstallKinds = [];
                AcceleratorIsRealInstall = false;
                Accelerator.DisplayLabel = "GPU Acceleration";
                Accelerator.Message = "DirectML is the production path on your GPU.";
                Accelerator.Status = ModelInstallStatus.Installed;
                Accelerator.Fraction = 1.0;
                break;
        }
        RefreshAcceleratorRestartRequired();
    }

    /// <summary>
    /// Re-attach the EngineClient.PropertyChanged handler. Called by
    /// EngineClient at the start of each spawn so a stale subscription
    /// against an orphaned EngineClient doesn't keep firing — and so a
    /// download that was in flight when the engine crashed is correctly
    /// flipped to Failed (otherwise the row would spin forever).
    /// </summary>
    public void Reset(bool cleanShutdown = false)
    {
        EngineClient.Instance.PropertyChanged -= OnEngineClientChanged;
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;

        // Any in-flight download owned by the now-dead engine is unreachable —
        // flip to Failed so the user sees a Retry button instead of a permanent
        // spinner. On a crash + respawn the engine genuinely "restarted"; on a
        // deliberate app-driven stop/restart it did not, so don't claim it did —
        // use a neutral, still-truthful caption.
        var reason = cleanShutdown
            ? "Download interrupted — please retry."
            : "Engine restarted — please retry.";
        FailIfDownloading(Clip, reason);
        FailIfDownloading(Arcface, reason);
        // RamPlus + Accelerator were omitted: both reach Downloading (RamPlus is
        // a gate on AllInstalled/IsBusy), and SeedFromSentinels early-returns for
        // a still-Downloading slot, so an engine crash mid-download left them
        // spinning forever with no Retry — and a stuck RamPlus blocks the
        // "Install all" button + onboarding auto-dismiss permanently.
        FailIfDownloading(RamPlus, reason);
        FailIfDownloading(DeepVlm, reason);
        FailIfDownloading(Accelerator, reason);
        // Whisper + Bge reach Downloading via the per-card Install button (SlotFor
        // now routes "whisper"/"bge_text"); same crash-mid-download strand as above.
        FailIfDownloading(Whisper, reason);
        FailIfDownloading(Bge, reason);

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
        // Captured here, on the UI thread, before any ConfigureAwait(false).
        _uiDispatcher ??= Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread();
        if (Interlocked.CompareExchange(ref _installAllInFlight, 1, 0) != 0)
        {
            DebugLog.Info("[INSTALL] InstallAllAsync already in flight; ignoring duplicate request");
            return;
        }
        try
        {
            var restrictedKinds = new List<string>();
            if (DeepVlm.Status != ModelInstallStatus.Installed)
            {
                restrictedKinds.Add(_deepVlmModelKind);
            }
            var includeAccelerator = IncludeAcceleratorInInstallAll();
            if (includeAccelerator)
            {
                restrictedKinds.AddRange(_acceleratorInstallKinds);
            }
            // Declining a license must NOT abort the whole batch. Prompt for every
            // required policy, then skip ONLY the slots whose kinds are tied to a
            // declined policy — the license-free core models (CLIP / RAM++ / faces
            // / Whisper / BGE, and an un-gated VLM like Qwen/Mistral) still install.
            var declinedPolicies = await ModelLicenseGate
                .RequestDeclinedPoliciesAsync(restrictedKinds).ConfigureAwait(true);

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
            var slotsToInstall = new List<ModelSlot>
            {
                Clip,
                Arcface,
                RamPlus,
                Bge,
                Whisper,
                DeepVlm,
            };
            if (includeAccelerator)
            {
                slotsToInstall.Add(Accelerator);
            }
            // Drop slots that need a declined license. They stay NotInstalled (so
            // AllInstalled / the Welcome auto-dismiss can't falsely report success)
            // with a non-blocking "skipped" message rather than a red Failed row.
            if (declinedPolicies.Count > 0)
            {
                slotsToInstall.RemoveAll(slot =>
                {
                    if (!SlotHasDeclinedKind(slot, declinedPolicies)) return false;
                    if (slot.Status != ModelInstallStatus.Installed)
                    {
                        slot.Message = "License not accepted — skipped";
                    }
                    DebugLog.Info($"[INSTALL] InstallAllAsync skipping {slot.DisplayLabel}: license declined");
                    return true;
                });
            }
            foreach (var slot in slotsToInstall)
            {
                if (slot.Status == ModelInstallStatus.Installed) continue;
                slot.ResetForRetry();
                slot.Status = ModelInstallStatus.Downloading;
                slot.Message = "Queued — starting download…";
                slot.LastProgressAt = now;
                // ResetForRetry nulled CurrentModelKind and PrewarmAsync only
                // restamps it after WaitForReadyAsync, so during engine
                // cold-start a pre-stamped Downloading row was kind-less and
                // its Cancel button lost the cancel (C7). Stamp the slot's
                // primary kind now; PrewarmAsync overwrites with the live kind.
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

    /// <summary>Include the accelerator in Install All only for x64 NVIDIA
    /// (CUDA stack) or Intel (OpenVINO) when that real pack is missing.</summary>
    private bool IncludeAcceleratorInInstallAll()
    {
        if (RuntimeInformation.ProcessArchitecture != Architecture.X64) return false;
        var vendor = (EngineClient.Instance.Info?.Hardware?.GpuVendor ?? string.Empty).ToLowerInvariant();
        if (vendor is not ("nvidia" or "intel")) return false;
        return Accelerator.Status is ModelInstallStatus.NotInstalled
                                 or ModelInstallStatus.Failed;
    }

    public Task CancelAllAsync() => EngineClient.Instance.CancelPrewarmAsync();

    /// <summary>The explicit Install/Retry click is the ONLY place a kind's
    /// cancel mark is erased. EngineClient.PrewarmModelAsync used to clear it
    /// at dispatch — but during engine cold start the dispatch is parked in
    /// WaitForReadyAsync, so a Cancel clicked in that window was erased by the
    /// very prewarm it should have cancelled (C9, the surviving branch (a) of
    /// the C7 finding).</summary>
    private static void ClearCancelMarks(params string[] kinds)
    {
        foreach (var k in kinds)
        {
            EngineClient.Instance.ClearPrewarmCancelMark(k);
        }
    }

    // Kinds installed by the SAME UI slot. A multi-kind slot's per-row Cancel must
    // cancel ALL its sub-downloads: slot.CurrentModelKind is only the latest of two
    // concurrent prewarms, so cancelling just that left the first (mobileclip_s2 /
    // cudnn_runtime_x64) streaming to completion after the user clicked Cancel.
    private static readonly string[][] SlotKindGroups =
    {
        new[] { "mobileclip_s2", "clip_text" },        // CLIP slot
        new[] { "cudnn_runtime_x64", "ort_cuda_x64", "llama_runtime_cuda_x64" },
        new[] { "llama_runtime_x64", VlmRecommendation.Gemma },
        new[] { "llama_runtime_x64", VlmRecommendation.Qwen },
        new[] { "llama_runtime_x64", VlmRecommendation.Mistral },
    };

    /// <summary>First kind a slot's installAction prewarms. InstallAllAsync's
    /// pre-stamp uses this so a Downloading row is never kind-less; for a
    /// multi-kind slot CancelModelAsync expands it back to the full
    /// SlotKindGroups group, so the first kind cancels the whole slot.</summary>
    private string? PrimaryKindFor(ModelSlot slot)
    {
        if (ReferenceEquals(slot, Clip)) return "mobileclip_s2";
        if (ReferenceEquals(slot, Arcface)) return "arcface_default";
        if (ReferenceEquals(slot, RamPlus)) return "ram_plus";
        if (ReferenceEquals(slot, Whisper)) return "whisper";
        if (ReferenceEquals(slot, Bge)) return "bge_text";
        if (ReferenceEquals(slot, DeepVlm)) return _deepVlmModelKind;
        if (ReferenceEquals(slot, Accelerator)) return _acceleratorInstallKinds.FirstOrDefault();
        return null;
    }

    /// <summary>Every model kind a slot's installAction downloads. Used by
    /// InstallAllAsync to decide whether a declined license policy affects the
    /// slot (skip it) or leaves it fully un-gated (install it).</summary>
    private IReadOnlyList<string> InstallKindsForSlot(ModelSlot slot)
    {
        if (ReferenceEquals(slot, Clip)) return new[] { "mobileclip_s2", "clip_text" };
        if (ReferenceEquals(slot, Arcface)) return new[] { "arcface_default" };
        if (ReferenceEquals(slot, RamPlus)) return new[] { "ram_plus" };
        if (ReferenceEquals(slot, Whisper)) return new[] { "whisper" };
        if (ReferenceEquals(slot, Bge)) return new[] { "bge_text" };
        if (ReferenceEquals(slot, DeepVlm)) return new[] { "llama_runtime_x64", _deepVlmModelKind };
        if (ReferenceEquals(slot, Accelerator)) return _acceleratorInstallKinds;
        return Array.Empty<string>();
    }

    /// <summary>True when any kind this slot installs is gated by a license
    /// policy the user just declined.</summary>
    private bool SlotHasDeclinedKind(ModelSlot slot, IReadOnlySet<string> declinedPolicies)
    {
        foreach (var kind in InstallKindsForSlot(slot))
        {
            var policyKey = ModelLicenseGate.PolicyKeyForModelKind(kind);
            if (policyKey is not null && declinedPolicies.Contains(policyKey)) return true;
        }
        return false;
    }

    /// Cancel a single slot's in-flight download(s) (the per-row Cancel button), so
    /// cancelling one row no longer aborts every other concurrent install. For a
    /// multi-kind slot (CLIP, Accelerator) this cancels every kind the slot owns.
    /// A null kind (slot never got a kind stamped) is a logged no-op — it used to
    /// fall through to CancelPrewarmAsync(null), the cancel-EVERYTHING path (C7).
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

    /// <summary>Refresh the welcome-sheet VLM recommendation from current RAM,
    /// available memory, dedicated VRAM, architecture, and models-drive space.
    /// Automatic refreshes never persist or replace an explicit user choice.
    /// When any VLM is already installed, the row resolves to a safe installed
    /// model: persisted selection first, then the hardware recommendation, then
    /// the best remaining installed model.</summary>
    public void UpdateDeepVlmRecommendation()
    {
        var profile = VlmRecommendation.CurrentProfile();
        if (profile.TotalRamGb <= 0) return;
        var recommendation = VlmRecommendation.Recommend(profile);
        DeepVlmRecommendation = recommendation.Reason;

        if (DeepVlm.Status == ModelInstallStatus.Downloading)
        {
            return;
        }

        string? persisted = null;
        var persistedIsExplicit = false;
        try
        {
            var settings = AppViewModel.Instance.Settings;
            persisted = settings.SelectedVlmModelKind;
            persistedIsExplicit = settings.SelectedVlmModelWasUserChosen;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[INSTALL] Couldn't read the persisted VLM selection: " + ex.Message);
        }

        if (_deepVlmSelectionIsUserInitiated || persistedIsExplicit)
        {
            var selected = _deepVlmSelectionIsUserInitiated ? DeepVlmModelKind : persisted;
            if (VlmRecommendation.IsSupported(selected))
            {
                SelectDeepVlmModel(selected!, userInitiated: false);
                if (!VlmRecommendation.CanRun(selected!, profile))
                {
                    DeepVlmRecommendation = $"Keeping your {VlmRecommendation.DisplayName(selected!)} selection, but current available memory is too low to run it safely. Choose a lighter model to continue.";
                }
                else if (!string.Equals(selected, recommendation.ModelKind, StringComparison.Ordinal))
                {
                    DeepVlmRecommendation = $"{recommendation.DisplayName} is recommended for this hardware; keeping your {VlmRecommendation.DisplayName(selected!)} selection.";
                }
                return;
            }
        }

        var installedSelection = VlmRecommendation.ResolveInstalledSelection(
            persisted,
            recommendation.ModelKind,
            profile,
            SentinelInstalled);
        if (installedSelection is not null)
        {
            SelectDeepVlmModel(installedSelection, userInitiated: false);
            if (!string.Equals(installedSelection, recommendation.ModelKind, StringComparison.Ordinal))
            {
                DeepVlmRecommendation = $"{recommendation.DisplayName} is recommended for this hardware; using the installed {VlmRecommendation.DisplayName(installedSelection)} model.";
            }
            return;
        }

        SelectDeepVlmModel(recommendation.ModelKind, userInitiated: false);
        DebugLog.Info($"[INSTALL] Deep Analyze recommendation: {recommendation.DisplayName} (RAM={profile.TotalRamGb:F1} GB, available={profile.AvailableRamGb:F1} GB, VRAM={profile.DedicatedVramMb} MB, GPU={profile.GpuVendor}, arch={profile.Architecture})");
    }

    /// <summary>Persist the Deep Analyze model the user just chose to install so
    /// the Deep Analyze tab + the manual auto-chain pass use the same weights.
    /// The explicit-choice flag is persisted even when the selected id happens
    /// to equal the historical default.</summary>
    private static void PersistSelectedVlmModelKind(string kind)
    {
        try
        {
            // Route through the shared singleton, NOT a fresh AppSettings.Load():
            // a fresh instance shares the static debounce CTS, so its Save() cancels
            // the singleton's pending write and persists a snapshot loaded from disk
            // that lacks the singleton's in-memory changes (lost update). (audit A8)
            var s = FileID.ViewModels.AppViewModel.Instance.Settings;
            if (s.SelectedVlmModelKind == kind && s.SelectedVlmModelWasUserChosen) return;
            s.SelectedVlmModelKind = kind;
            s.SelectedVlmModelWasUserChosen = true;
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
    /// <summary>Core sub-1 GB models (CLIP + RAM++ + ArcFace) — gates the
    /// Welcome sheet RE-SHOW decision only. The multi-GB Deep Analyze VLM is
    /// install-once/skippable and must NOT re-nag on every launch, so it's
    /// excluded here (mirrors macOS shouldShowWelcome()). AllInstalled —
    /// which DOES include the VLM — still drives auto-dismiss + the Done
    /// button inside the sheet.</summary>
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
        // RAM++ is the primary in-scan tagger and is now hosted on
        // Web-World-Wide/ram-plus-onnx (WS5 upload landed), so it gates
        // onboarding completion alongside CLIP/ArcFace/DeepVlm. (If RAM++ is
        // ever missing at runtime, tagging still degrades to CLIP scene-tags.)
        CoreModelsInstalled =
            Clip.Status == ModelInstallStatus.Installed
            && Arcface.Status == ModelInstallStatus.Installed
            && RamPlus.Status == ModelInstallStatus.Installed;
        AllInstalled =
            CoreModelsInstalled
            && Bge.Status == ModelInstallStatus.Installed
            && Whisper.Status == ModelInstallStatus.Installed
            && DeepVlm.Status == ModelInstallStatus.Installed;
        IsBusy =
            Clip.Status == ModelInstallStatus.Downloading
            || Arcface.Status == ModelInstallStatus.Downloading
            || RamPlus.Status == ModelInstallStatus.Downloading
            || Bge.Status == ModelInstallStatus.Downloading
            || Whisper.Status == ModelInstallStatus.Downloading
            || DeepVlm.Status == ModelInstallStatus.Downloading
            || Accelerator.Status == ModelInstallStatus.Downloading;
        RefreshAcceleratorRestartRequired();
    }

    private void RefreshAcceleratorRestartRequired()
    {
        var hardware = EngineClient.Instance.Info?.Hardware;
        if (hardware is null || !AcceleratorInstallComplete())
        {
            AcceleratorRestartRequired = false;
            return;
        }
        var expected = AcceleratorActivationPolicy.ExpectedProvider(
            hardware.GpuVendor,
            RuntimeInformation.ProcessArchitecture);
        string? providerOverride = null;
        try
        {
            providerOverride = AppViewModel.Instance.Settings.GpuExecutionProviderOverride;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[INSTALL] Couldn't read the execution-provider override: " + ex.Message);
        }
        AcceleratorRestartRequired = AcceleratorActivationPolicy.RestartRequired(
            hardware.GpuVendor,
            hardware.ExecutionProvider,
            RuntimeInformation.ProcessArchitecture,
            installComplete: true,
            providerOverride);
        if (AcceleratorRestartRequired && Accelerator.Status == ModelInstallStatus.Installed)
        {
            Accelerator.Message = $"Acceleration installed. Restart the engine to switch from {hardware.ExecutionProvider} to {expected}.";
        }
        else if (expected is not null
                 && !string.Equals(hardware.ExecutionProvider, expected, StringComparison.OrdinalIgnoreCase)
                 && !string.IsNullOrWhiteSpace(providerOverride)
                 && !string.Equals(providerOverride, "auto", StringComparison.OrdinalIgnoreCase))
        {
            Accelerator.Message = $"{expected.ToUpperInvariant()} acceleration files are installed, but the '{providerOverride}' provider override keeps {hardware.ExecutionProvider} active. Choose Auto or {expected} in Settings to use the pack.";
        }
        else if (expected is not null && Accelerator.Status == ModelInstallStatus.Installed)
        {
            Accelerator.Message = $"{expected.ToUpperInvariant()} acceleration is active.";
        }
    }

    private void OnSlotPropertyChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("ModelInstallerService.OnSlotPropertyChanged", () =>
    {
        if (e.PropertyName != nameof(ModelSlot.Status)) return;
        // Arm the no-progress watchdog from the Status-set-to-Downloading
        // transition point so EVERY entry into Downloading is watched — the
        // fresh install (PrewarmAsync / pre-stamp) AND a late-progress
        // Failed→Downloading revert in ModelSlot.Apply (the case a
        // once-failed-then-revived download previously left unwatched forever).
        // A Status PropertyChanged only fires on a genuine change, so Status ==
        // Downloading here means it just transitioned in. ArmNoProgressWatchdog
        // is idempotent (Interlocked guard), so exactly one watchdog runs.
        if (sender is ModelSlot slot && slot.Status == ModelInstallStatus.Downloading)
        {
            ArmNoProgressWatchdog(slot);
        }
        RecomputeAggregates();
    });

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
        SeedSlot(Whisper, WhisperSentinelIds);
        SeedSlot(Bge, BgeSentinelIds);
        SeedDeepVlmFromSentinels();
        // Accelerator slot — only flip to Installed if the
        // sentinel exists. Otherwise leave it as
        // UpdateAcceleratorForVendor decided (NotInstalled for NVIDIA,
        // Installed-with-message for non-NVIDIA / CPU).
        if (AcceleratorInstallComplete())
        {
            Accelerator.Status = ModelInstallStatus.Installed;
            Accelerator.Fraction = 1.0;
            Accelerator.Message = "GPU acceleration files are installed. FileID will verify the active provider when the engine is ready.";
            AcceleratorIsRealInstall = true;
        }
        if (EngineClient.Instance.Info is not null)
        {
            UpdateDeepVlmRecommendation();
        }
        RecomputeAggregates();
    }

    /// <summary>Alias for callers that just want a sentinel re-check
    /// without caring about the engine-event side of the state machine
    /// (MainWindow startup, DeepAnalyzeView model-install panel).</summary>
    public void Refresh() => SeedFromSentinels();

    private void SeedDeepVlmFromSentinels()
    {
        if (DeepVlm.Status is ModelInstallStatus.Downloading or ModelInstallStatus.Failed)
        {
            return;
        }
        if (!SentinelInstalled("llama_runtime_x64"))
        {
            DeepVlm.Status = ModelInstallStatus.NotInstalled;
            return;
        }

        var installed = VlmRecommendation.SupportedKinds.Where(SentinelInstalled).ToArray();
        if (installed.Length == 0)
        {
            DeepVlm.Status = ModelInstallStatus.NotInstalled;
            return;
        }

        string? persisted = null;
        try { persisted = AppViewModel.Instance.Settings.SelectedVlmModelKind; }
        catch (Exception ex) { DebugLog.Warn("[INSTALL] Couldn't read VLM selection while seeding sentinels: " + ex.Message); }
        var selected = persisted is not null
                       && installed.Contains(persisted, StringComparer.Ordinal)
            ? persisted
            : installed[0];
        // The persisted pick can point at weights that were never installed
        // while a DIFFERENT VLM is fully on disk (fresh onboarding: pick
        // Mistral in the Welcome combo → Install all → settings still hold
        // the qwen default). Every Deep Analyze consumer reads Settings
        // directly, so leave it stale and each Analyze fails with a
        // misleading "model isn't installed". Re-point Settings at the
        // installed model; an explicit user pick (WasUserChosen) still wins.
        if (!string.Equals(selected, persisted, StringComparison.Ordinal))
        {
            try
            {
                var s = AppViewModel.Instance.Settings;
                if (!s.SelectedVlmModelWasUserChosen)
                {
                    s.SelectedVlmModelKind = selected;
                    s.Save();
                    DebugLog.Info(
                        $"[INSTALL] SelectedVlmModelKind self-healed to installed '{selected}' (was '{persisted}', no weights on disk).");
                }
            }
            catch (Exception ex)
            {
                DebugLog.Warn("[INSTALL] VLM selection self-heal failed: " + ex.Message);
            }
        }
        SelectDeepVlmModel(selected, userInitiated: false);
    }

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
    private async Task PrewarmPrerequisiteAsync(string modelKind)
    {
        await EngineClient.Instance.WaitForReadyAsync(WaitForReadyTimeout).ConfigureAwait(false);
        if (EngineClient.Instance.IsPrewarmCancelled(modelKind)) return;
        await EngineClient.Instance.PrewarmModelAsync(modelKind, clearCancelMark: false).ConfigureAwait(false);
    }

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

        // Wait for engine Ready before touching slot.Status. If the engine
        // never reaches Ready, the slot stays in its prior state and the
        // user sees a clear error message instead of a misleading spinner.
        try
        {
            await EngineClient.Instance.WaitForReadyAsync(WaitForReadyTimeout).ConfigureAwait(false);
        }
        catch (TimeoutException ex)
        {
            // A timeout means the engine is still spinning up (cold-start model
            // load on a slow disk / ARM box), NOT that it crashed — tell the
            // user to wait a moment and retry rather than implying a failure.
            DebugLog.Warn($"[INSTALL] WaitForReadyAsync timed out for '{modelKind}': {ex.Message}");
            slot.Fail("Engine still starting up — give it a moment, then retry.");
            return;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[INSTALL] WaitForReadyAsync threw for '{modelKind}': {ex.Message}");
            slot.Fail("Engine not ready: " + ex.Message);
            return;
        }

        // A per-row Cancel during the wait above only sets the app-side mark —
        // its CancelPrewarm IPC faults because the engine isn't Ready, so the
        // engine never hears about it. Honor the mark here instead of
        // dispatching a download the user already cancelled (C9). The next
        // explicit Install/Retry click clears the mark (ClearCancelMarks).
        if (EngineClient.Instance.IsPrewarmCancelled(modelKind))
        {
            DebugLog.Info($"[INSTALL] {modelKind} was cancelled while waiting for engine Ready; skipping dispatch.");
            slot.ResetForRetry();
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
        // Anchor the no-progress watchdog clock at IPC-send time for BOTH the
        // fresh and the InstallAll-pre-stamped path. The watchdog is armed off
        // the Status→Downloading transition (OnSlotPropertyChanged), which for a
        // pre-stamped slot fired before WaitForReadyAsync; without this refresh
        // its window would be measured from the pre-stamp and could false-fire
        // while a slow cold-start engine was still becoming Ready.
        slot.LastProgressAt = DateTime.UtcNow;
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

        // The no-progress watchdog is armed from the Status-set-to-Downloading
        // transition (OnSlotPropertyChanged → ArmNoProgressWatchdog), so it is
        // already live for this slot here — including the InstallAll pre-stamp
        // and any Failed→Downloading revert. No explicit arm needed.
    }

    private void ArmNoProgressWatchdog(ModelSlot slot, CancellationToken ct = default)
    {
        // Exactly one live watchdog per slot. A running loop already re-checks
        // LastProgressAt each window, so it covers any Downloading state the slot
        // is in; a second arm request while one is live is a no-op. When the loop
        // exits (terminal state, fired failure, or cancellation) the finally
        // clears the guard, so a later Downloading transition — crucially the
        // Failed→Downloading revert in ModelSlot.Apply after the watchdog already
        // failed the slot — re-arms a fresh watchdog instead of leaving a revived
        // download unwatched forever.
        if (Interlocked.CompareExchange(ref slot.WatchdogLive, 1, 0) != 0) return;
        // This method can be reached after `await ...ConfigureAwait(false)`, so
        // the ambient DispatcherQueue.GetForCurrentThread() would return null
        // here and silently disable the watchdog. slot.Fail mutates x:Bind-
        // observed state, so it must marshal to the UI thread: prefer the
        // dispatcher captured at the public install entry points, falling back
        // to the ctor-time capture (APP-2).
        var ui = _uiDispatcher ?? _ui;
        _ = Task.Run(async () =>
        {
            try
            {
                // Loop, re-checking every window. The old one-shot watchdog
                // waited a single NoProgressTimeout and then stopped forever, so
                // an install that streamed progress past 60 s and THEN stalled
                // was never caught. Re-arm after each live window instead.
                while (true)
                {
                    await Task.Delay(NoProgressTimeout, ct).ConfigureAwait(false);
                    // cancellation check after the delay — if the user cancelled
                    // the install during the watchdog window, don't surface a "no
                    // response" error on top of a clean cancellation flow.
                    if (ct.IsCancellationRequested) return;
                    // Read-only check off-thread is fine (status/timestamp are
                    // primitives + DateTime; no torn-read risk on x64/ARM64).
                    // Slot reached a terminal state (Installed/Failed) or a fresh
                    // install replaced it — nothing left to watch.
                    if (slot.Status != ModelInstallStatus.Downloading) return;
                    // B2: this slot OR any other download progressed within the
                    // last window → the engine is alive; re-arm and keep watching.
                    var lastAny = new DateTime(Interlocked.Read(ref _lastAnyProgressAtTicks), DateTimeKind.Utc);
                    var lastProgress = slot.LastProgressAt > lastAny ? slot.LastProgressAt : lastAny;
                    if (DateTime.UtcNow - lastProgress < NoProgressTimeout) continue;
                    var modelKind = slot.CurrentModelKind ?? slot.DisplayLabel;
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
                    return;
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
            finally
            {
                // Release the single-owner guard so the next Downloading
                // transition can arm a fresh watchdog.
                Interlocked.Exchange(ref slot.WatchdogLive, 0);
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
            case "llama_runtime_x64":
                return DeepVlm;
            case "ram_plus":
            case "ram-plus":
                return RamPlus;
            case "whisper":
                return Whisper;
            case "bge_text":
                return Bge;
            // The CUDA provider pack + cuDNN (NVIDIA) and the OpenVINO pack
            // (Intel) all route to the single Accelerator slot.
            case "ort_cuda_x64":
            case "cudnn_runtime_x64":
            case "ort_openvino_x64":
            case "llama_runtime_cuda_x64":
                return Accelerator;
            default:
                return null;
        }
    }

    /// <summary>Runtime variants without their own welcome-sheet row. Their
    /// explicit Settings/accelerator actions still emit progress, so demote
    /// otherwise-unroutable events to debug instead of flooding the log.</summary>
    private static bool IsAutoInstallerOnly(string? modelKind)
    {
        return modelKind is "llama_runtime_cuda_x64"
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
    private static long _lastAnyProgressAtTicks = DateTime.MinValue.Ticks;

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
                    UpdateDeepVlmRecommendation();
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
        // event, including slot-less runtime packs.
        Interlocked.Exchange(ref _lastAnyProgressAtTicks, DateTime.UtcNow.Ticks);
        var n = Interlocked.Increment(ref _progressEventCount);
        if (n <= 5 || n % 50 == 0 || p.Fraction >= 0.999)
        {
            DebugLog.Info($"[INSTALL] OnEngineClientChanged #{n}: {p.ModelKind} {p.Fraction:P0} bytes={p.BytesDone}/{p.TotalBytes}");
        }
        var slot = SlotFor(p.ModelKind);
        if (slot is null)
        {
            // Known slot-less runtime variants have their own Settings flow;
            // demote the no-slot log so their progress does not flood app.log.
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
        slot.Apply(p, () => InstallCompleteFor(slot));
    }

    private void HandleEngineError(EngineError? error)
    {
        if (error is null) return;
        // Only route install-related errors. Other engine errors (e.g.
        // scan_failed, ipc_decode_failed) belong to other surfaces.
        var kind = error.Kind ?? string.Empty;
        var isInstallError =
            kind == "model_download_failed"
            || kind == "model_download_disk_full"
            || kind == "download_tls_pin_failed"
            || kind == "zip_extract_failed"
            || kind == "pack_not_available"
            // A stale engine that doesn't know a model_kind, or one that can't
            // resolve its models dir, stamps the originating model id on the
            // error. Route both to the install slot so the welcome row flips to
            // Failed with the engine's actionable message instead of spinning
            // forever behind a raw red toast.
            || kind == "unknown_model"
            || kind == "models_dir_unavailable"
            // sentinel_dir_create_failed / sentinel_write_failed /
            // sentinel_rename_failed: the bytes all landed but the engine
            // couldn't register the install marker, and it emits NO terminal
            // progress event after these — without routing them the row spins
            // at ~100% "Downloading" forever with no Retry (C10).
            || kind.StartsWith("sentinel_", StringComparison.OrdinalIgnoreCase)
            || kind.StartsWith("prewarm_", StringComparison.OrdinalIgnoreCase);
        if (!isInstallError) return;

        // prewarm_cancelled is user-initiated: not a failure (no red Retry toast),
        // but the slot must still LEAVE Downloading or it sticks at "Cancelling…"
        // forever — wedging IsBusy/AllInstalled (and the Install-all gate). Reset it
        // to NotInstalled so the user can re-install. ModelSlot.Set marshals to the
        // UI thread internally, so this is safe off the engine-event thread.
        if (kind == "prewarm_cancelled")
        {
            if (!string.IsNullOrEmpty(error.ModelKind))
            {
                SlotFor(error.ModelKind)?.ResetForRetry();
            }
            return;
        }

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
            var safePath = error.Path is null ? "<null>" : PathRedactor.Redact(error.Path);
            DebugLog.Warn($"[INSTALL] engine error '{kind}' has no routable slot (modelKind={error.ModelKind ?? "<null>"}, path={safePath})");
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
        if (ReferenceEquals(slot, Instance.Whisper)) return WhisperSentinelIds;
        if (ReferenceEquals(slot, Instance.Bge)) return BgeSentinelIds;
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

    private bool InstallCompleteFor(ModelSlot slot)
    {
        if (ReferenceEquals(slot, Clip))
        {
            return ClipSentinelIds.All(SentinelInstalled);
        }
        if (ReferenceEquals(slot, Accelerator))
        {
            return AcceleratorInstallComplete();
        }
        if (ReferenceEquals(slot, DeepVlm) && !string.IsNullOrEmpty(slot.CurrentModelKind))
        {
            return SentinelInstalled("llama_runtime_x64")
                && SentinelInstalled(slot.CurrentModelKind);
        }
        return SentinelExistsForAnyOf(SentinelIdsFor(slot));
    }

    private bool AcceleratorInstallComplete()
        => _acceleratorInstallKinds.Length > 0
            && _acceleratorInstallKinds.All(SentinelInstalled);

    /// <summary>Probe for the engine's install marker under
    /// `%LOCALAPPDATA%\FileID\Models\.sentinels\`. The engine writes EITHER
    /// `{id}.installed` OR, for versioned bundles, a content-hashed
    /// `{id}-{hash}.installed` (e.g. `mobileclip_s2-6e850a21215b0755.installed`,
    /// `arcface-….installed`, `ram_plus-….installed`) — both atomic (tmp+rename)
    /// only after every bundle file landed, so presence is sufficient. Match
    /// BOTH forms: an exact-name check (cheap) then the hashed variant. The `-`
    /// after {id} guards against an id that is a prefix of another (e.g.
    /// `arcface` must not match a hypothetical `arcface_xl-….installed`).
    /// (Was exact-`{id}.installed` only, so hashed sentinels read as
    /// NotInstalled and the Welcome sheet re-showed every launch.)
    /// The matching lives in <see cref="SentinelProbe"/> so onboarding and
    /// Settings agree on installed state.</summary>
    private static bool SentinelInstalled(string modelId) => SentinelProbe.Installed(modelId);

    public event PropertyChangedEventHandler? PropertyChanged;

    private void Set<T>(ref T field, T value, [CallerMemberName] string? propertyName = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value)) return;
        field = value;
        // x:Bind reads these aggregates on the UI thread, so PropertyChanged must
        // fire there. Reset()/SeedFromSentinels (and other engine-driven paths)
        // can run on a thread-pool continuation; marshal through the captured UI
        // dispatcher to avoid an RPC_E_WRONG_THREAD native fast-fail — the same
        // pattern ModelSlot.Set uses. (audit A2)
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
