// CudaAutoInstaller — silent install of the CUDA-flavored llama.cpp runtime
// when the engine reports an NVIDIA GPU.
//
// Deep Analyze runs 15-25% faster on the CUDA build than on the default
// Vulkan build (per shared/docs/MODELS.md). On macOS the Metal backend is
// always present so there's nothing to install; on Windows + NVIDIA the
// pack is a separate ~200 MB download. The previous design required users
// to find a button in Settings, which most missed. This service mirrors
// macOS's "the right backend is just there" experience.
//
// PRIVACY: no telemetry — failure paths log locally only. The download
// itself is the user's NVIDIA GPU triggering an off-the-shelf llama.cpp
// release from GitHub, which is the same canonical source the manual
// install button has always used. No new network surface.

using System.ComponentModel;
using System.IO;
using FileID.ViewModels;

namespace FileID.Services;

internal static class CudaAutoInstaller
{
    private const string ModelKind = "llama_runtime_cuda_x64";
    private const string SentinelDir = "llama.cpp-cuda";

    /// <summary>Set after the first attempt in a process so engine respawns
    /// don't re-fire. Bool is enough — the engine itself dedupes via its
    /// IN_FLIGHT hashset anyway, but cheaper to short-circuit here.</summary>
    private static int s_attempted; // 0 = not yet, 1 = done

    public static void Hook()
    {
        // Run any check that fits the current EngineClient state (e.g. if
        // the engine is already Ready by the time we subscribe), then
        // listen for future state changes.
        TryStart();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
    }

    private static void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(EngineClient.State)
                           or nameof(EngineClient.Info))
        {
            TryStart();
        }
    }

    private static void TryStart()
    {
        try
        {
            if (System.Threading.Interlocked.CompareExchange(ref s_attempted, 1, 0) != 0)
            {
                return;
            }
            if (EngineClient.Instance.State != EngineClient.LifecycleState.Ready)
            {
                // Not ready yet — undo the gate so the next State change re-evaluates.
                System.Threading.Interlocked.Exchange(ref s_attempted, 0);
                return;
            }
            var hw = EngineClient.Instance.Info?.Hardware;
            if (hw is null)
            {
                System.Threading.Interlocked.Exchange(ref s_attempted, 0);
                return;
            }
            var vendor = (hw.GpuVendor ?? string.Empty).ToLowerInvariant();
            if (vendor != "nvidia")
            {
                // Different vendor — don't attempt; leave s_attempted set so
                // we don't keep re-checking on every PropertyChanged.
                return;
            }

            // Opt-out.
            try
            {
                var settings = AppSettings.Load();
                if (settings.DisableAutoInstallCuda) return;
            }
            catch { /* fall through and try anyway */ }

            // Sentinel check — avoid re-downloading an existing install.
            try
            {
                var sentinel = Path.Combine(AppPaths.ModelsDir, SentinelDir, ".fileid-installed");
                if (File.Exists(sentinel))
                {
                    DebugLog.Info("[CUDA-AUTO] CUDA llama.cpp already installed; skipping.");
                    return;
                }
            }
            catch { /* if FS check fails, the engine's own short-circuit will catch it */ }

            DebugLog.Info("[CUDA-AUTO] NVIDIA detected + no sentinel — silently installing CUDA llama.cpp runtime.");
            _ = Task.Run(async () =>
            {
                try
                {
                    await EngineClient.Instance.PrewarmModelAsync(ModelKind).ConfigureAwait(false);
                    DebugLog.Info("[CUDA-AUTO] PrewarmModel IPC dispatched.");
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("[CUDA-AUTO] PrewarmModel failed: " + ex.Message);
                }
            });
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[CUDA-AUTO] TryStart threw: " + ex.Message);
        }
    }
}
