// DetailHostView code-behind. Subscribes to AppViewModel and swaps the
// hosted view when the active tab or folder-picked state changes.
//
// V14.2: tab swap is animated with a 220 ms opacity crossfade (the same
// timing macOS uses). Reduce-motion gates the animation so the swap is
// instant for users who prefer it.

using System.ComponentModel;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Animation;

namespace FileID.Views;

public sealed partial class DetailHostView : UserControl
{
    public DetailHostView()
    {
        InitializeComponent();
        Loaded += (_, _) => Sync(animate: false);
        AppViewModel.Instance.PropertyChanged += OnAppChanged;
        Unloaded += (_, _) => AppViewModel.Instance.PropertyChanged -= OnAppChanged;
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
        var vm = AppViewModel.Instance;
        UIElement child;
        if (!vm.HasFolder)
        {
            child = new OnboardingSplash();
        }
        else
        {
            child = vm.ActiveTab.Id switch
            {
                "library"     => (UIElement)new Library.LibraryView(),
                "people"      => (UIElement)new People.PeopleView(),
                "cleanup"     => (UIElement)new Cleanup.CleanupView(),
                "settings"    => (UIElement)new Settings.SettingsView(),
                "deepanalyze" => (UIElement)new DeepAnalyze.DeepAnalyzeView(),
                "restructure" => (UIElement)new Restructure.RestructureView(),
                _              => BuildPlaceholder("",  vm.ActiveTab.Label, "Coming soon."),
            };
        }

        if (!animate || ReducedMotion.Instance.IsReduced)
        {
            Host.Children.Clear();
            Host.Children.Add(child);
            return;
        }

        // Two-phase crossfade: fade existing content out (110 ms), swap
        // the child, fade new content in (110 ms). Total 220 ms — matches
        // the macOS tab transition.
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
        sbOut.Completed += (_, _) =>
        {
            Host.Children.Clear();
            Host.Children.Add(child);
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
            sbIn.Begin();
        };
        sbOut.Begin();
    }

    private static EmptyStateView BuildPlaceholder(string glyph, string title, string body) =>
        new()
        {
            Glyph = glyph,
            Title = title,
            Body = body,
            Secondary = "Phase 1 scaffold — visual + interaction parity will land in the phase noted.",
        };
}
