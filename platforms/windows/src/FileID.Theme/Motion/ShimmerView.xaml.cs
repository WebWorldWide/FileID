// ShimmerView code-behind. Shows or hides the fixed highlight band from
// IsActive + ReducedMotion. The band does not animate (see ShimmerView.xaml).
// When reduced-motion is on, the highlight stays hidden (BaseLayer alone
// reads as "loading state at rest").

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
        // Subscribe on Loaded (not the ctor) so subscribe/unsubscribe are
        // lifecycle-paired. This control lives in a virtualized
        // ItemsRepeater: a ctor-time subscription leaks via the static event
        // for any instance that's never Loaded, and after the first recycle
        // (Unloaded → -=) it would stay deaf to reduced-motion toggles.
        Loaded += (_, _) =>
        {
            ReducedMotion.Instance.PropertyChanged += OnReducedMotionChanged;
            Sync();
        };
        Unloaded += (_, _) =>
        {
            ReducedMotion.Instance.PropertyChanged -= OnReducedMotionChanged;
        };
    }

    /// <summary>
    /// Tile is currently loading → the highlight band shows. False → plain placeholder.
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
        bool showBand = IsActive && !ReducedMotion.Instance.IsReduced;
        Highlight.Opacity = showBand ? 0.6 : 0;
    }
}
