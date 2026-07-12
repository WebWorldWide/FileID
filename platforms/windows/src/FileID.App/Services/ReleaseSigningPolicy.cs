using System.Reflection;

namespace FileID.Services;

internal static class ReleaseSigningPolicy
{
    private static readonly IReadOnlyDictionary<string, string?> s_metadata =
        typeof(ReleaseSigningPolicy).Assembly
            .GetCustomAttributes<AssemblyMetadataAttribute>()
            .ToDictionary(attribute => attribute.Key, attribute => attribute.Value, StringComparer.Ordinal);

    public static bool RequireSignedEngine =>
        s_metadata.TryGetValue("FileIDRequireSignedEngine", out var value)
        && string.Equals(value, "true", StringComparison.OrdinalIgnoreCase);

    public static string? ExpectedSignerSubject =>
        s_metadata.TryGetValue("FileIDExpectedSignerSubject", out var value)
            ? value
            : null;

    public static string? ExpectedSignerPublicKeySha256 =>
        s_metadata.TryGetValue("FileIDExpectedSignerPublicKeySha256", out var value)
            ? value
            : null;
}
