using System;
using System.IO;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

#if DEBUG
public sealed class IsolatedInstanceTests
{
    [Fact]
    public void TestProcessUsesIsolatedAppData()
    {
        var expectedRoot = Path.Combine(
            Path.GetTempPath(),
            "FileID-App-Tests",
            Environment.ProcessId.ToString(),
            "FileID");

        Assert.Equal(expectedRoot, AppPaths.Root);
        Assert.Equal(Path.Combine(expectedRoot, "fileid.sqlite"), AppPaths.DbPath);
    }

    [Fact]
    public void IsolatedDebugInstanceRequiresSeparateDatabaseAndAppData()
    {
        Assert.Throws<InvalidOperationException>(
            () => Program.ResolveInstanceMutexName("ui-test", null, @"D:\Isolated"));
        Assert.Throws<InvalidOperationException>(
            () => Program.ResolveInstanceMutexName("ui-test", @"D:\test.sqlite", null));
    }

    [Fact]
    public void IsolatedDebugInstanceUsesStableDistinctMutex()
    {
        var first = Program.ResolveInstanceMutexName(
            "ui-test",
            @"D:\test.sqlite",
            @"D:\Isolated");
        var repeated = Program.ResolveInstanceMutexName(
            "ui-test",
            @"D:\test.sqlite",
            @"D:\Isolated");
        var other = Program.ResolveInstanceMutexName(
            "other-test",
            @"D:\test.sqlite",
            @"D:\Isolated");

        Assert.Equal(first, repeated);
        Assert.NotEqual(first, other);
        Assert.StartsWith(@"Local\FileID-Test-", first, StringComparison.Ordinal);
    }
}
#endif
