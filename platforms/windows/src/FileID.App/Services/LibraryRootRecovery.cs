// LibraryRootRecovery — heals a stale LastFolderPath at startup.
//
// app-settings.json can point at a folder that no longer exists (deleted,
// renamed, or a test corpus cleaned up) while the engine DB still holds a
// full scan of a DIFFERENT root. Without recovery the app boots into a
// contradictory state: the sidebar header names the dead folder, Start Scan
// targets it (and fails with a misleading "No supported files found"), and
// the Library grid shows the DB's files from the other root.
//
// Recovery rule: if the saved folder is MISSING but the DB's most recent
// scan root EXISTS on disk, fall back to that root (it is what the library
// contents actually belong to). If neither exists — e.g. an unplugged
// external drive — change nothing: the saved path may come back when the
// drive is re-attached, and clearing it would bounce the user to onboarding
// while their library data is still intact.

using System;
using System.Collections.Generic;
using System.IO;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.ViewModels;

namespace FileID.Services;

internal static class LibraryRootRecovery
{
    /// <summary>Pure decision rule (unit-testable): returns the replacement
    /// root, or null when nothing should change. <paramref name="dirExists"/>
    /// is injected so tests don't touch the filesystem.</summary>
    internal static string? PickRecoveredRoot(
        string? lastFolderPath,
        IReadOnlyList<string?> dbRootsNewestFirst,
        Func<string, bool> dirExists)
    {
        if (string.IsNullOrWhiteSpace(lastFolderPath))
        {
            return null; // never picked — onboarding is the correct state
        }
        if (dirExists(lastFolderPath))
        {
            return null; // saved folder is fine
        }
        foreach (var raw in dbRootsNewestFirst)
        {
            if (string.IsNullOrWhiteSpace(raw)) continue;
            var root = raw.TrimEnd('\\', '/');
            if (root.Length == 0) continue;
            if (string.Equals(root, lastFolderPath.TrimEnd('\\', '/'), StringComparison.OrdinalIgnoreCase))
            {
                continue; // same dead folder the settings already point at
            }
            if (dirExists(root))
            {
                return root;
            }
        }
        return null; // nothing reachable — leave state alone (drive may return)
    }

    /// <summary>Fire-and-forget startup pass. Filesystem + DB probes run on
    /// the thread pool (Directory.Exists can stall on a dead network path);
    /// the FolderPath swap is marshaled back to the UI dispatcher.</summary>
    public static async Task RunAsync(Microsoft.UI.Dispatching.DispatcherQueue ui)
    {
        var last = AppViewModel.Instance.FolderPath;
        if (string.IsNullOrWhiteSpace(last)) return;

        string? recovered;
        bool lastMissing = false;
        try
        {
            recovered = await Task.Run(() =>
            {
                if (SafeDirectoryExists(last)) return null;
                lastMissing = true;
                var roots = ReadRecentScanRoots(AppPaths.DbPath, limit: 10);
                return PickRecoveredRoot(last, roots, SafeDirectoryExists);
            }).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[RECOVER] library-root probe failed: " + ex.Message);
            return;
        }

        if (recovered is null)
        {
            if (lastMissing)
            {
                DebugLog.Warn($"[RECOVER] saved library folder is unreachable and no scanned root exists on disk; leaving settings untouched: {PathRedactor.Redact(last)}");
            }
            return;
        }

        ui.TryEnqueue(() =>
        {
            var vm = AppViewModel.Instance;
            // The user may have picked a new folder while we probed — never
            // clobber a fresh deliberate choice.
            if (!string.Equals(vm.FolderPath, last, StringComparison.Ordinal)) return;
            DebugLog.Info($"[RECOVER] saved library folder missing ({PathRedactor.Redact(last)}); falling back to the DB's scanned root {PathRedactor.Redact(recovered)}.");
            vm.FolderPath = recovered;
            try
            {
                EngineClient.Instance.LastWarning = new EngineError(
                    "library_root_recovered",
                    $"Your previous library folder ({Path.GetFileName(last.TrimEnd('\\', '/'))}) no longer exists. FileID switched back to the library it last scanned: {recovered}.",
                    null);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("[RECOVER] surfacing the recovery banner failed: " + ex.Message);
            }
        });
    }

    private static bool SafeDirectoryExists(string path)
    {
        try { return Directory.Exists(path); }
        catch { return false; }
    }

    /// <summary>Most-recent scan roots from scan_sessions, newest first,
    /// deduped case-insensitively. Empty on any failure (no DB yet, locked,
    /// old schema) — recovery is strictly best-effort.</summary>
    private static IReadOnlyList<string?> ReadRecentScanRoots(string dbPath, int limit)
    {
        var roots = new List<string?>();
        try
        {
            if (!File.Exists(dbPath)) return roots;
            using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                {
                    DataSource = dbPath,
                    Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                }.ToString());
            conn.Open();
            using var cmd = conn.CreateCommand();
            cmd.CommandText = """
                SELECT root_path FROM scan_sessions
                WHERE root_path IS NOT NULL AND root_path != ''
                ORDER BY started_at DESC
                LIMIT $limit
                """;
            cmd.Parameters.AddWithValue("$limit", limit);
            using var reader = cmd.ExecuteReader();
            var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            while (reader.Read())
            {
                if (reader.IsDBNull(0)) continue;
                var root = reader.GetString(0);
                if (seen.Add(root)) roots.Add(root);
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[RECOVER] scan_sessions read failed: " + ex.Message);
        }
        return roots;
    }
}
