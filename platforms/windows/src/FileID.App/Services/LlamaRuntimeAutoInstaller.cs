// LlamaRuntimeAutoInstaller — silent install of the Vulkan llama.cpp runtime
// at engine-ready time so Deep Analyze "just works" the first time the user
// opens the tab.
//
// Previously this was an advisory banner inside Deep Analyze with an Install
// button. Most users didn't notice it until they tried to caption an image
// and got nothing. The CudaAutoInstaller pattern proved that silent install
// is the better default; this is the same idea for the base Vulkan runtime
// every Windows user needs (NVIDIA, AMD, Intel, Adreno — Vulkan covers all).
//
// PRIVACY: no telemetry — failure paths log locally only. The download is a
// plain HTTPS GET against the official llama.cpp GitHub release (the same
// source the previous manual button used). No new network surface.

using System;
using System.ComponentModel;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using FileID.ViewModels;

namespace FileID.Services;

internal static class LlamaRuntimeAutoInstaller
{
    private const string ModelKind = "llama_runtime_x64";

    /// <summary>Set after the first attempt in a process so engine respawns
    /// don't re-fire. Matches the CudaAutoInstaller pattern.</summary>
    private static int s_attempted; // 0 = not yet, 1 = done

    public static void Hook()
    {
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
            if (Interlocked.CompareExchange(ref s_attempted, 1, 0) != 0)
            {
                return;
            }
            if (EngineClient.Instance.State != EngineClient.LifecycleState.Ready)
            {
                Interlocked.Exchange(ref s_attempted, 0);
                return;
            }

            // Opt-out.
            try
            {
                var settings = AppSettings.Load();
                if (settings.DisableAutoInstallVulkanRuntime) return;
            }
            catch { /* fall through and try anyway */ }

            // Sentinel check — same canonical path the engine writes after
            // a successful prewarm. Matches ModelInstallerService.HasEngineSentinel.
            try
            {
                var sentinel = Path.Combine(AppPaths.ModelsDir, ".sentinels", $"{ModelKind}.installed");
                if (File.Exists(sentinel))
                {
                    DebugLog.Info("[VULKAN-AUTO] llama.cpp runtime already installed; skipping.");
                    return;
                }
            }
            catch { /* if FS check fails, engine's own short-circuit will catch it */ }

            DebugLog.Info("[VULKAN-AUTO] no sentinel — silently installing Vulkan llama.cpp runtime.");
            _ = Task.Run(async () =>
            {
                try
                {
                    await EngineClient.Instance.PrewarmModelAsync(ModelKind).ConfigureAwait(false);
                    DebugLog.Info("[VULKAN-AUTO] PrewarmModel IPC dispatched.");
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("[VULKAN-AUTO] PrewarmModel failed: " + ex.Message);
                }
            });
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[VULKAN-AUTO] TryStart threw: " + ex.Message);
        }
    }
}
