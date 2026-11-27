// Crash-safe resource lookups from code-behind. Application.Current.Resources[key] throws KeyNotFoundException on a genuine miss, which is a native fast-fail in code-behind (it killed SuggestedMergesSheet on theme brushes that are not reliably in Application.Resources).
// These helpers keep the full merged-dictionary traversal and return a fallback plus a one-time warning on a miss. ResourceDictionary.TryGetValue is not used because it skips merged and theme dictionaries.
// Call on the UI thread; the fallback constructs a DispatcherObject.

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
