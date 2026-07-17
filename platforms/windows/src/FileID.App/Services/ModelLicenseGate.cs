using System.Collections.Generic;
using System.Text.Json;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Services;

internal static class ModelLicenseGate
{
    private sealed record Policy(string Key, string DisplayName, string TermsUrl, string ReviewedAt);

    private static readonly SemaphoreSlim Gate = new(1, 1);
    private static readonly HashSet<string> SessionAcceptances = [];

    internal static string? PolicyKeyForModelKind(string modelKind) => modelKind switch
    {
        "gemma_3_4b" or "gemma_3_12b" or "paligemma_3b" => "Gemma",
        "cudnn_runtime_x64" => "NVIDIA-cuDNN",
        "llama_runtime_cuda_x64" => "NVIDIA-CUDA",
        _ => null,
    };

    internal static async Task<bool> EnsureAcceptedAsync(IEnumerable<string> modelKinds)
    {
        var policies = modelKinds
            .Select(PolicyForModelKind)
            .Where(policy => policy is not null)
            .Cast<Policy>()
            .DistinctBy(policy => policy.Key)
            .ToArray();
        if (policies.Length == 0) return true;

        await Gate.WaitAsync().ConfigureAwait(true);
        try
        {
            foreach (var policy in policies)
            {
                if (IsAccepted(policy)) continue;
                if (!await ShowTermsAsync(policy).ConfigureAwait(true)) return false;
                if (!RememberAcceptance(policy)) return false;
            }
            return true;
        }
        finally
        {
            Gate.Release();
        }
    }

    private static Policy? PolicyForModelKind(string modelKind) => PolicyKeyForModelKind(modelKind) switch
    {
        "Gemma" => new Policy(
            "Gemma",
            "Google Gemma Terms of Use",
            "https://ai.google.dev/gemma/terms",
            "2026-07-16"),
        "NVIDIA-cuDNN" => new Policy(
            "NVIDIA-cuDNN",
            "NVIDIA cuDNN Software License Agreement",
            "https://docs.nvidia.com/deeplearning/cudnn/latest/reference/eula.html",
            "2026-07-16"),
        "NVIDIA-CUDA" => new Policy(
            "NVIDIA-CUDA",
            "NVIDIA CUDA Toolkit End User License Agreement",
            "https://docs.nvidia.com/cuda/eula/index.html",
            "2026-07-16"),
        _ => null,
    };

    private static string AcceptanceKey(Policy policy)
        => $"ModelLicenseAccepted:{policy.Key}:{policy.ReviewedAt}";

    // The app is UNPACKAGED (WiX MSI / Burn bundle — no MSIX, no package
    // identity), so Windows.Storage.ApplicationData.Current is unavailable and
    // throws. Persist acceptance to a small JSON file beside app-settings.json,
    // the same file-based store every other setting uses.
    private static string AcceptancePath =>
        System.IO.Path.Combine(AppPaths.Root, "model-licenses.json");

    private static bool IsAccepted(Policy policy)
    {
        var key = AcceptanceKey(policy);
        if (SessionAcceptances.Contains(key)) return true;
        try
        {
            if (LoadAccepted().Contains(key))
            {
                SessionAcceptances.Add(key);
                return true;
            }
            return false;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Could not read {policy.Key} license acceptance: {ex.Message}");
            return false;
        }
    }

    private static bool RememberAcceptance(Policy policy)
    {
        var key = AcceptanceKey(policy);
        // The user explicitly accepted, so honor it for this session first — a
        // failed durable write (read-only profile, disk full) must NOT block the
        // download they just asked for; it only means a re-prompt next launch.
        SessionAcceptances.Add(key);
        try
        {
            var accepted = LoadAccepted();
            if (accepted.Add(key))
            {
                System.IO.Directory.CreateDirectory(AppPaths.Root);
                var tmp = AcceptancePath + ".tmp";
                System.IO.File.WriteAllText(tmp, JsonSerializer.Serialize(accepted));
                System.IO.File.Move(tmp, AcceptancePath, overwrite: true);
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"{policy.Key} acceptance recorded for this session but could not be persisted: {ex.Message}");
        }
        return true;
    }

    private static HashSet<string> LoadAccepted()
    {
        if (!System.IO.File.Exists(AcceptancePath)) return [];
        var json = System.IO.File.ReadAllText(AcceptancePath);
        var list = JsonSerializer.Deserialize<List<string>>(json);
        return list is null ? [] : new HashSet<string>(list);
    }

    private static async Task<bool> ShowTermsAsync(Policy policy)
    {
        var root = (App.HostWindow?.Content as FrameworkElement)?.XamlRoot;
        if (root is null)
        {
            DebugLog.Warn($"Refusing {policy.Key} download because the license dialog has no XamlRoot.");
            return false;
        }

        var content = new StackPanel { Spacing = 12 };
        content.Children.Add(new TextBlock
        {
            Text = $"This optional download is governed by the {policy.DisplayName}, not FileID's Apache-2.0 license. Review the terms before downloading. Selecting ‘I accept and download’ records acceptance only on this device.",
            TextWrapping = TextWrapping.Wrap,
            MaxWidth = 520,
        });
        content.Children.Add(new HyperlinkButton
        {
            Content = "Review full terms",
            NavigateUri = new Uri(policy.TermsUrl),
            HorizontalAlignment = HorizontalAlignment.Left,
            Padding = new Thickness(0),
        });

        var dialog = new ContentDialog
        {
            XamlRoot = root,
            Title = "License acceptance required",
            Content = content,
            PrimaryButtonText = "I accept and download",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        try
        {
            return await dialog.ShowAsync() == ContentDialogResult.Primary;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Refusing {policy.Key} download because the license dialog failed: {ex.Message}");
            return false;
        }
    }
}
