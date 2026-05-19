// ClassifierAutoInstaller — silent install of the MobileNetV3 scene
// classifier at engine-ready time so Library auto-tags include semantic
// labels ("Dog", "Beach", "Document") on the first scan a user runs.
//
// Without the classifier the engine falls back to enriched-extras only,
// and TopTwoTags ends up showing "Has Location" / "Wide" — useless for
// content discovery. The classifier model is small enough (~22 MB ONNX +
// ~21 KB labels) to be auto-installable without surprising the user with
// a heavy download. Mirrors LlamaRuntimeAutoInstaller in shape + opt-out
// + sentinel-gating.
//
// PRIVACY: no telemetry — failure paths log locally only. The download
// is a plain HTTPS GET against HuggingFace (same source the manual
// banner button uses). No new network surface.

using System;
using System.ComponentModel;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using FileID.ViewModels;

namespace FileID.Services;

internal static class ClassifierAutoInstaller
{
    private const string ModelKind = "classifier_mobilenetv3";

    /// <summary>Set after the first attempt in a process so engine respawns
    /// don't re-fire. Matches LlamaRuntimeAutoInstaller / CudaAutoInstaller.</summary>
    private static int s_attempted; // 0 = not yet, 1 = done

    public static void Hook()
    {
        TryStart();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
    }

    private static void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("ClassifierAutoInstaller.OnEngineChanged", () =>
        {
            if (e.PropertyName is nameof(EngineClient.State)
                               or nameof(EngineClient.Info))
            {
                DebugLog.Debug($"[ENGINE-SUB:ClassifierAutoInstaller] {e.PropertyName}");
                TryStart();
            }
        });

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

            // Opt-out. Symmetric with the Vulkan auto-installer: a user
            // who explicitly disabled auto-install of the classifier
            // (e.g. for offline use, or to keep startup quiet) is honored.
            try
            {
                var settings = AppSettings.Load();
                if (settings.DisableAutoInstallClassifier) return;
            }
            catch { /* fall through and try anyway */ }

            // Sentinel check — same canonical path the engine writes after
            // a successful prewarm. Matches ModelInstallerService's
            // ClassifierSentinelIds path resolution.
            try
            {
                var sentinel = Path.Combine(AppPaths.ModelsDir, ".sentinels", $"{ModelKind}.installed");
                if (File.Exists(sentinel))
                {
                    DebugLog.Info("[CLASSIFIER-AUTO] sentinel present, skipping.");
                    return;
                }
            }
            catch { /* if FS check fails, engine's own short-circuit will catch it */ }

            DebugLog.Info("[CLASSIFIER-AUTO] sentinel missing, fetching MobileNetV3 (~22 MB).");
            _ = Task.Run(async () =>
            {
                try
                {
                    await EngineClient.Instance.PrewarmModelAsync(ModelKind).ConfigureAwait(false);
                    DebugLog.Info("[CLASSIFIER-AUTO] PrewarmModel IPC dispatched.");
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("[CLASSIFIER-AUTO] PrewarmModel failed: " + ex.Message);
                }
            });
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[CLASSIFIER-AUTO] TryStart threw: " + ex.Message);
        }
    }
}
