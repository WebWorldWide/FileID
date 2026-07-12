using System.Collections.Generic;
using System.Runtime.InteropServices;
using FileID.Services;
using Xunit;

namespace FileID.App.Tests;

public sealed class VlmRecommendationTests
{
    private const ulong PlentyOfDisk = 200UL * 1024 * 1024 * 1024;

    [Fact]
    public void Nominal32GbWithHighEndNvidiaGpu_RecommendsMistral()
    {
        var profile = new VlmHardwareProfile(
            TotalRamGb: 30.9,
            AvailableRamGb: 13.5,
            DedicatedVramMb: 15_977,
            GpuVendor: "nvidia",
            Architecture: Architecture.X64,
            FreeDiskBytes: PlentyOfDisk);

        var result = VlmRecommendation.Recommend(profile);

        Assert.Equal(VlmRecommendation.Mistral, result.ModelKind);
        Assert.True(result.HasEnoughMemory);
        Assert.True(result.HasEnoughDisk);
    }

    [Fact]
    public void HighVramDoesNotReplaceRequiredSystemRam()
    {
        var profile = new VlmHardwareProfile(
            TotalRamGb: 8,
            AvailableRamGb: 6,
            DedicatedVramMb: 16 * 1024,
            GpuVendor: "nvidia",
            Architecture: Architecture.X64,
            FreeDiskBytes: PlentyOfDisk);

        var result = VlmRecommendation.Recommend(profile);

        Assert.Equal(VlmRecommendation.Gemma, result.ModelKind);
        Assert.False(VlmRecommendation.CanRun(VlmRecommendation.Qwen, profile));
        Assert.False(VlmRecommendation.CanRun(VlmRecommendation.Mistral, profile));
    }

    [Fact]
    public void Nominal16GbWithoutDedicatedGpu_RecommendsQwen()
    {
        var profile = new VlmHardwareProfile(
            TotalRamGb: 15.5,
            AvailableRamGb: 13,
            DedicatedVramMb: 0,
            GpuVendor: "none",
            Architecture: Architecture.X64,
            FreeDiskBytes: PlentyOfDisk);

        Assert.Equal(VlmRecommendation.Qwen, VlmRecommendation.Recommend(profile).ModelKind);
    }

    [Fact]
    public void DiskPressureDowngradesMistralBeforeDownloadStarts()
    {
        var qwenOnlyDisk = VlmRecommendation.RequiredFreeBytes(VlmRecommendation.Qwen) + 1;
        Assert.True(qwenOnlyDisk < VlmRecommendation.RequiredFreeBytes(VlmRecommendation.Mistral));
        var profile = new VlmHardwareProfile(
            TotalRamGb: 30.9,
            AvailableRamGb: 24,
            DedicatedVramMb: 15_977,
            GpuVendor: "nvidia",
            Architecture: Architecture.X64,
            FreeDiskBytes: qwenOnlyDisk);

        var result = VlmRecommendation.Recommend(profile);

        Assert.Equal(VlmRecommendation.Qwen, result.ModelKind);
        Assert.True(result.HasEnoughDisk);
    }

    [Fact]
    public void Arm64UsesTheEmulationSafeLightweightModel()
    {
        var profile = new VlmHardwareProfile(
            TotalRamGb: 32,
            AvailableRamGb: 28,
            DedicatedVramMb: 12 * 1024,
            GpuVendor: "qualcomm",
            Architecture: Architecture.Arm64,
            FreeDiskBytes: PlentyOfDisk);

        Assert.Equal(VlmRecommendation.Gemma, VlmRecommendation.Recommend(profile).ModelKind);
    }

    [Fact]
    public void InstalledSelectionPrefersAnExplicitSafeChoice()
    {
        var profile = new VlmHardwareProfile(
            TotalRamGb: 30.9,
            AvailableRamGb: 13.5,
            DedicatedVramMb: 15_977,
            GpuVendor: "nvidia",
            Architecture: Architecture.X64,
            FreeDiskBytes: PlentyOfDisk);
        var installed = new HashSet<string>
        {
            VlmRecommendation.Gemma,
            VlmRecommendation.Mistral,
        };

        var selected = VlmRecommendation.ResolveInstalledSelection(
            VlmRecommendation.Gemma,
            VlmRecommendation.Mistral,
            profile,
            installed.Contains);

        Assert.Equal(VlmRecommendation.Gemma, selected);
    }

    [Fact]
    public void InstalledSelectionFallsBackFromMissingPersistedModel()
    {
        var profile = new VlmHardwareProfile(
            TotalRamGb: 30.9,
            AvailableRamGb: 13.5,
            DedicatedVramMb: 15_977,
            GpuVendor: "nvidia",
            Architecture: Architecture.X64,
            FreeDiskBytes: PlentyOfDisk);

        var selected = VlmRecommendation.ResolveInstalledSelection(
            VlmRecommendation.Qwen,
            VlmRecommendation.Mistral,
            profile,
            kind => kind == VlmRecommendation.Gemma);

        Assert.Equal(VlmRecommendation.Gemma, selected);
    }

    [Fact]
    public void InstalledSelectionNeverReturnsAnUnsafeModel()
    {
        var profile = new VlmHardwareProfile(
            TotalRamGb: 8,
            AvailableRamGb: 5,
            DedicatedVramMb: 0,
            GpuVendor: "none",
            Architecture: Architecture.X64,
            FreeDiskBytes: PlentyOfDisk);

        var selected = VlmRecommendation.ResolveInstalledSelection(
            VlmRecommendation.Mistral,
            VlmRecommendation.Mistral,
            profile,
            kind => kind == VlmRecommendation.Mistral);

        Assert.Null(selected);
    }

    [Fact]
    public void InstalledNvidiaPackOnDirectMlRequiresRestart()
    {
        Assert.True(AcceleratorActivationPolicy.RestartRequired(
            gpuVendor: "nvidia",
            activeProvider: "directml",
            architecture: Architecture.X64,
            installComplete: true));
        Assert.False(AcceleratorActivationPolicy.RestartRequired(
            gpuVendor: "nvidia",
            activeProvider: "cuda",
            architecture: Architecture.X64,
            installComplete: true));
        Assert.False(AcceleratorActivationPolicy.RestartRequired(
            gpuVendor: "nvidia",
            activeProvider: "directml",
            architecture: Architecture.X64,
            installComplete: false));
        Assert.False(AcceleratorActivationPolicy.RestartRequired(
            gpuVendor: "nvidia",
            activeProvider: "cpu",
            architecture: Architecture.X64,
            installComplete: true,
            providerOverride: "cpu"));
    }
}
