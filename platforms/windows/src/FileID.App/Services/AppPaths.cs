// AppPaths — C# mirror of the Rust engine's `paths.rs`.
//
// Engine + app share the same environment-first directory resolution so
// isolated runs never split the writer DB/model tree from the app's readers.

using System.IO;

namespace FileID.Services;

internal static class AppPaths
{
    /// <summary>%LOCALAPPDATA%\FileID\ unless the process environment overrides it.</summary>
    public static string Root { get; } = ResolveRoot();

    public static string DbPath { get; } = ResolveDbPath(Root, Environment.GetEnvironmentVariable("FILEID_DB"));
    public static string LogsDir => Path.Combine(Root, "logs");
    public static string ModelsDir { get; } = ResolveModelsDir(Root, Environment.GetEnvironmentVariable("FILEID_MODELS_DIR"));
    public static string HuggingFaceDir => ResolveHuggingFaceDir(ModelsDir);
    public static string ThumbsDir => Path.Combine(Root, "thumbs.cache");
    public static string FacesDir => Path.Combine(Root, "face_crops");
    public static string SettingsPath => Path.Combine(Root, "app-settings.json");
    public static string AppLogPath => Path.Combine(LogsDir, "app.log");

    /// <summary>
    /// Engine binary path. Looks beside the app first (ship layout where
    /// FileID.exe and FileIDEngine.exe sit in the same install dir) and
    /// falls back to the dev build location for `dotnet run` workflows.
    /// </summary>
    public static string EngineExePath
    {
        get
        {
            var beside = Path.Combine(AppContext.BaseDirectory, "FileIDEngine.exe");
            if (File.Exists(beside))
            {
                return beside;
            }
            // Dev fallback: ../../engine/target/{x86_64,aarch64}-pc-windows-msvc/release/FileIDEngine.exe
            // The platform string varies by target triple; we try both.
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

    // Run-once guard. The app's directory tree does not vanish mid-session, so
    // the six CreateDirectory syscalls only need to happen the first time. This
    // matters most for the DebugLog.Write hot path, which calls EnsureDirectories
    // per line (twice per inbound IPC event via the [APPLY:N] enter/exit lines):
    // without this guard each log line paid ~6 fs syscalls.
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

    private static string ResolveRoot()
    {
        return ResolveRoot(
            Environment.GetEnvironmentVariable("LOCALAPPDATA"),
            Environment.GetEnvironmentVariable("USERPROFILE"),
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            Environment.GetFolderPath(Environment.SpecialFolder.UserProfile));
    }

    internal static string ResolveRoot(
        string? localAppDataOverride,
        string? userProfileOverride,
        string? knownLocalAppData,
        string? knownUserProfile)
    {
        if (!string.IsNullOrWhiteSpace(localAppDataOverride))
            return Path.Combine(localAppDataOverride, "FileID");
        if (!string.IsNullOrWhiteSpace(userProfileOverride))
            return Path.Combine(userProfileOverride, "AppData", "Local", "FileID");
        if (!string.IsNullOrWhiteSpace(knownLocalAppData))
            return Path.Combine(knownLocalAppData, "FileID");
        if (!string.IsNullOrWhiteSpace(knownUserProfile))
            return Path.Combine(knownUserProfile, "AppData", "Local", "FileID");
        throw new InvalidOperationException("Could not resolve %LOCALAPPDATA% or %USERPROFILE% for FileID state.");
    }

    internal static string ResolveDbPath(string root, string? overridePath)
        => string.IsNullOrWhiteSpace(overridePath) ? Path.Combine(root, "fileid.sqlite") : overridePath;

    internal static string ResolveModelsDir(string root, string? overridePath)
        => string.IsNullOrWhiteSpace(overridePath) ? Path.Combine(root, "Models") : overridePath;

    internal static string ResolveHuggingFaceDir(string modelsDir)
        => Path.Combine(modelsDir, "HuggingFace");
}
