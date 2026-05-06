// AppPaths — C# mirror of the Rust engine's `paths.rs`.
//
// All app-side filesystem locations live under %LOCALAPPDATA%\FileID\.
// Engine + app share the same directory layout so a DB written by the
// engine is reachable by the app's read-only connection without any
// path-translation logic.

using System.IO;

namespace FileID.Services;

internal static class AppPaths
{
    /// <summary>%LOCALAPPDATA%\FileID\</summary>
    public static string Root { get; } = ResolveRoot();

    public static string DbPath        => Path.Combine(Root, "fileid.sqlite");
    public static string LogsDir       => Path.Combine(Root, "logs");
    public static string ModelsDir     => Path.Combine(Root, "Models");
    public static string HuggingFaceDir => Path.Combine(Root, "Models", "HuggingFace");
    public static string ThumbsDir     => Path.Combine(Root, "thumbs.cache");
    public static string FacesDir      => Path.Combine(Root, "face_crops");
    public static string SettingsPath  => Path.Combine(Root, "app-settings.json");
    public static string AppLogPath    => Path.Combine(LogsDir, "app.log");

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

    public static void EnsureDirectories()
    {
        Directory.CreateDirectory(Root);
        Directory.CreateDirectory(LogsDir);
        Directory.CreateDirectory(ModelsDir);
        Directory.CreateDirectory(HuggingFaceDir);
        Directory.CreateDirectory(ThumbsDir);
        Directory.CreateDirectory(FacesDir);
    }

    private static string ResolveRoot()
    {
        var localAppData = Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData);
        if (string.IsNullOrEmpty(localAppData))
        {
            // Fallback: user-profile path (rare on Windows desktop SKUs).
            localAppData = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.UserProfile),
                "AppData", "Local");
        }
        return Path.Combine(localAppData, "FileID");
    }
}
