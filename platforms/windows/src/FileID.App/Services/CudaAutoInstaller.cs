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
    // ONNX Runtime CUDA provider pack + cuDNN — flips the SCAN pipeline (RAM++,
    // CLIP, faces) off DirectML onto the CUDA EP (~3-5x). Separate from the
    // llama.cpp CUDA runtime above (that's the Deep Analyze VLM backend).
    private const string OrtCudaKind = "ort_cuda_x64";
    private const string CudnnKind = "cudnn_runtime_x64";
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

            // First: the ORT CUDA provider pack (the scan-pipeline accelerator).
            // Independent of the llama.cpp CUDA runtime below — own toggle, own
            // sentinel — so a box that already has llama-cuda still gets the EP.
            TryInstallOrtCudaPack();

            // Opt-out (the llama.cpp CUDA runtime for Deep Analyze).
            try
            {
                var settings = AppSettings.Load();
                if (settings.DisableAutoInstallCuda) return;
            }
            catch { /* fall through and try anyway */ }

            // Sentinel check — avoid re-firing the prewarm IPC when the
            // engine already dropped its install marker. Canonical path
            // matches ModelInstallerService.HasEngineSentinel: the engine
            // writes `Models/.sentinels/{id}.installed` atomically at the
            // end of handle_prewarm_model.
            try
            {
                var sentinel = Path.Combine(AppPaths.ModelsDir, ".sentinels", $"{ModelKind}.installed");
                var cudaDir = Path.Combine(AppPaths.ModelsDir, "llama.cpp-cuda");
                // Match the engine's VlmRunner (dir root OR bin/ subdir).
                bool mtmdPresent = File.Exists(Path.Combine(cudaDir, "llama-mtmd-cli.exe"))
                                || File.Exists(Path.Combine(cudaDir, "bin", "llama-mtmd-cli.exe"));
                if (File.Exists(sentinel) && mtmdPresent)
                {
                    DebugLog.Info("[CUDA-AUTO] CUDA llama.cpp already installed (mtmd-cli present); skipping.");
                    return;
                }
                if (File.Exists(sentinel))
                {
                    // Stale pre-mtmd CUDA build (e.g. b4475): sentinel present but
                    // the multimodal binary is missing. Clear the sentinel + cached
                    // zips so the prewarm re-downloads the current self-contained
                    // build (llama binaries + cudart).
                    DebugLog.Info("[CUDA-AUTO] CUDA runtime present but missing llama-mtmd-cli.exe (stale build) — reinstalling.");
                    try { File.Delete(sentinel); } catch { /* best-effort */ }
                    try { File.Delete(Path.Combine(cudaDir, "llama-runtime.zip")); } catch { /* best-effort */ }
                    try { File.Delete(Path.Combine(cudaDir, "cudart.zip")); } catch { /* best-effort */ }
                }
            }
            catch { /* if FS check fails, the engine's own short-circuit will catch it */ }

            DebugLog.Info("[CUDA-AUTO] NVIDIA detected + no sentinel — silently installing CUDA llama.cpp runtime.");
            // attach a fault sink so a Task.Run exception that
            // escapes the inner try/catch doesn't become an
            // UnobservedTaskException at GC time. Also bound the prewarm
            // with a 30-minute timeout — a stuck engine + an unawaited
            // wait would otherwise pin the closure (and any captured
            // state) forever.
            var prewarmTask = Task.Run(async () =>
            {
                using var timeoutCts = new System.Threading.CancellationTokenSource(TimeSpan.FromMinutes(30));
                try
                {
                    var prewarm = EngineClient.Instance.PrewarmModelAsync(ModelKind);
                    var winner = await Task.WhenAny(prewarm, Task.Delay(System.Threading.Timeout.Infinite, timeoutCts.Token))
                        .ConfigureAwait(false);
                    if (winner != prewarm)
                    {
                        DebugLog.Warn("[CUDA-AUTO] PrewarmModel timed out after 30 min.");
                        return;
                    }
                    await prewarm.ConfigureAwait(false);
                    DebugLog.Info("[CUDA-AUTO] PrewarmModel IPC dispatched.");
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("[CUDA-AUTO] PrewarmModel failed: " + ex.Message);
                }
            });
            _ = prewarmTask.ContinueWith(
                t => DebugLog.Error("[CUDA-AUTO] worker faulted: " + t.Exception),
                System.Threading.Tasks.TaskContinuationOptions.OnlyOnFaulted);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[CUDA-AUTO] TryStart threw: " + ex.Message);
        }
    }

    /// <summary>Silently install the ONNX Runtime CUDA provider pack + cuDNN on
    /// NVIDIA so the SCAN pipeline runs on the CUDA EP (~3-5x vs DirectML)
    /// without the user hunting for a Settings button — mirrors the
    /// "the right backend is just there" philosophy. Gated by
    /// <c>DisableAutoInstallCudnn</c>; own sentinel so it fires independently of
    /// the llama.cpp CUDA runtime. Safe to auto-enable: the engine's ep_guard
    /// reverts to DirectML if the CUDA bind ever crashes.</summary>
    private static void TryInstallOrtCudaPack()
    {
        try
        {
            if (AppSettings.Load().DisableAutoInstallCudnn) return;
        }
        catch { /* fall through and try anyway */ }

        try
        {
            // The engine writes this sentinel after the provider pack lands; it
            // (not a DLL scan) is the authoritative "installed" signal.
            var sentinel = Path.Combine(AppPaths.ModelsDir, ".sentinels", $"{OrtCudaKind}.installed");
            if (File.Exists(sentinel))
            {
                DebugLog.Info("[CUDA-AUTO] ORT CUDA provider pack already installed; skipping.");
                return;
            }
        }
        catch { /* if the FS check fails, the engine's own short-circuit catches it */ }

        DebugLog.Info("[CUDA-AUTO] NVIDIA detected + no ORT CUDA pack — silently installing CUDA provider + cuDNN.");
        var task = Task.Run(async () =>
        {
            try
            {
                // cuDNN first, provider last: the provider (ort_cuda_x64) is the
                // "installed" gate (see ModelInstallerService), and finishing it
                // last keeps the Accelerator slot's status accurate. Each
                // PrewarmModelAsync just dispatches the IPC; the engine dedupes
                // and short-circuits if the files + sentinel already exist.
                await EngineClient.Instance.PrewarmModelAsync(CudnnKind).ConfigureAwait(false);
                await EngineClient.Instance.PrewarmModelAsync(OrtCudaKind).ConfigureAwait(false);
                DebugLog.Info("[CUDA-AUTO] ORT CUDA provider + cuDNN prewarm dispatched.");
            }
            catch (Exception ex)
            {
                DebugLog.Warn("[CUDA-AUTO] ORT CUDA pack install failed: " + ex.Message);
            }
        });
        _ = task.ContinueWith(
            t => DebugLog.Error("[CUDA-AUTO] ORT CUDA worker faulted: " + t.Exception),
            System.Threading.Tasks.TaskContinuationOptions.OnlyOnFaulted);
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
            if (AppSettings.Load().DisableAutoInstallOpenVino) return;
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
