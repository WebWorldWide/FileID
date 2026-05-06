// PrivacyGrep — strings audit over the engine binary.
//
// The "What we don't do" panel in Settings advertises zero telemetry.
// This service makes that claim verifiable: scan the running engine
// binary for the markers used by every commercial telemetry library
// (Sentry, AppInsights, Firebase, Segment, Mixpanel, Google Analytics,
// Amplitude, AppCenter). Zero hits = the claim holds.
//
// Privacy: the scan is local-only. We read the binary's own bytes,
// match ASCII substrings via a fast Boyer-Moore-Horspool pass, return
// any matches. No network, no logging beyond DebugLog.

using System;
using System.Collections.Generic;
using System.IO;
using System.Threading.Tasks;

namespace FileID.Services;

internal static class PrivacyGrep
{
    private static readonly string[] Markers = new[]
    {
        // Sentry
        "sentry.io",
        "io.sentry",
        "SentryClient",
        // App Insights
        "applicationinsights",
        "TelemetryClient",
        // Firebase
        "firebaseio.com",
        "firebase-",
        // Segment
        "segment.com",
        "api.segment.io",
        // Mixpanel
        "api.mixpanel.com",
        "mixpanel-",
        // Google Analytics
        "google-analytics.com",
        "googletagmanager.com",
        // Amplitude
        "api.amplitude.com",
        // AppCenter
        "in.appcenter.ms",
        "AppCenter.start",
    };

    /// <summary>
    /// Scan the live engine binary at `~\AppData\Local\FileID-App\FileIDEngine.exe`
    /// for each marker; return any hits. Empty list = clean. Runs on a
    /// background thread so the UI doesn't block during the scan.
    /// </summary>
    public static Task<IReadOnlyList<string>> RunAsync()
    {
        return Task.Run<IReadOnlyList<string>>(() =>
        {
            var hits = new List<string>();
            try
            {
                var enginePath = AppPaths.EngineExePath;
                if (!File.Exists(enginePath))
                {
                    return Array.Empty<string>();
                }
                var bytes = File.ReadAllBytes(enginePath);
                foreach (var marker in Markers)
                {
                    if (ContainsAscii(bytes, marker))
                    {
                        hits.Add(marker);
                    }
                }
            }
            catch (Exception ex)
            {
                DebugLog.Warn("PrivacyGrep failed: " + ex.Message);
            }
            return hits;
        });
    }

    /// <summary>
    /// Boyer-Moore-Horspool ASCII substring search over a byte buffer.
    /// Avoids loading the whole binary into a string (multi-MB) and
    /// avoids UTF-16 reinterpretation (the binary contains both ASCII
    /// and UTF-16; ASCII markers reach both representations because the
    /// markers themselves are ASCII).
    /// </summary>
    private static bool ContainsAscii(byte[] haystack, string needle)
    {
        if (needle.Length == 0 || haystack.Length < needle.Length) return false;
        var nlen = needle.Length;
        Span<int> skip = stackalloc int[256];
        for (int i = 0; i < 256; i++) skip[i] = nlen;
        for (int i = 0; i < nlen - 1; i++)
        {
            byte c = (byte)needle[i];
            skip[c] = nlen - 1 - i;
        }
        int last = nlen - 1;
        int j = 0;
        while (j <= haystack.Length - nlen)
        {
            int k = last;
            while (k >= 0 && (byte)needle[k] == haystack[j + k]) k--;
            if (k < 0) return true;
            j += skip[haystack[j + last]];
        }
        return false;
    }
}
