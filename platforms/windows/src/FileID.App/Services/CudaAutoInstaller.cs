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
        // B1: also re-evaluate when the VLM slot changes. We defer the
        // ~650 MB CUDA speed pack until a VLM is actually installed (see
        // TryStart), so the first launch's bandwidth + disk go to the
        // functional models the app needs to scan + tag rather than a
        // speed upgrade contending with them. This is the re-trigger that
        // fires CUDA once SmolVLM finishes downloading.
        ModelInstallerService.Instance.Vlm.PropertyChanged += OnVlmChanged;
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

    private static void OnVlmChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("CudaAutoInstaller.OnVlmChanged", () =>
        {
            if (e.PropertyName == nameof(ModelSlot.Status))
            {
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

            // B1: defer until a VLM is actually installed. The CUDA llama
            // runtime ONLY accelerates VLM inference (Deep Analyze + the
            // background auto-tag pass). Pulling its ~650 MB at engine-ready
            // — concurrently with the SmolVLM (~700 MB) + Vulkan-runtime
            // downloads the app needs first — was a primary cause of the
            // "very slow" first-run (three big downloads contending with the
            // first scan's GPU work). Re-arm so the Vlm-slot PropertyChanged
            // (OnVlmChanged) re-fires this once a VLM lands. Until then there's
            // nothing for CUDA to accelerate anyway, so deferring costs nothing.
            if (ModelInstallerService.Instance.Vlm.Status != ModelInstallStatus.Installed)
            {
                DebugLog.Info("[CUDA-AUTO] deferring CUDA runtime until a VLM is installed (avoids starving the SmolVLM/Vulkan downloads on first run).");
                System.Threading.Interlocked.Exchange(ref s_attempted, 0);
                return;
            }

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
}
