using System.Collections.ObjectModel;
using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;

namespace FileID.Views.People;

public sealed partial class FaceReviewPanel : UserControl
{
    private static readonly string[] Queues = { FaceQueuePolicy.Faces, FaceQueuePolicy.Merges, FaceQueuePolicy.Splits };
    private readonly ObservableCollection<FaceReviewItemVm> _rows = new();
    private readonly FaceReviewQueue _queue = new();
    private readonly ObservableCollection<FaceMergeRowVm> _mergeRows = new();
    private readonly KeyedReviewQueue<FaceMergeSuggestion, (long, long)> _merges = new(merge => (merge.PersonId, merge.OtherPersonId));
    private readonly ObservableCollection<FaceSplitRowVm> _splitRows = new();
    private readonly KeyedReviewQueue<FaceSplitSuggestion, long> _splits = new(split => split.PersonId);
    private readonly FaceQueueTotals _totals = new();
    private string _activeQueue = FaceQueuePolicy.Faces;
    private long? _personId;
    private string? _personName;
    private string? _undoOperationId;
    private int _loadGeneration;
    private bool _unloaded;

    public event EventHandler<long>? TotalChanged;

    public long Total => _personId is null ? _totals.Sum : _queue.Total;

    public FaceReviewPanel()
    {
        InitializeComponent();
        RowsRepeater.ItemsSource = _rows;
        MergeRepeater.ItemsSource = _mergeRows;
        SplitRepeater.ItemsSource = _splitRows;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += OnUnloaded;
    }

    public void SetPerson(long? personId, string? personName = null)
    {
        _personId = personId;
        _personName = personName;
        QueueSwitcher.Visibility = personId is null ? Visibility.Visible : Visibility.Collapsed;
        if (personId is not null) SelectQueue(FaceQueuePolicy.Faces, reload: false);
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("FaceReviewPanel.OnEngineChanged", () =>
        {
            if (_unloaded || e.PropertyName != nameof(EngineClient.LastFaceClustering)) return;
            DebugLog.Info($"[ENGINE-SUB:FaceReviewPanel] {e.PropertyName}");
            DispatcherQueue.TryEnqueue(() => _ = ReloadAsync());
        });

    public async Task ReloadAsync()
    {
        var generation = ++_loadGeneration;
        var queue = _activeQueue;
        SetBusy(true);
        try
        {
            var page = await EngineClient.Instance.ListFaceReviewAsync(_personId, 0, FaceReviewPolicy.PageSize, FaceQueuePolicy.ListQueue(queue));
            if (_unloaded || generation != _loadGeneration) return;
            if (FaceReviewPolicy.Failure(page) is { } failure)
            {
                StatusText.Text = failure;
                return;
            }
            ApplyPage(queue, page, reset: true);
            StatusText.Text = "";
            if (_personId is null) await RefreshOtherTotalsAsync(queue, generation);
        }
        catch (Exception ex)
        {
            if (!_unloaded) StatusText.Text = "Couldn't load suggestions: " + ex.Message;
            DebugLog.Warn("FaceReviewPanel reload failed: " + ex.Message);
        }
        finally
        {
            if (!_unloaded && generation == _loadGeneration)
            {
                SetBusy(false);
                RenderCounts();
            }
        }
    }

    private async Task RefreshOtherTotalsAsync(string loaded, int generation)
    {
        foreach (var queue in Queues)
        {
            if (queue == loaded) continue;
            try
            {
                var page = await EngineClient.Instance.ListFaceReviewAsync(null, 0, 1, FaceQueuePolicy.ListQueue(queue));
                if (_unloaded || generation != _loadGeneration) return;
                if (FaceReviewPolicy.IsOk(page)) _totals.Set(queue, page.Total);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("FaceReviewPanel queue count failed: " + ex.Message);
            }
        }
    }

    private void ApplyPage(string queue, FaceReviewPage page, bool reset)
    {
        if (!FaceReviewPolicy.IsOk(page)) return;
        _totals.Set(queue, page.Total);
        switch (queue)
        {
            case FaceQueuePolicy.Merges:
                if (reset) _mergeRows.Clear();
                var merges = reset ? _merges.Reset(page.Merges, page.Total) : _merges.Append(page.Merges, page.Total);
                foreach (var merge in merges) _mergeRows.Add(new FaceMergeRowVm { Model = merge });
                break;
            case FaceQueuePolicy.Splits:
                if (reset) _splitRows.Clear();
                var splits = reset ? _splits.Reset(page.Splits, page.Total) : _splits.Append(page.Splits, page.Total);
                foreach (var split in splits) _splitRows.Add(new FaceSplitRowVm { Model = split });
                break;
            default:
                if (reset) _rows.Clear();
                var items = reset ? _queue.Reset(page) : _queue.Append(page);
                foreach (var item in items) _rows.Add(new FaceReviewItemVm { Model = item });
                break;
        }
    }

    private async void OnMoreClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync("FaceReviewPanel.OnMoreClicked", async () =>
        {
            MoreButton.IsEnabled = false;
            var queue = _activeQueue;
            try
            {
                var page = await EngineClient.Instance.ListFaceReviewAsync(_personId, NextOffset(queue), FaceReviewPolicy.PageSize, FaceQueuePolicy.ListQueue(queue));
                if (_unloaded || queue != _activeQueue) return;
                if (FaceReviewPolicy.Failure(page) is { } failure) StatusText.Text = failure;
                ApplyPage(queue, page, reset: false);
            }
            catch (Exception ex)
            {
                if (!_unloaded) StatusText.Text = "Couldn't load more suggestions: " + ex.Message;
            }
            finally
            {
                if (!_unloaded)
                {
                    MoreButton.IsEnabled = true;
                    RenderCounts();
                }
            }
        });

    private long NextOffset(string queue) => queue switch
    {
        FaceQueuePolicy.Merges => _merges.NextOffset,
        FaceQueuePolicy.Splits => _splits.NextOffset,
        _ => _queue.NextOffset,
    };

    private void OnQueueClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun("FaceReviewPanel.OnQueueClicked", () =>
        {
            var queue = ReferenceEquals(sender, MergesQueueButton) ? FaceQueuePolicy.Merges
                : ReferenceEquals(sender, SplitsQueueButton) ? FaceQueuePolicy.Splits
                : FaceQueuePolicy.Faces;
            SelectQueue(queue, reload: true);
        });

    private void SelectQueue(string queue, bool reload)
    {
        var changed = queue != _activeQueue;
        _activeQueue = queue;
        FacesQueueButton.IsChecked = queue == FaceQueuePolicy.Faces;
        MergesQueueButton.IsChecked = queue == FaceQueuePolicy.Merges;
        SplitsQueueButton.IsChecked = queue == FaceQueuePolicy.Splits;
        if (!changed) return;
        RowsRepeater.Visibility = queue == FaceQueuePolicy.Faces ? Visibility.Visible : Visibility.Collapsed;
        MergeRepeater.Visibility = queue == FaceQueuePolicy.Merges ? Visibility.Visible : Visibility.Collapsed;
        SplitRepeater.Visibility = queue == FaceQueuePolicy.Splits ? Visibility.Visible : Visibility.Collapsed;
        QueueHelpText.Text = FaceQueuePolicy.QueueHelp(queue);
        EmptyText.Text = FaceQueuePolicy.EmptyText(queue);
        RenderCounts();
        if (!ReducedMotion.Instance.IsReduced)
        {
            SpringEasing.SetScalar(RowsScroller, "Opacity", 0f);
            SpringEasing.AnimateOpacity(RowsScroller, 1f, 0.38, 0.8);
        }
        if (reload) _ = ReloadAsync();
    }

    private void OnRowElementPrepared(ItemsRepeater sender, ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not FrameworkElement element) return;
        if (FileID.Views.Restructure.RestructureView.ResolveRepeaterItem<FaceReviewItemVm>(sender.ItemsSource, args.Index) is not { } item) return;
        element.DataContext = item;
    }

    private void OnMergeElementPrepared(ItemsRepeater sender, ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not FrameworkElement element) return;
        if (FileID.Views.Restructure.RestructureView.ResolveRepeaterItem<FaceMergeRowVm>(sender.ItemsSource, args.Index) is not { } item) return;
        element.DataContext = item;
    }

    private void OnSplitElementPrepared(ItemsRepeater sender, ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not FrameworkElement element) return;
        if (FileID.Views.Restructure.RestructureView.ResolveRepeaterItem<FaceSplitRowVm>(sender.ItemsSource, args.Index) is not { } item) return;
        element.DataContext = item;
    }

    private void OnAcceptClicked(object sender, RoutedEventArgs e) => Resolve(sender, FaceReviewPolicy.Accept);
    private void OnRejectClicked(object sender, RoutedEventArgs e) => Resolve(sender, FaceReviewPolicy.Reject);
    private void OnIgnoreClicked(object sender, RoutedEventArgs e) => Resolve(sender, FaceReviewPolicy.Ignore);

    private async void Resolve(object sender, string resolution)
        => await DebugLog.SafeRunAsync("FaceReviewPanel.Resolve", async () =>
        {
            if ((sender as FrameworkElement)?.DataContext is not FaceReviewItemVm vm || vm.IsBusy) return;
            vm.IsBusy = true;
            try
            {
                var page = await EngineClient.Instance.ResolveFaceReviewAsync(vm.FaceId, vm.PersonId, resolution);
                if (_unloaded) return;
                if (FaceReviewPolicy.Failure(page) is { } failure)
                {
                    StatusText.Text = failure;
                    return;
                }
                _queue.Remove(vm.FaceId);
                _rows.Remove(vm);
                _totals.Set(FaceQueuePolicy.Faces, _queue.Total);
                StatusText.Text = "";
                if (page.OperationId is { } operationId) OfferUndo(operationId, FaceReviewPolicy.ChangeLabel(resolution, vm.Model.PersonName));
                RenderCounts();
            }
            catch (Exception ex)
            {
                if (!_unloaded) StatusText.Text = "The suggestion couldn't be saved: " + ex.Message;
            }
            finally
            {
                vm.IsBusy = false;
            }
        });

    private async void OnAcceptAllClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync("FaceReviewPanel.OnAcceptAllClicked", async () =>
        {
            if (_personId is not { } personId || !AcceptAllButton.IsEnabled) return;
            AcceptAllButton.IsEnabled = false;
            try
            {
                var page = await EngineClient.Instance.ResolveFaceReviewAsync(null, personId, FaceQueuePolicy.AcceptAll);
                if (_unloaded) return;
                if (FaceReviewPolicy.Failure(page) is { } failure)
                {
                    StatusText.Text = failure;
                    return;
                }
                var message = FaceQueuePolicy.AcceptAllMessage(page);
                await ReloadAsync();
                if (_unloaded) return;
                if (page.OperationId is { } operationId) OfferUndo(operationId, message);
                else StatusText.Text = message + ".";
            }
            catch (Exception ex)
            {
                if (!_unloaded) StatusText.Text = "The faces couldn't be accepted: " + ex.Message;
            }
            finally
            {
                if (!_unloaded) AcceptAllButton.IsEnabled = true;
            }
        });

    private void OnMergeClicked(object sender, RoutedEventArgs e) => ResolveMerge(sender, FaceQueuePolicy.Merge);
    private void OnMergeNotSameClicked(object sender, RoutedEventArgs e) => ResolveMerge(sender, FaceQueuePolicy.NotSamePerson);
    private void OnMergeDismissClicked(object sender, RoutedEventArgs e) => ResolveMerge(sender, FaceQueuePolicy.Dismiss);

    private async void ResolveMerge(object sender, string resolution)
        => await DebugLog.SafeRunAsync("FaceReviewPanel.ResolveMerge", async () =>
        {
            if ((sender as FrameworkElement)?.DataContext is not FaceMergeRowVm vm || vm.IsBusy) return;
            vm.IsBusy = true;
            try
            {
                var page = await EngineClient.Instance.ResolveFaceReviewAsync(null, vm.Model.PersonId, resolution, vm.Model.OtherPersonId);
                if (_unloaded) return;
                if (FaceReviewPolicy.Failure(page) is { } failure)
                {
                    StatusText.Text = failure;
                    return;
                }
                await ReloadAsync();
                if (!_unloaded && page.OperationId is { } operationId) OfferUndo(operationId, FaceQueuePolicy.MergeMessage(resolution, vm.Model));
            }
            catch (Exception ex)
            {
                if (!_unloaded) StatusText.Text = "The suggestion couldn't be saved: " + ex.Message;
            }
            finally
            {
                vm.IsBusy = false;
            }
        });

    private void OnSplitClicked(object sender, RoutedEventArgs e) => ResolveSplit(sender, FaceQueuePolicy.Split);
    private void OnSplitDismissClicked(object sender, RoutedEventArgs e) => ResolveSplit(sender, FaceQueuePolicy.Dismiss);

    private async void ResolveSplit(object sender, string resolution)
        => await DebugLog.SafeRunAsync("FaceReviewPanel.ResolveSplit", async () =>
        {
            if ((sender as FrameworkElement)?.DataContext is not FaceSplitRowVm vm || vm.IsBusy) return;
            vm.IsBusy = true;
            try
            {
                var page = await EngineClient.Instance.ResolveFaceReviewAsync(null, vm.Model.PersonId, resolution);
                if (_unloaded) return;
                if (FaceReviewPolicy.Failure(page) is { } failure)
                {
                    StatusText.Text = failure;
                    return;
                }
                await ReloadAsync();
                if (!_unloaded && page.OperationId is { } operationId) OfferUndo(operationId, FaceQueuePolicy.SplitMessage(resolution, vm.Model));
            }
            catch (Exception ex)
            {
                if (!_unloaded) StatusText.Text = "The suggestion couldn't be saved: " + ex.Message;
            }
            finally
            {
                vm.IsBusy = false;
            }
        });

    private void OfferUndo(string operationId, string label)
    {
        _undoOperationId = operationId;
        UndoText.Text = label + ".";
        UndoButton.IsEnabled = true;
        if (UndoBar.Visibility != Visibility.Visible)
        {
            UndoBar.Visibility = Visibility.Visible;
            if (!ReducedMotion.Instance.IsReduced)
            {
                SpringEasing.SetScalar(UndoBar, "Opacity", 0f);
                SpringEasing.AnimateOpacity(UndoBar, 1f, 0.38, 0.8);
            }
        }
        ChangeLog.Instance.Push(label, ChangeKind.Other, async () =>
        {
            var page = await EngineClient.Instance.UndoFaceReviewAsync(operationId);
            return FaceReviewPolicy.IsOk(page);
        });
    }

    private async void OnUndoClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync("FaceReviewPanel.OnUndoClicked", async () =>
        {
            if (_undoOperationId is not { } operationId) return;
            UndoButton.IsEnabled = false;
            try
            {
                var page = await EngineClient.Instance.UndoFaceReviewAsync(operationId);
                if (_unloaded) return;
                if (FaceReviewPolicy.Failure(page) is { } failure)
                {
                    StatusText.Text = failure;
                    UndoButton.IsEnabled = true;
                    return;
                }
                _undoOperationId = null;
                UndoBar.Visibility = Visibility.Collapsed;
                await ReloadAsync();
            }
            catch (Exception ex)
            {
                if (!_unloaded)
                {
                    StatusText.Text = "Undo didn't finish: " + ex.Message;
                    UndoButton.IsEnabled = true;
                }
            }
        });

    private void RenderCounts()
    {
        HeaderText.Text = FaceReviewPolicy.Header(Total);
        AutomationProperties.SetName(HeaderText, FaceReviewPolicy.Header(Total));
        var (isEmpty, hasMore) = _activeQueue switch
        {
            FaceQueuePolicy.Merges => (_merges.IsEmpty, _merges.HasMore),
            FaceQueuePolicy.Splits => (_splits.IsEmpty, _splits.HasMore),
            _ => (_queue.IsEmpty, _queue.HasMore),
        };
        EmptyText.Visibility = isEmpty ? Visibility.Visible : Visibility.Collapsed;
        MoreButton.Visibility = hasMore ? Visibility.Visible : Visibility.Collapsed;
        RenderQueueButton(FacesQueueButton, FaceQueuePolicy.Faces);
        RenderQueueButton(MergesQueueButton, FaceQueuePolicy.Merges);
        RenderQueueButton(SplitsQueueButton, FaceQueuePolicy.Splits);
        RenderAcceptAll();
        TotalChanged?.Invoke(this, Total);
    }

    private void RenderQueueButton(ToggleButton button, string queue)
    {
        button.Content = FaceQueuePolicy.QueueText(queue, _totals[queue]);
        AutomationProperties.SetName(button, FaceQueuePolicy.QueueLabel(queue, _totals[queue]));
    }

    private void RenderAcceptAll()
    {
        var suggested = FaceQueuePolicy.SuggestedCount(_queue.Items);
        var show = _activeQueue == FaceQueuePolicy.Faces && FaceQueuePolicy.ShowAcceptAll(_personId, suggested);
        AcceptAllButton.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        if (!show) return;
        var name = _queue.Items.FirstOrDefault(item => item.PersonId == _personId)?.PersonName ?? _personName;
        AcceptAllButton.Content = FaceQueuePolicy.AcceptAllText(suggested);
        AutomationProperties.SetName(AcceptAllButton, FaceQueuePolicy.AcceptAllLabel(suggested, name));
    }

    private void SetBusy(bool busy)
    {
        BusyPanel.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
        BusyRing.IsActive = busy;
    }
}
