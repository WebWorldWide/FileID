// SmolVlmAutoInstaller — silent install of the SmolVLM weights at engine-ready
// time so the background auto-tag pass has a model the first time it runs.
// SmolVLM is the default tagger (smallest/fastest VLM, ~700 MB gguf + mmproj).
//
// Mirrors LlamaRuntimeAutoInstaller: at engine-ready, if the smolvlm sentinel
// (and weights) are absent, fire PrewarmModelAsync("smolvlm") once. CLIP scene
// tags act as an INSTANT PLACEHOLDER during the scan; SmolVLM tags supersede
// them as the background pass lands (ReadStore prefers source='vlm' over
// source='auto'). On the first scan the model may still be downloading when the
// auto-chain checks the VLM slot, so auto-tagging kicks in from the next scan;
// the user can also run a manual pass from the Deep Analyze tab once it's in.
//
// PRIVACY: no telemetry — failure paths log locally only. The download is a
// plain HTTPS GET against the official ggml-org HuggingFace GGUF repo (the same
// canonical source the Deep Analyze model picker uses). No new network surface.

using System;
using System.ComponentModel;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
using FileID.ViewModels;

namespace FileID.Services;

internal static class SmolVlmAutoInstaller
{
    private const string ModelKind = "smolvlm";

    /// <summary>Set after the first attempt in a process so engine respawns
    /// don't re-fire. Matches the CudaAutoInstaller / LlamaRuntimeAutoInstaller
    /// pattern.</summary>
    private static int s_attempted; // 0 = not yet, 1 = done

    /// <summary>C1: re-arm the one-shot gate so a later engine-Ready (e.g.
    /// after a crash + respawn that interrupted a mid-flight download)
    /// re-evaluates the sentinel/weights and re-fires if still missing.
    /// Without it a crash during the ~700 MB download would abandon SmolVLM
    /// for the rest of the session → no VLM tags until a full app restart.
    /// Called from EngineClient's ReadyEvent arm.</summary>
    public static void ResetAttempt() => Interlocked.Exchange(ref s_attempted, 0);

    public static void Hook()
    {
        TryStart();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
    }

    private static void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("SmolVlmAutoInstaller.OnEngineChanged", () =>
        {
            if (e.PropertyName is nameof(EngineClient.State)
                               or nameof(EngineClient.Info))
            {
                DebugLog.Debug($"[ENGINE-SUB:SmolVlmAutoInstaller] {e.PropertyName}");
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

            // Opt-out.
            try
            {
                var settings = AppSettings.Load();
                if (settings.DisableAutoInstallSmolVlm) return;
            }
            catch { /* fall through and try anyway */ }

            // Sentinel + weights check — the engine writes
            // `Models/.sentinels/{id}.installed` atomically at the end of
            // handle_prewarm_model. We ALSO require the gguf weights on disk: a
            // sentinel without weights (an interrupted download) should
            // re-install rather than short-circuit on the sentinel alone.
            try
            {
                var vlmDir = Path.Combine(AppPaths.ModelsDir, "vlm", "smolvlm");
                var sentinel = Path.Combine(AppPaths.ModelsDir, ".sentinels", $"{ModelKind}.installed");
                bool weightsPresent = File.Exists(Path.Combine(vlmDir, "model.gguf"))
                                   && File.Exists(Path.Combine(vlmDir, "mmproj.gguf"));
                if (File.Exists(sentinel) && weightsPresent)
                {
                    DebugLog.Info("[SMOLVLM-AUTO] SmolVLM already installed (weights present); skipping.");
                    return;
                }
                if (File.Exists(sentinel))
                {
                    DebugLog.Info("[SMOLVLM-AUTO] sentinel present but weights missing (interrupted download) — reinstalling.");
                    try { File.Delete(sentinel); } catch { /* best-effort */ }
                }
            }
            catch { /* if FS check fails, the engine's own short-circuit will catch it */ }

            DebugLog.Info("[SMOLVLM-AUTO] no sentinel — silently installing SmolVLM weights (~700 MB).");
            _ = Task.Run(async () =>
            {
                try
                {
                    await EngineClient.Instance.PrewarmModelAsync(ModelKind).ConfigureAwait(false);
                    DebugLog.Info("[SMOLVLM-AUTO] PrewarmModel IPC dispatched.");
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("[SMOLVLM-AUTO] PrewarmModel failed: " + ex.Message);
                }
            });
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[SMOLVLM-AUTO] TryStart threw: " + ex.Message);
        }
    }
}
