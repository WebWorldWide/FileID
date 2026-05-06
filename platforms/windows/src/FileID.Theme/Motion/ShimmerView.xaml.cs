// ShimmerView code-behind. Drives the storyboard on/off based on
// IsActive + ReducedMotion. When reduced-motion is on, the highlight
// stays hidden (BaseLayer alone reads as "loading state at rest").

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using System.ComponentModel;

namespace FileID.Theme.Motion;

public sealed partial class ShimmerView : UserControl
{
    public static readonly DependencyProperty IsActiveProperty =
        DependencyProperty.Register(nameof(IsActive), typeof(bool), typeof(ShimmerView),
            new PropertyMetadata(true, (d, _) => ((ShimmerView)d).Sync()));

    public ShimmerView()
    {
        InitializeComponent();
        Loaded += (_, _) => Sync();
        Unloaded += (_, _) =>
        {
            ShimmerStoryboard.Stop();
            ReducedMotion.Instance.PropertyChanged -= OnReducedMotionChanged;
        };
        ReducedMotion.Instance.PropertyChanged += OnReducedMotionChanged;
    }

    /// <summary>
    /// Tile is currently loading → shimmer animates. False → static placeholder.
    /// </summary>
    public bool IsActive
    {
        get => (bool)GetValue(IsActiveProperty);
        set => SetValue(IsActiveProperty, value);
    }

    private void OnReducedMotionChanged(object? sender, PropertyChangedEventArgs e)
    {
        // The OS fires this off-thread; marshal back to the UI thread before
        // touching XAML.
        DispatcherQueue.TryEnqueue(Sync);
    }

    private void Sync()
    {
        bool shouldRun = IsActive && !ReducedMotion.Instance.IsReduced;
        if (shouldRun)
        {
            ShimmerStoryboard.Begin();
            Highlight.Opacity = 0.6;
        }
        else
        {
            ShimmerStoryboard.Stop();
            Highlight.Opacity = 0;
        }
    }
}
