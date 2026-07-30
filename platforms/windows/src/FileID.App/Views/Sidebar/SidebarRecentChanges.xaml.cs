// SidebarRecentChanges code-behind. Badge count follows
// ChangeLog.PendingCount; ChangeLog.Changed may fire off the UI thread
// (pushes arrive via EngineClient event handlers), so all XAML writes go
// through DispatcherQueue.TryEnqueue.

using System;
using FileID.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarRecentChanges : UserControl
{
    private bool _unloaded;
    private bool _subscribed;
    private bool _dialogOpen;

    public SidebarRecentChanges()
    {
        InitializeComponent();
        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        _unloaded = false;
        if (!_subscribed)
        {
            ChangeLog.Instance.Changed += OnChangeLogChanged;
            _subscribed = true;
        }
        Sync();
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        if (!_subscribed) return;
        ChangeLog.Instance.Changed -= OnChangeLogChanged;
        _subscribed = false;
    }

    private void OnChangeLogChanged(object? sender, EventArgs e)
    {
        if (_unloaded) return;
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_unloaded) return;
            Sync();
        });
    }

    private void Sync()
        => DebugLog.SafeRun(nameof(Sync), () =>
        {
            var total = ChangeLog.Instance.Count;
            var pending = ChangeLog.Instance.PendingCount;
            Visibility = total == 0 ? Visibility.Collapsed : Visibility.Visible;
            CountBadge.Visibility = pending == 0 ? Visibility.Collapsed : Visibility.Visible;
            CountText.Text = pending.ToString();
        });

    private async void OnOpenClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnOpenClicked), async () =>
        {
            var root = XamlRoot;
            if (_dialogOpen || _unloaded || root is null) return;
            _dialogOpen = true;
            try
            {
                var dialog = new ContentDialog
                {
                    XamlRoot = root,
                    Title = "Changes this session",
                    Content = new FileID.Views.SessionChangesSheet(),
                    CloseButtonText = "Close",
                    DefaultButton = ContentDialogButton.Close,
                };
                await dialog.ShowAsync();
            }
            finally
            {
                _dialogOpen = false;
            }
        });
}
