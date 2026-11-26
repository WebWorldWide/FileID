// Shows or hides the fixed highlight band from IsActive and ReducedMotion; the band itself does not animate.

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
        // Subscribe on Loaded, not the constructor: in a virtualized ItemsRepeater a constructor-time subscription
        // leaks via the static event for never-loaded instances, and recycled ones (Unloaded unsubscribes) would miss toggles.
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

    /// <summary>True while the tile is loading: the highlight band shows. False: plain placeholder.</summary>
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
