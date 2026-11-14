// SuggestedMergesSheet code-behind. Binds EngineClient's
// LastMergeSuggestions to an ItemsRepeater via a DataTemplate over
// MergeSuggestionVm (see PeopleViewModel). Each row shows side-by-side anchor
// face JPEGs + similarity % + action buttons. Merge fires mergeClusters IPC;
// Different-people writes a face_verifications row so we don't keep
// re-suggesting it.
//
// Rendering is data-template-driven (not imperative UIElement construction):
// the template resolves {ThemeResource} brushes natively and the ItemsRepeater
// recycles containers, so we never index theme brushes off
// Application.Resources (KeyNotFoundException) nor rebuild sibling UIElement
// subtrees per engine event (layout-pass fast-fail) — the two crash shapes the
// prior imperative BuildRow/BuildFaceImage path hit. See platforms/windows/
// CLAUDE.md (V15.x DispatcherObject / ItemsRepeater notes).

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
        // subscribe in ctor (not Loaded). ContentDialog hosts
        // don't reliably fire Loaded; the WelcomeSheet hit the same wall.
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += OnUnloaded;
        Loaded += async (_, _) =>
        {
            // Trigger a fresh suggestion fetch whenever the sheet opens, and
            // bound-wait the engine's reply so the sheet doesn't sit forever on
            // the placeholder when clustering is still running. On success the
            // MergeSuggestionsEvent already drives Render() via the
            // LastMergeSuggestions PropertyChanged subscription — nothing extra
            // to do here.
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
        // Guard any dispatcher tick that fires after the ContentDialog closes
        // (Render would otherwise touch a torn-down sheet), and drop the
        // engine subscription.
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

            // Await the engine's bulkActionResult BEFORE dimming the row.
            // Previously the row was marked resolved on a fire-and-forget
            // send, so an engine-side merge failure left the pair greyed-out
            // and un-actionable while the clusters stayed un-merged.
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
            // The merged-away source person no longer exists; resolve any other
            // visible pair that references it so the user can't act on a
            // now-dangling person (which would be a no-op merge).
            foreach (var other in _rows)
            {
                if (other.IsResolved) continue;
                if (other.SourcePersonId == vm.SourcePersonId
                    || other.DestinationPersonId == vm.SourcePersonId
                    // The destination cluster's membership changed (it absorbed
                    // the source's faces), so any other suggestion involving
                    // the destination is now stale — its similarity score and
                    // face anchors are invalidated by the merge.
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

    // This sheet is shown inside a dialog, and only one dialog can be open at
    // a time, so the confirmation is a flyout on the button that was pressed.
    private static Task<bool> ConfirmMergeAsync(FrameworkElement anchor)
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
        // Route through the engine's single-writer connection (the app must
        // not open its own DB writer). The engine persists the verdict keyed
        // on the stable anchor face ids so findMergeSuggestions keeps
        // suppressing the pair across re-clustering.
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
