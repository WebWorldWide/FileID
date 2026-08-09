// DetailHostView code-behind. Subscribes to AppViewModel and swaps the
// hosted view when the active tab or folder-picked state changes.
//
// tab swap is animated with a 220 ms opacity crossfade (the same
// timing macOS uses). Reduce-motion gates the animation so the swap is
// instant for users who prefer it.

using System.ComponentModel;
using FileID.Services;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Animation;

namespace FileID.Views;

public sealed partial class DetailHostView : UserControl
{
    /// <summary>The currently-running tab-swap Storyboard, if any. Tracked
    /// so it can be Stopped on Unloaded — otherwise the animation keeps
    /// running past view detach, holding a reference to the (now orphaned)
    /// Host element and preventing GC.</summary>
    private Storyboard? _activeStoryboard;

    public DetailHostView()
    {
        InitializeComponent();
        Loaded += (_, _) => Sync(animate: false);
        AppViewModel.Instance.PropertyChanged += OnAppChanged;
        Unloaded += OnUnloaded;
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.PropertyChanged -= OnAppChanged;
        try { _activeStoryboard?.Stop(); } catch { /* best-effort */ }
        _activeStoryboard = null;
    }

    private void OnAppChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(AppViewModel.ActiveTab) or nameof(AppViewModel.HasFolder))
        {
            DispatcherQueue.TryEnqueue(() => Sync(animate: true));
        }
    }

    private void Sync(bool animate)
    {
        if (!animate || ReducedMotion.Instance.IsReduced)
        {
            try { _activeStoryboard?.Stop(); } catch { /* best-effort */ }
            _activeStoryboard = null;
            CommitChild(BuildChild());
            Host.Opacity = 1.0;
            return;
        }

        // Crossfade: fade the current content out (110 ms), THEN build + swap
        // the new view, then fade it in (110 ms). The new view is built inside
        // the fade-out completion (not up front) for a load-bearing reason: a
        // rapid second tab click Stops this storyboard, so its Completed never
        // runs. If we built the view eagerly here, that view would have already
        // subscribed to EngineClient.PropertyChanged in its ctor but never be
        // added to the tree — so it would never Unload, never unsubscribe, and
        // become a zombie that keeps refreshing a never-shown ReadStore on
        // every engine event. Lazy-building in Completed means a superseded
        // swap constructs nothing.
        var fadeOut = new DoubleAnimation
        {
            To = 0.0,
            Duration = new Duration(System.TimeSpan.FromMilliseconds(110)),
            EasingFunction = new SineEase { EasingMode = EasingMode.EaseInOut },
        };
        Storyboard.SetTarget(fadeOut, Host);
        Storyboard.SetTargetProperty(fadeOut, "Opacity");
        var sbOut = new Storyboard();
        sbOut.Children.Add(fadeOut);
        // Stop any prior in-flight animation before starting a new one;
        // racing storyboards on the same target leak references.
        try { _activeStoryboard?.Stop(); } catch { }
        _activeStoryboard = sbOut;
        sbOut.Completed += (_, _) =>
        {
            // Superseded by a newer swap (defensive — Stop() shouldn't raise
            // Completed, but never build/dispose for a stale storyboard).
            if (!ReferenceEquals(_activeStoryboard, sbOut)) return;

            CommitChild(BuildChild());

            var fadeIn = new DoubleAnimation
            {
                From = 0.0,
                To = 1.0,
                Duration = new Duration(System.TimeSpan.FromMilliseconds(110)),
                EasingFunction = new SineEase { EasingMode = EasingMode.EaseInOut },
            };
            Storyboard.SetTarget(fadeIn, Host);
            Storyboard.SetTargetProperty(fadeIn, "Opacity");
            var sbIn = new Storyboard();
            sbIn.Children.Add(fadeIn);
            sbIn.Completed += (_, _) =>
            {
                if (ReferenceEquals(_activeStoryboard, sbIn)) _activeStoryboard = null;
            };
            _activeStoryboard = sbIn;
            sbIn.Begin();
        };
        sbOut.Begin();
    }

    /// <summary>Build the view for the currently-active tab. Pure construction;
    /// no tree mutation — call <see cref="CommitChild"/> to mount it.</summary>
    private UIElement BuildChild()
    {
        var vm = AppViewModel.Instance;
        // Settings is reachable WITHOUT a folder (matches the special-case
        // in SidebarTabList that keeps the Settings entry enabled at the
        // pre-folder onboarding stage). Without this short-circuit the
        // build falls through to OnboardingSplash and the user never
        // sees the Settings view they just clicked.
        if (vm.ActiveTab.Id == "settings")
        {
            return new Settings.SettingsView();
        }
        if (!vm.HasFolder)
        {
            return new OnboardingSplash();
        }
        return vm.ActiveTab.Id switch
        {
            "library" => (UIElement)new Library.LibraryView(),
            "people" => (UIElement)new People.PeopleView(),
            "cleanup" => (UIElement)new Cleanup.CleanupView(),
            "deepanalyze" => (UIElement)new DeepAnalyze.DeepAnalyzeView(),
            "restructure" => (UIElement)new Restructure.RestructureView(),
            _ => BuildPlaceholder("", vm.ActiveTab.Label, "Coming soon."),
        };
    }

    /// <summary>Atomically swap the hosted view: dispose any IDisposable
    /// outgoing child, clear the host (which fires the outgoing view's Unloaded
    /// → its subscription teardown), then mount the new child. Always run
    /// synchronously so the outgoing view is never disposed mid-animation.</summary>
    private void CommitChild(UIElement child)
    {
        DisposePriorChild();
        Host.Children.Clear();
        Host.Children.Add(child);
    }

    /// <summary>explicitly dispose the outgoing tab's UserControl
    /// if it implements IDisposable, then clear the host. Without this the
    /// old view became unreachable and waited on GC to finalize, leaving its
    /// ReadStore / ClipSearchService / thumbnail cache alive for an
    /// unbounded window — during which engine event callbacks could still
    /// fire into a detached XAML element and crash the dispatcher.</summary>
    private void DisposePriorChild()
    {
        if (Host.Children.Count == 0) return;
        foreach (var c in Host.Children)
        {
            if (c is System.IDisposable d)
            {
                try { d.Dispose(); } catch (System.Exception ex) { DebugLog.Warn("DetailHostView prior-child Dispose threw: " + ex.Message); }
            }
        }
    }

    private static EmptyStateView BuildPlaceholder(string glyph, string title, string body) =>
        new()
        {
            Glyph = glyph,
            Title = title,
            Body = body,
            Secondary = "Scaffold placeholder — visual + interaction parity in progress.",
        };
}
