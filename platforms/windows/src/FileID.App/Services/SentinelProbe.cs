// Shared install-sentinel probe. The engine writes one sentinel per
// installed model bundle under `%LOCALAPPDATA%\FileID\Models\.sentinels\`
// as either flat `{id}.installed` or content-hashed `{id}-{hash}.installed`
// (atomic temp+rename; see engine main.rs handle_prewarm_model). Every
// app-side "is it installed?" check must match BOTH forms — flat-only
// probes in SettingsView and the auto-installers never saw the hashed
// sentinels, so an already-installed CUDA runtime pack re-dispatched its
// prewarm on every Settings load / engine Ready (repeated 0%→100% progress
// spam with outcome=already_installed).

using System.IO;

namespace FileID.Services;

internal static class SentinelProbe
{
    public static bool Installed(string modelId) =>
        InstalledIn(Path.Combine(AppPaths.ModelsDir, ".sentinels"), modelId);

    public static bool InstalledIn(string sentinelsDir, string modelId)
    {
        try
        {
            if (File.Exists(Path.Combine(sentinelsDir, $"{modelId}.installed"))) return true;
            if (!Directory.Exists(sentinelsDir)) return false;
            // `{id}-*` keeps the match exact-id: `arcface` must not match a
            // hypothetical `arcface_xl-{hash}.installed`.
            foreach (var _ in Directory.EnumerateFiles(sentinelsDir, $"{modelId}-*.installed"))
            {
                return true;
            }
            return false;
        }
        catch { return false; }
    }
}
