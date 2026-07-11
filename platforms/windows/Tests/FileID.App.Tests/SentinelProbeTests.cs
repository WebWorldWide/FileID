using System;
using System.IO;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

// Regression for the CUDA-pack prewarm loop: the engine writes hashed
// `{id}-{hash}.installed` sentinels, but SettingsView + the auto-installers
// probed only the flat `{id}.installed` name — so an installed pack read as
// missing and its prewarm re-dispatched on every Settings load / engine
// Ready. The shared probe must latch on BOTH forms.
public sealed class SentinelProbeTests : IDisposable
{
    private readonly string _dir;

    public SentinelProbeTests()
    {
        _dir = Path.Combine(Path.GetTempPath(), "fileid-sentinel-tests-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(_dir);
    }

    public void Dispose()
    {
        try { Directory.Delete(_dir, recursive: true); } catch { }
    }

    private void Touch(string name) =>
        File.WriteAllText(Path.Combine(_dir, name), string.Empty);

    [Fact]
    public void FlatSentinel_ReadsInstalled()
    {
        Touch("llama_runtime_cuda_x64.installed");
        Assert.True(SentinelProbe.InstalledIn(_dir, "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void HashedSentinel_ReadsInstalled()
    {
        Touch("llama_runtime_cuda_x64-c380324c262d1b84.installed");
        Assert.True(SentinelProbe.InstalledIn(_dir, "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void NoSentinel_ReadsNotInstalled()
    {
        Assert.False(SentinelProbe.InstalledIn(_dir, "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void MissingDirectory_ReadsNotInstalled()
    {
        Assert.False(SentinelProbe.InstalledIn(Path.Combine(_dir, "nope"), "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void HashedSentinel_DoesNotMatchOtherKinds()
    {
        Touch("llama_runtime_x64-bbc12079e7ab29b1.installed");
        Assert.True(SentinelProbe.InstalledIn(_dir, "llama_runtime_x64"));
        Assert.False(SentinelProbe.InstalledIn(_dir, "llama_runtime_cuda_x64"));
    }

    [Fact]
    public void IdPrefix_DoesNotMatchLongerId()
    {
        Touch("arcface_xl-0011223344556677.installed");
        Assert.False(SentinelProbe.InstalledIn(_dir, "arcface"));
    }

    [Fact]
    public void UnreadableDir_ReadsNotInstalled()
    {
        Assert.False(SentinelProbe.InstalledIn("\0invalid\0", "llama_runtime_cuda_x64"));
    }
}
