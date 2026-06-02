// ThemeHelper — crash-safe resource lookups from code-behind.
//
// WHY: `Application.Current.Resources[key]` (the ResourceDictionary INDEXER) does
// the full merged-dictionary + active-theme-dictionary traversal, and throws
// KeyNotFoundException when the key is genuinely absent. From code-behind that is
// a NATIVE FAST-FAIL waiting to happen — it killed SuggestedMergesSheet on theme
// brushes (TextFillColor*, SubtleFill*), which are resolved per-element via
// {ThemeResource} and are NOT reliably present in Application.Current.Resources.
// These helpers wrap the indexer so a present resource returns UNCHANGED (full
// traversal — no visual regression) and a genuine miss yields a fallback plus a
// one-time warning (so a real missing CUSTOM brush still surfaces in logs instead
// of being silently masked). Do NOT use ResourceDictionary.TryGetValue here: it
// does not traverse merged/theme dictionaries, so it would falsely miss resolvable
// brushes and regress their color. Call on the UI thread (the fallback constructs
// a DispatcherObject on a genuine miss).

using System;
using System.Collections.Concurrent;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

namespace FileID.Services;

public static class ThemeHelper
{
    private static readonly ConcurrentDictionary<string, byte> _warned = new();

    /// <summary>Resolve a Brush resource without ever throwing. Returns the
    /// resolved brush when present, else <paramref name="fallback"/> (or a
    /// transparent brush) and warns once for the key.</summary>
    public static Brush GetBrushSafe(string key, Brush? fallback = null)
    {
        try
        {
            var res = Application.Current?.Resources;
            if (res != null && res[key] is Brush b)
            {
                return b;
            }
        }
        catch (Exception)
        {
            // KeyNotFound / unresolvable — fall through to the fallback.
        }
        WarnOnce(key);
        return fallback ?? new SolidColorBrush(Colors.Transparent);
    }

    /// <summary>Resolve a Style resource without throwing. Returns null on a
    /// miss (assigning null to a FrameworkElement.Style clears it to default).</summary>
    public static Style? GetStyleSafe(string key)
    {
        try
        {
            var res = Application.Current?.Resources;
            if (res != null && res[key] is Style s)
            {
                return s;
            }
        }
        catch (Exception)
        {
            // missing — return null (default style), no crash.
        }
        WarnOnce(key);
        return null;
    }

    private static void WarnOnce(string key)
    {
        if (_warned.TryAdd(key, 0))
        {
            DebugLog.Warn($"[THEME] resource '{key}' not resolvable via Application.Current.Resources; using fallback");
        }
    }
}
