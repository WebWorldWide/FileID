// SidebarRecentChanges code-behind. Badge count follows
// ChangeLog.UndoableCount; ChangeLog.Changed may fire off the UI thread
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
    private bool _dialogOpen;

    public SidebarRecentChanges()
    {
        InitializeComponent();
        ChangeLog.Instance.Changed += OnChangeLogChanged;
        Unloaded += (_, _) =>
        {
            _unloaded = true;
            ChangeLog.Instance.Changed -= OnChangeLogChanged;
        };
        Loaded += (_, _) => Sync();
    }

    private void OnChangeLogChanged(object? sender, EventArgs e)
        => DispatcherQueue.TryEnqueue(() =>
        {
            if (_unloaded) return;
            Sync();
        });

    private void Sync()
        => DebugLog.SafeRun(nameof(Sync), () =>
        {
            var total = ChangeLog.Instance.Count;
            var undoable = ChangeLog.Instance.UndoableCount;
            Visibility = total == 0 ? Visibility.Collapsed : Visibility.Visible;
            CountBadge.Visibility = undoable == 0 ? Visibility.Collapsed : Visibility.Visible;
            CountText.Text = undoable.ToString();
        });

    private async void OnOpenClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnOpenClicked), async () =>
        {
            if (_dialogOpen) return;
            _dialogOpen = true;
            try
            {
                var dialog = new ContentDialog
                {
                    XamlRoot = XamlRoot,
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
