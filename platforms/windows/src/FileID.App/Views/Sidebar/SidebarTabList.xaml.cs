// SidebarTabList code-behind. Builds six tab buttons programmatically and
// keeps their selected/disabled visual state in sync with AppViewModel.

using System.ComponentModel;
using FileID.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarTabList : UserControl
{
    private readonly Dictionary<string, Button> _tabButtons = new();

    public SidebarTabList()
    {
        InitializeComponent();
        BuildButtons();
        AppViewModel.Instance.PropertyChanged += OnAppViewModelChanged;
        Loaded += (_, _) => SyncSelection();
        Unloaded += (_, _) => AppViewModel.Instance.PropertyChanged -= OnAppViewModelChanged;
    }

    private void BuildButtons()
    {
        foreach (var tab in SidebarTab.All)
        {
            var btn = new Button
            {
                HorizontalAlignment = HorizontalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Stretch,
                Padding = new Thickness(10, 8, 10, 8),
                BorderThickness = new Thickness(1),
                BorderBrush = new SolidColorBrush(Colors.Transparent),
                Background = new SolidColorBrush(Colors.Transparent),
                CornerRadius = new CornerRadius(8),
                Tag = tab.Id,
            };
            AutomationProperties.SetName(btn, tab.Label);
            AutomationProperties.SetHelpText(btn, $"Switch to the {tab.Label} tab");

            var glyphIcon = new FontIcon
            {
                FontFamily = new FontFamily("Segoe Fluent Icons"),
                Glyph = tab.IconGlyph,
                FontSize = 14,
                Opacity = 0.85,
            };
            var label = new TextBlock
            {
                Text = tab.Label,
                FontSize = 13,
                VerticalAlignment = VerticalAlignment.Center,
            };
            var stack = new StackPanel
            {
                Orientation = Orientation.Horizontal,
                Spacing = 10,
                VerticalAlignment = VerticalAlignment.Center,
            };
            stack.Children.Add(glyphIcon);
            stack.Children.Add(label);
            btn.Content = stack;

            btn.Click += (_, _) => AppViewModel.Instance.ActiveTab = tab;

            _tabButtons[tab.Id] = btn;
            ButtonsHost.Children.Add(btn);
        }
    }

    private void OnAppViewModelChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(AppViewModel.ActiveTab) or nameof(AppViewModel.HasFolder))
        {
            DispatcherQueue.TryEnqueue(SyncSelection);
        }
    }

    private void SyncSelection()
    {
        var vm = AppViewModel.Instance;
        var goldBrush = (SolidColorBrush)Application.Current.Resources["GoldSelectedBackgroundBrush"];
        var goldStroke = (SolidColorBrush)Application.Current.Resources["GoldSelectedStrokeBrush"];
        var transparent = new SolidColorBrush(Colors.Transparent);

        foreach (var (tabId, btn) in _tabButtons)
        {
            bool selected = vm.ActiveTab.Id == tabId;
            btn.Background = selected ? goldBrush : transparent;
            btn.BorderBrush = selected ? goldStroke : transparent;

            // Disable every tab except Settings until a folder is picked
            // (matches macOS "Pick a folder above to enable tabs" hint).
            bool enabled = vm.HasFolder || tabId == "settings";
            btn.IsEnabled = enabled;
            btn.Opacity = enabled ? 1.0 : 0.4;
        }
    }
}
