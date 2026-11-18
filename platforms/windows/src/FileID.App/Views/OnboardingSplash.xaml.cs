// OnboardingSplash code-behind. Drives the rainbow-drift animation on the

using System;
using System.ComponentModel;
using FileID.Services;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Animation;
using Windows.Foundation;

namespace FileID.Views;

public sealed partial class OnboardingSplash : UserControl
{
    private Storyboard? _shimmerStoryboard;

    public OnboardingSplash()
    {
        InitializeComponent();
        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
        ReducedMotion.Instance.PropertyChanged += OnReducedMotionChanged;
    }

    private void OnLoaded(object sender, RoutedEventArgs e) => UpdateShimmer();

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        ReducedMotion.Instance.PropertyChanged -= OnReducedMotionChanged;
        StopShimmer();
    }

    private void OnReducedMotionChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnReducedMotionChanged), () => OnReducedMotionChangedCore(sender, e));

    private void OnReducedMotionChangedCore(object? sender, PropertyChangedEventArgs e)
    {
        DispatcherQueue.TryEnqueue(UpdateShimmer);
    }

    private void UpdateShimmer()
    {
        StopShimmer();
        if (ReducedMotion.Instance.IsReduced)
        {
            TitleBrush.StartPoint = new Point(0, 0);
            TitleBrush.EndPoint = new Point(1, 1);
            return;
        }

        var startAnim = new PointAnimation
        {
            From = new Point(0, 0),
            To = new Point(1, 0),
            Duration = new Duration(TimeSpan.FromSeconds(12)),
            RepeatBehavior = RepeatBehavior.Forever,
            EnableDependentAnimation = true,
        };
        Storyboard.SetTarget(startAnim, TitleBrush);
        Storyboard.SetTargetProperty(startAnim, "StartPoint");

        var endAnim = new PointAnimation
        {
            From = new Point(1, 1),
            To = new Point(2, 1),
            Duration = new Duration(TimeSpan.FromSeconds(12)),
            RepeatBehavior = RepeatBehavior.Forever,
            EnableDependentAnimation = true,
        };
        Storyboard.SetTarget(endAnim, TitleBrush);
        Storyboard.SetTargetProperty(endAnim, "EndPoint");

        var sb = new Storyboard();
        sb.Children.Add(startAnim);
        sb.Children.Add(endAnim);
        _shimmerStoryboard = sb;
        sb.Begin();
    }

    private void StopShimmer()
    {
        try { _shimmerStoryboard?.Stop(); } catch { /* best-effort */ }
        _shimmerStoryboard = null;
    }

    private async void OnPickFolderClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            var hwnd = App.HostWindow is { } window
                ? WinRT.Interop.WindowNative.GetWindowHandle(window)
                : IntPtr.Zero;
            var result = await FolderPickerService.PickFolderAsync(hwnd, FolderPickerService.ScanCommitText);
            if (result.FailureReason is not null)
            {
                // On first-run the user is stranded on this splash with no other affordance, so surface the actionable reason and offer to re-open the picker — never swallow it to a log line.
                var retry = await ShowPickFailureAsync(result.FailureReason);
                if (retry)
                {
                    OnPickFolderClicked(sender, e);
                }
                return;
            }
            if (result.Path is null)
            {
                // User cancelled the picker — not an error, no surfacing.
                return;
            }
            await AppViewModel.Instance.ChooseFolderAsync(result.Path);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnboardingSplash.OnPickFolderClicked threw: " + ex);
            await ShowAlertAsync(
                "Couldn't open the folder picker",
                "FileID couldn't open the folder picker.\n\n"
                + "Try again. If it keeps failing, restart FileID.\n\n"
                + "Details: " + ex.Message);
        }
    }

    private async Task<bool> ShowPickFailureAsync(string reason)
    {
        try
        {
            if (XamlRoot is null)
            {
                DebugLog.Warn("ShowPickFailureAsync: XamlRoot is null; skipping dialog.");
                return false;
            }
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = "Couldn't use that folder",
                Content = reason,
                PrimaryButtonText = "Try again",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Primary,
            };
            return await dialog.ShowAsync() == ContentDialogResult.Primary;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("ShowPickFailureAsync threw: " + ex.Message);
            return false;
        }
    }

    private async Task ShowAlertAsync(string title, string body)
    {
        // Catch + log so a failed alert never escalates to App.UnhandledException.
        try
        {
            if (XamlRoot is null)
            {
                DebugLog.Warn($"ShowAlertAsync: XamlRoot is null ({title}); skipping dialog.");
                return;
            }
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = title,
                Content = body,
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ShowAlertAsync({title}) threw: " + ex.Message);
        }
    }
}
