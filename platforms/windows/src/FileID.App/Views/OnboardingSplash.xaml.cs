// OnboardingSplash code-behind. Drives the rainbow-drift animation on the
// "FileID" title and routes the "Pick a folder" CTA to the same
// FolderPickerService the sidebar uses.
//
// Animation is a 1:1 port of macOS Detail.swift:171-177 — `shimmer` driven
// from 0 to 1 over 12 s linear, repeating forever. We animate the
// LinearGradientBrush's EndPoint instead of a State<Double> bound to a
// UnitPoint expression, since WinUI doesn't have SwiftUI's reactive
// gradient endpoint pattern.

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

        // Mirror macOS Detail.swift's startPoint=(shimmer,0) + endPoint=(shimmer+1,1)
        // with `shimmer` linear-driven 0→1 over 12 s. Both points slide together
        // on the X axis; Y stays fixed (0 on start, 1 on end) so the gradient
        // direction is constant 45° and only the gradient origin translates.
        // Result is a single smooth drift of the rainbow across the title.
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
            var result = await FolderPickerService.PickFolderAsync(hwnd);
            if (result.FailureReason is not null || result.Path is null)
            {
                return;
            }
            AppViewModel.Instance.FolderPath = result.Path;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnboardingSplash.OnPickFolderClicked threw: " + ex);
        }
    }
}
