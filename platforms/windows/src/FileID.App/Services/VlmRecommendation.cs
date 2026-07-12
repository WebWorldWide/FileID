using System;
using System.Collections.Generic;
using System.Linq;
using System.IO;
using System.Runtime.InteropServices;
using FileID.ViewModels;

namespace FileID.Services;

internal readonly record struct VlmHardwareProfile(
    double TotalRamGb,
    double AvailableRamGb,
    ulong DedicatedVramMb,
    string GpuVendor,
    Architecture Architecture,
    ulong? FreeDiskBytes);

internal readonly record struct VlmRecommendationResult(
    string ModelKind,
    string DisplayName,
    ulong DownloadBytes,
    string Reason,
    bool HasEnoughMemory,
    bool HasEnoughDisk);

internal static class VlmRecommendation
{
    internal const string Gemma = "gemma_3_4b";
    internal const string Qwen = "qwen2_5_vl_7b";
    internal const string Mistral = "mistral_small_3_2";

    internal static readonly string[] SupportedKinds = [Mistral, Qwen, Gemma];

    internal static VlmHardwareProfile CurrentProfile()
    {
        var info = EngineClient.Instance.Info;
        var total = info?.PhysicalMemoryGB ?? 0;
        if (total <= 0 && info?.Hardware is { RamTotalMb: > 0 } hwTotal)
        {
            total = hwTotal.RamTotalMb / 1024.0;
        }
        var available = info?.Hardware is { RamAvailableMb: > 0 } hwAvailable
            ? hwAvailable.RamAvailableMb / 1024.0
            : total;
        var vram = info?.Hardware?.VramMb ?? 0;
        var vendor = info?.Hardware?.GpuVendor ?? "none";
        return new VlmHardwareProfile(
            total,
            available,
            vram,
            vendor,
            RuntimeInformation.ProcessArchitecture,
            FreeBytesFor(AppPaths.ModelsDir));
    }

    internal static VlmRecommendationResult Recommend(VlmHardwareProfile profile)
    {
        var memoryOrder = PreferredOrder(profile);
        var memoryChoice = memoryOrder.FirstOrDefault(kind => CanRun(kind, profile)) ?? Gemma;
        var choice = memoryOrder.FirstOrDefault(kind =>
            CanRun(kind, profile) && HasDiskFor(kind, profile.FreeDiskBytes));
        choice ??= memoryChoice;

        var hasMemory = CanRun(choice, profile);
        var hasDisk = HasDiskFor(choice, profile.FreeDiskBytes);
        var display = DisplayName(choice);
        var reason = BuildReason(display, profile, hasMemory, hasDisk);
        return new VlmRecommendationResult(
            choice,
            display,
            DownloadBytes(choice),
            reason,
            hasMemory,
            hasDisk);
    }

    internal static bool CanRun(string kind, VlmHardwareProfile profile)
    {
        if (profile.Architecture == Architecture.Arm64 && kind != Gemma)
        {
            return false;
        }

        var minimumTotal = kind switch
        {
            Mistral => 23.5,
            Qwen => 11.5,
            Gemma => 7.5,
            _ => double.MaxValue,
        };
        if (profile.TotalRamGb < minimumTotal)
        {
            return false;
        }

        var reserve = profile.TotalRamGb switch
        {
            <= 10 => 2.0,
            <= 20 => 4.0,
            _ => 6.0,
        };
        var usableSystem = Math.Max(0, profile.TotalRamGb - reserve);
        if (profile.AvailableRamGb > 0)
        {
            usableSystem = Math.Min(usableSystem, Math.Max(0, profile.AvailableRamGb - 1.5));
        }

        var vramGb = profile.DedicatedVramMb / 1024.0;
        var usableVram = HasDedicatedAccelerator(profile)
            ? Math.Max(0, vramGb - 1.5) * 0.8
            : 0;
        var workingSet = WorkingSetGb(kind);
        if (workingSet <= 0) return false;
        return usableSystem + usableVram >= workingSet;
    }

    internal static bool HasDiskFor(string kind, ulong? freeDiskBytes)
        => freeDiskBytes is null || freeDiskBytes.Value >= RequiredFreeBytes(kind);

    internal static ulong RequiredFreeBytes(string kind)
    {
        const ulong stagingHeadroom = 512UL * 1024 * 1024;
        return checked(DownloadBytes(kind) * 2 + stagingHeadroom);
    }

    internal static ulong DownloadBytes(string kind) => kind switch
    {
        Mistral => 15_178_000_000UL,
        Qwen => 6_100_000_000UL,
        Gemma => 3_351_251_104UL,
        _ => 0,
    };

    internal static double WorkingSetGb(string kind) => kind switch
    {
        Mistral => 16.0,
        Qwen => 7.0,
        Gemma => 4.5,
        _ => 0,
    };

    internal static string DisplayName(string kind) => kind switch
    {
        Mistral => "Mistral-Small 3.2 24B",
        Qwen => "Qwen2.5-VL 7B",
        Gemma => "Gemma 3 4B",
        _ => kind,
    };

    internal static bool IsSupported(string? kind)
        => kind is Gemma or Qwen or Mistral;

    internal static string? ResolveInstalledSelection(
        string? persistedKind,
        string recommendedKind,
        VlmHardwareProfile profile,
        Func<string, bool> isInstalled)
    {
        var candidates = new List<string>(5);
        if (IsSupported(persistedKind)) candidates.Add(persistedKind!);
        if (IsSupported(recommendedKind)) candidates.Add(recommendedKind);
        candidates.AddRange(SupportedKinds);

        return candidates
            .Distinct(StringComparer.Ordinal)
            .FirstOrDefault(kind => isInstalled(kind) && CanRun(kind, profile));
    }

    private static string[] PreferredOrder(VlmHardwareProfile profile)
    {
        if (profile.Architecture == Architecture.Arm64)
        {
            return [Gemma];
        }

        var vramGb = profile.DedicatedVramMb / 1024.0;
        var highEndGpu = HasDedicatedAccelerator(profile) && vramGb >= 12.0;
        if ((profile.TotalRamGb >= 29.5 && highEndGpu) || profile.TotalRamGb >= 47.5)
        {
            return [Mistral, Qwen, Gemma];
        }
        if (profile.TotalRamGb >= 15.0
            || (profile.TotalRamGb >= 11.5 && HasDedicatedAccelerator(profile) && vramGb >= 8.0))
        {
            return [Qwen, Gemma];
        }
        return [Gemma];
    }

    private static bool HasDedicatedAccelerator(VlmHardwareProfile profile)
    {
        var vendor = profile.GpuVendor.ToLowerInvariant();
        return profile.DedicatedVramMb > 0
            && vendor is "nvidia" or "amd" or "intel" or "qualcomm";
    }

    private static string BuildReason(
        string display,
        VlmHardwareProfile profile,
        bool hasMemory,
        bool hasDisk)
    {
        if (!hasMemory)
        {
            return $"{display} is the lightest option, but this PC has too little available memory to run a vision model safely.";
        }
        if (!hasDisk)
        {
            return $"{display} fits this PC's memory, but the models drive needs more free space before installation.";
        }
        var vram = profile.DedicatedVramMb >= 1024
            ? $" and {profile.DedicatedVramMb / 1024.0:0.#} GB dedicated VRAM"
            : string.Empty;
        var arch = profile.Architecture == Architecture.Arm64 ? " ARM64" : string.Empty;
        return $"Recommended for this{arch} PC's {profile.TotalRamGb:0.#} GB RAM{vram}.";
    }

    private static ulong? FreeBytesFor(string path)
    {
        try
        {
            var full = Path.GetFullPath(path);
            var root = Path.GetPathRoot(full);
            if (string.IsNullOrWhiteSpace(root)) return null;
            var drive = new DriveInfo(root);
            return drive.IsReady ? (ulong)drive.AvailableFreeSpace : null;
        }
        catch
        {
            return null;
        }
    }
}

internal static class AcceleratorActivationPolicy
{
    internal static string? ExpectedProvider(string? gpuVendor, Architecture architecture)
    {
        if (architecture != Architecture.X64) return null;
        return (gpuVendor ?? string.Empty).ToLowerInvariant() switch
        {
            "nvidia" => "cuda",
            "intel" => "openvino",
            _ => null,
        };
    }

    internal static bool RestartRequired(
        string? gpuVendor,
        string? activeProvider,
        Architecture architecture,
        bool installComplete,
        string? providerOverride = null)
    {
        if (!installComplete) return false;
        var expected = ExpectedProvider(gpuVendor, architecture);
        var hasConflictingOverride = !string.IsNullOrWhiteSpace(providerOverride)
            && !string.Equals(providerOverride, "auto", StringComparison.OrdinalIgnoreCase)
            && !string.Equals(providerOverride, expected, StringComparison.OrdinalIgnoreCase);
        return expected is not null
            && !hasConflictingOverride
            && !string.Equals(activeProvider, expected, StringComparison.OrdinalIgnoreCase);
    }
}
