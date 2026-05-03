// ModelInstallerService — orchestrates the three install statuses the
// Welcome sheet renders: CLIP, ArcFace, and the recommended VLM.
//
// Each model has a typed install status (NotInstalled / Downloading /
// Installed / Failed). The actual fetching is delegated to the engine via
// the IPC `prewarmModel` command — the engine knows the canonical URLs +
// SHA256s + 12-way parallel range-GET strategy. Progress flows back via
// EngineClient.ModelDownloadProgress events; we snapshot them here for
// per-model rendering.
//
// PRIVACY: this class never makes a network call directly. It only sends
// IPC commands; the engine is the sole network surface.

using System.ComponentModel;
using System.IO;
using System.Runtime.CompilerServices;
using FileID.ViewModels;

namespace FileID.Services;

internal enum ModelInstallStatus
{
    NotInstalled,
    Downloading,
    Installed,
    Failed,
}

internal sealed class ModelInstallerService : INotifyPropertyChanged
{
    public static ModelInstallerService Instance { get; } = new();

    private ModelInstallerService()
    {
        Refresh();
        EngineClient.Instance.PropertyChanged += OnEngineClientChanged;
    }

    // Per-model status. Each is bound by the Welcome sheet.
    private ModelInstallStatus _clipStatus;
    public ModelInstallStatus ClipStatus
    {
        get => _clipStatus;
        private set => Set(ref _clipStatus, value);
    }

    private ModelInstallStatus _arcfaceStatus;
    public ModelInstallStatus ArcfaceStatus
    {
        get => _arcfaceStatus;
        private set => Set(ref _arcfaceStatus, value);
    }

    private ModelInstallStatus _vlmStatus;
    public ModelInstallStatus VlmStatus
    {
        get => _vlmStatus;
        private set => Set(ref _vlmStatus, value);
    }

    private double _clipProgress;
    public double ClipProgress
    {
        get => _clipProgress;
        private set => Set(ref _clipProgress, value);
    }

    private double _arcfaceProgress;
    public double ArcfaceProgress
    {
        get => _arcfaceProgress;
        private set => Set(ref _arcfaceProgress, value);
    }

    private double _vlmProgress;
    public double VlmProgress
    {
        get => _vlmProgress;
        private set => Set(ref _vlmProgress, value);
    }

    private string? _statusMessage;
    public string? StatusMessage
    {
        get => _statusMessage;
        private set => Set(ref _statusMessage, value);
    }

    /// <summary>True iff every model the user opted to install is now Installed.</summary>
    public bool AllInstalled =>
        ClipStatus == ModelInstallStatus.Installed
        && ArcfaceStatus == ModelInstallStatus.Installed
        && VlmStatus == ModelInstallStatus.Installed;

    /// <summary>
    /// Re-check on-disk state. The engine-side `prewarmModel` writes a
    /// `.fileid-installed` sentinel into each model dir on success; we
    /// detect it here.
    /// </summary>
    public void Refresh()
    {
        ClipStatus    = HasSentinel(Path.Combine(AppPaths.ModelsDir, "clip_text"))
                      || HasSentinel(Path.Combine(AppPaths.ModelsDir, "mobileclip_image"))
                          ? ModelInstallStatus.Installed
                          : ModelInstallStatus.NotInstalled;
        ArcfaceStatus = HasSentinel(Path.Combine(AppPaths.ModelsDir, "arcfaceIResNet50"))
                      || HasSentinel(Path.Combine(AppPaths.ModelsDir, "arcfaceMobileFace"))
                          ? ModelInstallStatus.Installed
                          : ModelInstallStatus.NotInstalled;
        VlmStatus     = Directory.Exists(AppPaths.HuggingFaceDir)
                      && Directory.EnumerateDirectories(AppPaths.HuggingFaceDir).Any()
                          ? ModelInstallStatus.Installed
                          : ModelInstallStatus.NotInstalled;
    }

    public Task InstallClipAsync()        => PrewarmAsync("mobileclip_s2");
    public Task InstallArcfaceAsync()     => PrewarmAsync("arcface_default");
    public Task InstallRecommendedVlmAsync() => PrewarmAsync("qwen2_5_vl_3b");

    public async Task InstallAllAsync()
    {
        // Engine processes IPC commands serially through its job queue,
        // so we can fire-and-forget all three; the engine handles
        // ordering. Each one's progress events route back to the per-model
        // status fields via OnEngineClientChanged.
        await InstallClipAsync();
        await InstallArcfaceAsync();
        await InstallRecommendedVlmAsync();
    }

    public Task CancelAllAsync() => EngineClient.Instance.CancelPrewarmAsync();

    private async Task PrewarmAsync(string modelKind)
    {
        try
        {
            await EngineClient.Instance.PrewarmModelAsync(modelKind);
            // Set the matching status to Downloading immediately; the
            // engine's first ModelDownloadProgress event will refine the
            // fraction.
            switch (modelKind)
            {
                case "mobileclip_s2":      ClipStatus = ModelInstallStatus.Downloading; break;
                case "arcface_default":    ArcfaceStatus = ModelInstallStatus.Downloading; break;
                case "qwen2_5_vl_3b":      VlmStatus = ModelInstallStatus.Downloading; break;
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Prewarm '{modelKind}' failed: {ex.Message}");
            StatusMessage = "Couldn't reach the engine. " + ex.Message;
        }
    }

    private void OnEngineClientChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(EngineClient.ModelDownloadProgress))
        {
            return;
        }
        var progress = EngineClient.Instance.ModelDownloadProgress;
        if (progress is null)
        {
            return;
        }
        switch (progress.ModelKind)
        {
            case "mobileclip_s2":
                ClipProgress = progress.Fraction;
                if (progress.Fraction >= 1.0)
                {
                    ClipStatus = ModelInstallStatus.Installed;
                }
                break;
            case "arcface_default":
            case "arcface_iresnet50":
            case "arcface_mobileface":
                ArcfaceProgress = progress.Fraction;
                if (progress.Fraction >= 1.0)
                {
                    ArcfaceStatus = ModelInstallStatus.Installed;
                }
                break;
            case "qwen2_5_vl_3b":
            case "qwen2_5_vl_7b":
            case "gemma_3_4b":
            case "smolvlm":
                VlmProgress = progress.Fraction;
                if (progress.Fraction >= 1.0)
                {
                    VlmStatus = ModelInstallStatus.Installed;
                }
                break;
        }
        StatusMessage = progress.Message;
    }

    private static bool HasSentinel(string dir)
    {
        try
        {
            return Directory.Exists(dir) && File.Exists(Path.Combine(dir, ".fileid-installed"));
        }
        catch { return false; }
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void Set<T>(ref T field, T value, [CallerMemberName] string? propertyName = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value))
        {
            return;
        }
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
        if (propertyName is nameof(ClipStatus) or nameof(ArcfaceStatus) or nameof(VlmStatus))
        {
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(AllInstalled)));
        }
    }
}
