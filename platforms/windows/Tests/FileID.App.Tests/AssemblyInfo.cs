using System;
using System.IO;
using System.Runtime.CompilerServices;
using Xunit;

[assembly: CollectionBehavior(DisableTestParallelization = true)]

internal static class TestEnvironment
{
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
