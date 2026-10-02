using System;
using System.IO;
using System.Runtime.CompilerServices;
using Xunit;

[assembly: CollectionBehavior(DisableTestParallelization = true)]

internal static class TestEnvironment
{
    [System.Diagnostics.CodeAnalysis.SuppressMessage("Usage", "CA2255", Justification = "Set isolated process-wide paths before any app static initializer can write state.")]
    [ModuleInitializer]
    internal static void Initialize()
    {
        var root = Path.Combine(
            Path.GetTempPath(),
            "FileID-App-Tests",
            Environment.ProcessId.ToString());
        Directory.CreateDirectory(root);
        Environment.SetEnvironmentVariable("LOCALAPPDATA", root);
        Environment.SetEnvironmentVariable(
            "FILEID_DB",
            Path.Combine(root, "FileID", "fileid.sqlite"));
    }
}
