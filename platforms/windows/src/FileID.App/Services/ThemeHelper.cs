// ThemeHelper — crash-safe resource lookups from code-behind.
// WHY: `Application.Current.Resources[key]` (the ResourceDictionary INDEXER) does a full merged-dictionary + active-theme-dictionary traversal and throws KeyNotFoundException when the key is genuinely absent. In code-behind that is a NATIVE FAST-FAIL waiting to happen (it killed SuggestedMergesSheet on theme brushes like TextFillColor*/SubtleFill*, resolved per-element via {ThemeResource}, that are NOT reliably present in Application.Current.Resources). These helpers wrap the indexer so a present resource returns UNCHANGED (full traversal, no visual regression) and a genuine miss yields the fallback plus a one-time warning (so a real missing CUSTOM brush still surfaces in logs). Do NOT use ResourceDictionary.TryGetValue here: it does not traverse merged/theme dictionaries, so it would falsely miss resolvable brushes and regress color.
// Call on the UI thread (the fallback constructs a DispatcherObject on a genuine miss).

using System;
using System.Collections.Concurrent;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;

namespace FileID.Services;

public static class ThemeHelper
{
    private static readonly ConcurrentDictionary<string, byte> _warned = new();

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
        }
        if (FindInThemeDictionaries(key) is Brush themed)
        {
            return themed;
        }
        WarnOnce(key);
        return fallback ?? new SolidColorBrush(Colors.Transparent);
    }

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
        }
        WarnOnce(key);
        return null;
    }

    /// <summary>Second chance for keys that live only in a merged dictionary's ThemeDictionaries (Theme.xaml's contrast-sensitive tokens): the active high-contrast entry when the OS is in high contrast, else Default.</summary>
    private static object? FindInThemeDictionaries(string key)
    {
        try
        {
            var res = Application.Current?.Resources;
            if (res is null) return null;
            bool highContrast = false;
            try { highContrast = new Windows.UI.ViewManagement.AccessibilitySettings().HighContrast; }
            catch (Exception) { /* default theme */ }
            var themeKey = highContrast ? "HighContrast" : "Default";
            foreach (var merged in res.MergedDictionaries)
            {
                if (merged.ThemeDictionaries.TryGetValue(themeKey, out var themed)
                    && themed is ResourceDictionary dictionary
                    && dictionary.TryGetValue(key, out var value))
                {
                    return value;
                }
            }
        }
        catch (Exception)
        {
        }
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
