// SuggestedMergesSheet code-behind. Renders EngineClient's merge suggestions through a data template: it resolves {ThemeResource} brushes natively (no Application.Resources lookups, which throw KeyNotFoundException) and the ItemsRepeater recycles containers instead of rebuilding UIElement subtrees per engine event (a layout-pass fast-fail).

using System;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Threading.Tasks;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.People;

public sealed partial class SuggestedMergesSheet : UserControl
{
    private readonly ObservableCollection<MergeSuggestionVm> _rows = new();
    private bool _unloaded;

    public SuggestedMergesSheet()
    {
        InitializeComponent();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += OnUnloaded;
        Loaded += async (_, _) =>
        {
            // Fetch fresh suggestions on open; the bounded wait keeps the placeholder from lingering while clustering runs.
            HeaderText.Text = "Looking for similar clusters…";
            SetBusy(true);
            try
            {
                await EngineClient.Instance.WaitForMergeSuggestionsAsync(TimeSpan.FromSeconds(30));
                SetBusy(false);
            }
            catch (TimeoutException)
            {
                SetBusy(false);
                HeaderText.Text = "Still preparing — clustering may be running. Try reopening this in a moment.";
            }
            catch (Exception ex)
            {
                SetBusy(false);
                Services.DebugLog.Error($"FindMergeSuggestions failed: {ex.Message}");
                HeaderText.Text = "Couldn't fetch suggestions — see logs.";
            }
        };
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        // A dispatcher tick after the dialog closes must not Render into a torn-down sheet.
        _unloaded = true;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => Services.DebugLog.SafeRun("SuggestedMergesSheet.OnEngineChanged", () =>
        {
            if (e.PropertyName != nameof(EngineClient.LastMergeSuggestions)) return;
            Services.DebugLog.Info($"[ENGINE-SUB:SuggestedMergesSheet] {e.PropertyName}");
            DispatcherQueue.TryEnqueue(Render);
        });

    private void Render()
    {
        if (_unloaded) return;
        var sug = EngineClient.Instance.LastMergeSuggestions;
        if (sug is null) return;
        SetBusy(false);
        _rows.Clear();
        if (sug.Pairs.Count == 0)
        {
            HeaderText.Text = "No likely merges found. (Try after a fresh scan + re-cluster.)";
            return;
        }
        HeaderText.Text = $"{sug.Pairs.Count} candidate pair{(sug.Pairs.Count == 1 ? "" : "s")} — review each.";
        foreach (var p in sug.Pairs)
        {
            _rows.Add(new MergeSuggestionVm { Model = p });
        }
    }

    private void SetBusy(bool busy)
    {
        BusyPanel.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
        BusyRing.IsActive = busy;
    }

    private async void OnMergeClicked(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement anchor || anchor.DataContext is not MergeSuggestionVm vm) return;
        if (vm.IsResolved || vm.IsBusy) return;
        vm.IsBusy = true;
        try
        {
            if (!await ConfirmMergeAsync(anchor)) return;
            if (_unloaded) return;

            long[] sourceFaceIds;
            try { sourceFaceIds = await PeopleView.ReadPersonFaceIdsAsync(vm.SourcePersonId); }
            catch (Exception ex)
            {
                Services.DebugLog.Warn("SuggestedMergesSheet: could not read the faces for undo: " + ex.Message);
                sourceFaceIds = Array.Empty<long>();
            }

            var r = await EngineClient.Instance.WaitForBulkActionResultAsync(
                "mergeClusters",
                () => EngineClient.Instance.MergeClustersAsync(vm.SourcePersonId, vm.DestinationPersonId),
                TimeSpan.FromSeconds(30));
            if (_unloaded) return;
            if (!PersonMergePolicy.Succeeded(r))
            {
                StatusText.Text = PersonMergePolicy.FailureText(r);
                return;
            }
            var before = Services.ChangeLog.Instance.MostRecentUndoableOn(Services.ChangePage.People);
            PeopleView.OfferMergeUndo(r, vm.SourcePersonId, vm.DestinationPersonId, sourceFaceIds);
            var filed = Services.ChangeLog.Instance.MostRecentUndoableOn(Services.ChangePage.People);
            _lastMerge = ReferenceEquals(filed, before) ? null : filed;
            _lastMergeRows.Clear();
            vm.IsResolved = true;
            _lastMergeRows.Add(vm);
            foreach (var other in _rows)
            {
                if (other.IsResolved) continue;
                if (other.SourcePersonId == vm.SourcePersonId
                    || other.DestinationPersonId == vm.SourcePersonId
                    || other.SourcePersonId == vm.DestinationPersonId
                    || other.DestinationPersonId == vm.DestinationPersonId)
                {
                    other.IsResolved = true;
                    _lastMergeRows.Add(other);
                }
            }
            StatusText.Text = PersonMergePolicy.Merged;
            UndoMergeButton.Visibility = _lastMerge is null ? Visibility.Collapsed : Visibility.Visible;
        }
        catch (Exception ex)
        {
            if (!_unloaded) StatusText.Text = $"{PersonMergePolicy.CouldNotMerge} ({ex.Message})";
        }
        finally
        {
            vm.IsBusy = false;
        }
    }

    internal static Task<bool> ConfirmMergeAsync(FrameworkElement anchor)
    {
        var answer = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var merge = new Button { Content = "Merge" };
        if (Services.ThemeHelper.GetStyleSafe("AccentButtonStyle") is { } accent) merge.Style = accent;
        var cancel = new Button { Content = "Cancel" };
        var buttons = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 8,
            HorizontalAlignment = HorizontalAlignment.Right,
        };
        buttons.Children.Add(cancel);
        buttons.Children.Add(merge);
        var panel = new StackPanel { Spacing = 12, MaxWidth = 300 };
        panel.Children.Add(new TextBlock { Text = PersonMergePolicy.ConfirmText, TextWrapping = TextWrapping.Wrap });
        panel.Children.Add(buttons);
        var flyout = new Flyout { Content = panel };
        merge.Click += (_, _) => Services.DebugLog.SafeRun("SuggestedMergesSheet.ConfirmMerge", () =>
        {
            answer.TrySetResult(true);
            flyout.Hide();
        });
        cancel.Click += (_, _) => Services.DebugLog.SafeRun("SuggestedMergesSheet.CancelMerge", flyout.Hide);
        flyout.Closed += (_, _) => answer.TrySetResult(false);
        flyout.ShowAt(anchor);
        return answer.Task;
    }

    private Services.ChangeLogEntry? _lastMerge;
    private readonly System.Collections.Generic.List<MergeSuggestionVm> _lastMergeRows = new();

    private async void OnUndoMergeClicked(object sender, RoutedEventArgs e)
        => await Services.DebugLog.SafeRunAsync("SuggestedMergesSheet.OnUndoMergeClicked", async () =>
        {
            if (_lastMerge is not { } entry) return;
            UndoMergeButton.IsEnabled = false;
            try
            {
                var undone = await Services.ChangeLog.Instance.UndoAsync(entry);
                if (_unloaded) return;
                if (!undone)
                {
                    StatusText.Text = PersonMergePolicy.CouldNotPutBack;
                    return;
                }
                foreach (var row in _lastMergeRows) row.IsResolved = false;
                _lastMergeRows.Clear();
                _lastMerge = null;
                UndoMergeButton.Visibility = Visibility.Collapsed;
                StatusText.Text = PersonMergePolicy.PutBack;
            }
            finally
            {
                if (!_unloaded) UndoMergeButton.IsEnabled = true;
            }
        });

    private async void OnDifferentClicked(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not MergeSuggestionVm vm) return;
        if (vm.IsResolved || vm.IsBusy) return;
        vm.IsBusy = true;
        try
        {
            await MarkDifferentAsync(vm);
        }
        finally
        {
            vm.IsBusy = false;
        }
    }

    private async Task MarkDifferentAsync(MergeSuggestionVm vm)
    {
        // The app never opens its own DB writer; go through the engine.
        try
        {
            await EngineClient.Instance.MarkPersonsDifferentAsync(
                vm.SourcePersonId,
                vm.DestinationPersonId,
                vm.SourceAnchorFaceId,
                vm.DestinationAnchorFaceId);
            vm.IsResolved = true;
            StatusText.Text = "Marked as different people. FileID won't suggest this pair again.";
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Couldn't save: {ex.Message}";
        }
    }
}
