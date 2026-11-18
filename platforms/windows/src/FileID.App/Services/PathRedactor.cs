// PathRedactor — strips PII from file paths before they're logged.
// PRIVACY: every log call site that emits a user file path passes it through here first. Reviewed in every PR per shared/docs/PRIVACY.md.

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
        if (normalized.StartsWith(s_stateRootNormalized, StringComparison.OrdinalIgnoreCase)
            && (normalized.Length == s_stateRootNormalized.Length
                || normalized[s_stateRootNormalized.Length] == '\\'))
        {
            return path;
        }
        // Require an exact-home or separator boundary after the prefix — otherwise `C:\Users\bob` would match `C:\Users\bobby\…` and leak a DIFFERENT user's name as `~by\…`.
        if (normalized.StartsWith(s_userProfileNormalized, StringComparison.OrdinalIgnoreCase)
            && (normalized.Length == s_userProfileNormalized.Length
                || normalized[s_userProfileNormalized.Length] == '\\'))
        {
            var tail = path[s_userProfile.Length..];
            return "~" + tail;
        }
        if (path.StartsWith("/Users/", StringComparison.Ordinal))
        {
            var slashAfterUser = path.IndexOf('/', "/Users/".Length);
            if (slashAfterUser > 0)
            {
                return "~" + path[slashAfterUser..];
            }
            return "~/<home>";
        }
        if (path.StartsWith("/Volumes/", StringComparison.Ordinal))
        {
            var parts = path.Split('/', StringSplitOptions.RemoveEmptyEntries);
            var keep = parts.Length >= 2 ? parts[^2..] : parts;
            return "…/" + string.Join('/', keep);
        }
        return path;
    }

    private static string NormalizeForCompare(string p) =>
        p.Replace('/', '\\').TrimEnd('\\').ToLowerInvariant();
}
