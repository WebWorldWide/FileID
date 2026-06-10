// PathRedactor — strips PII from file paths before they're logged.
//
// Mirror of `redactPathForLog(_:)` on macOS (PathRedaction.swift) and
// `redact_path_for_log` in the Rust engine. The semantics:
//
//   C:\Users\adam\photos\trip.jpg  →  ~\photos\trip.jpg
//   /Users/adam/photos/trip.jpg     →  ~/photos/trip.jpg   (cross-platform DB paths)
//
// Replaces the user's home directory prefix with `~`. Username (the most
// directly-identifying segment) goes away. The structure of the rest of
// the path stays so debug logs are still useful.
//
// PRIVACY: every log call site that emits a user file path passes it
// through here first. Reviewed in every PR per shared/docs/PRIVACY.md.

namespace FileID.Services;

internal static class PathRedactor
{
    private static readonly string s_userProfile = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
    private static readonly string s_userProfileNormalized = NormalizeForCompare(s_userProfile);
    private static readonly string s_stateRootNormalized = NormalizeForCompare(AppPaths.Root);

    public static string Redact(string? path)
    {
        if (string.IsNullOrEmpty(path))
        {
            return "<null>";
        }
        var normalized = NormalizeForCompare(path);
        // FileID's own state tree (%LOCALAPPDATA%\FileID\…) carries no
        // user-chosen names — pass it through verbatim so model/db/log
        // paths stay debuggable. Parity with the Rust engine
        // (platform.rs root() boundary) and macOS (fileIDStateRoot).
        if (normalized.StartsWith(s_stateRootNormalized, StringComparison.OrdinalIgnoreCase)
            && (normalized.Length == s_stateRootNormalized.Length
                || normalized[s_stateRootNormalized.Length] == '\\'))
        {
            return path;
        }
        // Require an exact-home or separator boundary after the prefix —
        // otherwise `C:\Users\bob` would match `C:\Users\bobby\…` and leak a
        // DIFFERENT user's name as `~by\…`.
        if (normalized.StartsWith(s_userProfileNormalized, StringComparison.OrdinalIgnoreCase)
            && (normalized.Length == s_userProfileNormalized.Length
                || normalized[s_userProfileNormalized.Length] == '\\'))
        {
            // Preserve original separator style by counting from the original path.
            var tail = path[s_userProfile.Length..];
            return "~" + tail;
        }
        // Cross-platform DB rows: also catch the macOS shape `/Users/<name>/...`.
        if (path.StartsWith("/Users/", StringComparison.Ordinal))
        {
            var slashAfterUser = path.IndexOf('/', "/Users/".Length);
            if (slashAfterUser > 0)
            {
                return "~" + path[slashAfterUser..];
            }
            return "~/<home>";
        }
        return path;
    }

    private static string NormalizeForCompare(string p) =>
        p.Replace('/', '\\').TrimEnd('\\').ToLowerInvariant();
}
