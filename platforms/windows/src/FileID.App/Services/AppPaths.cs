// Mirrors the Rust engine's `paths.rs` so the app's read-only connection finds the engine's database.

using System.IO;

namespace FileID.Services;

internal static class AppPaths
{
    public static string Root { get; } = ResolveRoot();

    public static string DbPath => ResolveDbPath(Root, Environment.GetEnvironmentVariable("FILEID_DB"));
    public static string LogsDir => Path.Combine(Root, "logs");
    public static string ModelsDir => ResolveModelsDir(Root, Environment.GetEnvironmentVariable("FILEID_MODELS_DIR"));
    public static string HuggingFaceDir => ResolveHuggingFaceDir(ModelsDir);
    public static string ThumbsDir => Path.Combine(Root, "thumbs.cache");
    public static string FacesDir => Path.Combine(Root, "face_crops");
    public static string SettingsPath => Path.Combine(Root, "app-settings.json");
    public static string AppLogPath => Path.Combine(LogsDir, "app.log");

    public static string EngineExePath
    {
        get
        {
            var beside = Path.Combine(AppContext.BaseDirectory, "FileIDEngine.exe");
            if (File.Exists(beside))
            {
                return beside;
            }
            // Dev fallback: the engine's cargo target directory for this architecture.
            var arch = System.Runtime.InteropServices.RuntimeInformation.ProcessArchitecture;
            string triple = arch == System.Runtime.InteropServices.Architecture.Arm64
                ? "aarch64-pc-windows-msvc"
                : "x86_64-pc-windows-msvc";

            var devRelease = Path.Combine(AppContext.BaseDirectory,
                "..", "..", "..", "..", "..", "engine", "target", triple, "release", "FileIDEngine.exe");
            if (File.Exists(devRelease))
            {
                return Path.GetFullPath(devRelease);
            }
            var devDebug = Path.Combine(AppContext.BaseDirectory,
                "..", "..", "..", "..", "..", "engine", "target", triple, "debug", "FileIDEngine.exe");
            if (File.Exists(devDebug))
            {
                return Path.GetFullPath(devDebug);
            }
            return beside;
        }
    }

    // DebugLog.Write calls EnsureDirectories per line; the guard avoids a handful of filesystem calls on each.
    private static volatile bool _directoriesEnsured;

    public static void EnsureDirectories()
    {
        if (_directoriesEnsured) return;
        Directory.CreateDirectory(Root);
        Directory.CreateDirectory(LogsDir);
        Directory.CreateDirectory(ModelsDir);
        Directory.CreateDirectory(HuggingFaceDir);
        Directory.CreateDirectory(ThumbsDir);
        Directory.CreateDirectory(FacesDir);
        _directoriesEnsured = true;
    }

    internal static string ResolveRoot(
        string? localAppDataEnvironment,
        string? userProfileEnvironment,
        string knownLocalAppData,
        string knownUserProfile)
    {
        var localAppData = !string.IsNullOrWhiteSpace(localAppDataEnvironment)
            ? localAppDataEnvironment
            : !string.IsNullOrWhiteSpace(userProfileEnvironment)
                ? Path.Combine(userProfileEnvironment, "AppData", "Local")
                : !string.IsNullOrWhiteSpace(knownLocalAppData)
                    ? knownLocalAppData
                    : Path.Combine(knownUserProfile, "AppData", "Local");
        return Path.Combine(localAppData, "FileID");
    }

    internal static string ResolveDbPath(string root, string? overridePath) =>
        string.IsNullOrWhiteSpace(overridePath) ? Path.Combine(root, "fileid.sqlite") : overridePath;

    internal static string ResolveModelsDir(string root, string? overridePath) =>
        string.IsNullOrWhiteSpace(overridePath) ? Path.Combine(root, "Models") : overridePath;

    internal static string ResolveHuggingFaceDir(string modelsDir) =>
        Path.Combine(modelsDir, "HuggingFace");

    private static string ResolveRoot() => ResolveRoot(
        Environment.GetEnvironmentVariable("LOCALAPPDATA"),
        Environment.GetEnvironmentVariable("USERPROFILE"),
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        Environment.GetFolderPath(Environment.SpecialFolder.UserProfile));
}
