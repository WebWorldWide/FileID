// CudaAutoInstaller — GPU-vendor DETECTION for the Accelerator slot.
//
// Historically this also SILENTLY installed the NVIDIA CUDA packs (the ORT
// CUDA provider + cuDNN, and the llama.cpp CUDA runtime) the moment the
// engine reported an NVIDIA GPU on launch. That violated the rule that a
// GPU acceleration pack downloads ONLY on an explicit user action, so the
// auto-install dispatch was removed. The NVIDIA packs now reach install
// exclusively through the user-driven paths (WelcomeSheet GPU button,
// Settings install, Install-all). This service keeps only the cheap
// vendor detection so the Accelerator slot can still surface
// recommended/installed status (which is read from the engine's sentinels
// by ModelInstallerService, not fired from here).
//
// PRIVACY: no telemetry — detection logs locally only and issues no
// network call.

using System.ComponentModel;
using System.IO;
using FileID.ViewModels;

namespace FileID.Services;

internal static class CudaAutoInstaller
{
    // ONNX Runtime OpenVINO provider pack — Intel's scan-pipeline accelerator.
    private const string OpenVinoKind = "ort_openvino_x64";

    /// <summary>Set after the first attempt in a process so engine respawns
    /// don't re-fire. Bool is enough — the engine itself dedupes via its
    /// IN_FLIGHT hashset anyway, but cheaper to short-circuit here.</summary>
    private static int s_attempted; // 0 = not yet, 1 = done

    /// <summary>C1: re-arm the one-shot gate so a later engine-Ready (e.g.
    /// after a crash + respawn) re-evaluates the sentinel/binary and re-fires
    /// if still missing. Called from EngineClient's ReadyEvent arm.</summary>
    public static void ResetAttempt() => System.Threading.Interlocked.Exchange(ref s_attempted, 0);

    public static void Hook()
    {
        // Run any check that fits the current EngineClient state (e.g. if
        // the engine is already Ready by the time we subscribe), then
        // listen for future state changes.
        TryStart();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
    }

    private static void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("CudaAutoInstaller.OnEngineChanged", () =>
        {
            if (e.PropertyName is nameof(EngineClient.State)
                               or nameof(EngineClient.Info))
            {
                DebugLog.Debug($"[ENGINE-SUB:CudaAutoInstaller] {e.PropertyName}");
                TryStart();
            }
        });

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
            if (vendor == "intel")
            {
                // Intel: the OpenVINO EP (Apache-2.0) is the accelerated scan
                // path. No llama CUDA runtime applies on Intel.
                TryInstallOpenVinoPack();
                return;
            }
            if (vendor != "nvidia")
            {
                // AMD / Snapdragon / none — DirectML is the path; nothing to
                // fetch. (Snapdragon's QNN SDK is proprietary, so we never host
                // it.) Leave s_attempted set so we don't re-check every event.
                return;
            }

            // NVIDIA detected. We deliberately do NOT auto-install the CUDA
            // packs here (the ORT CUDA provider + cuDNN, or the llama.cpp CUDA
            // runtime): a GPU acceleration pack downloads ONLY on an explicit
            // user action (WelcomeSheet GPU button, Settings install,
            // Install-all). Detection-only — the Accelerator slot reads its
            // recommended/installed status from the engine's sentinels via
            // ModelInstallerService. Leave s_attempted set so we don't re-check
            // every event.
            DebugLog.Info("[CUDA-AUTO] NVIDIA detected — CUDA packs are install-on-demand (no auto-install).");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[CUDA-AUTO] TryStart threw: " + ex.Message);
        }
    }

    /// <summary>Silently install the ONNX Runtime OpenVINO pack on Intel so the
    /// scan pipeline runs on the OpenVINO EP instead of DirectML. Apache-2.0,
    /// so commercial-clean to redistribute. Gated by
    /// <c>DisableAutoInstallOpenVino</c>; own sentinel. Safe to auto-enable:
    /// ep_guard reverts to DirectML if the OpenVINO bind crashes, and if the
    /// pack artifact isn't hosted yet the download 404s gracefully and Intel
    /// stays on DirectML.</summary>
    private static void TryInstallOpenVinoPack()
    {
        try
        {
            if (AppViewModel.Instance.Settings.DisableAutoInstallOpenVino) return;
        }
        catch { /* fall through and try anyway */ }

        try
        {
            var sentinel = Path.Combine(AppPaths.ModelsDir, ".sentinels", $"{OpenVinoKind}.installed");
            if (File.Exists(sentinel))
            {
                DebugLog.Info("[CUDA-AUTO] ORT OpenVINO pack already installed; skipping.");
                return;
            }
        }
        catch { /* engine short-circuit catches it */ }

        DebugLog.Info("[CUDA-AUTO] Intel detected + no OpenVINO pack — silently installing OpenVINO EP.");
        var task = Task.Run(async () =>
        {
            try
            {
                await EngineClient.Instance.PrewarmModelAsync(OpenVinoKind).ConfigureAwait(false);
                DebugLog.Info("[CUDA-AUTO] ORT OpenVINO prewarm dispatched.");
            }
            catch (Exception ex)
            {
                DebugLog.Warn("[CUDA-AUTO] ORT OpenVINO pack install failed: " + ex.Message);
            }
        });
        _ = task.ContinueWith(
            t => DebugLog.Error("[CUDA-AUTO] OpenVINO worker faulted: " + t.Exception),
            System.Threading.Tasks.TaskContinuationOptions.OnlyOnFaulted);
    }
}
