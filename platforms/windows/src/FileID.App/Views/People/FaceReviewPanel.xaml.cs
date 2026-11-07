using System.Collections.ObjectModel;
using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.People;

public sealed partial class FaceReviewPanel : UserControl
{
    private readonly ObservableCollection<FaceReviewItemVm> _rows = new();
    private readonly FaceReviewQueue _queue = new();
    private long? _personId;
    private string? _undoOperationId;
    private int _loadGeneration;
    private bool _unloaded;

    public event EventHandler<long>? TotalChanged;

    public long Total => _queue.Total;

    public FaceReviewPanel()
    {
        InitializeComponent();
        RowsRepeater.ItemsSource = _rows;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += OnUnloaded;
    }

    public void SetPerson(long? personId) => _personId = personId;

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("FaceReviewPanel.OnEngineChanged", () =>
        {
            if (_unloaded || e.PropertyName != nameof(EngineClient.LastFaceClustering)) return;
            DebugLog.Debug($"[ENGINE-SUB:FaceReviewPanel] {e.PropertyName}");
            DispatcherQueue.TryEnqueue(() => _ = ReloadAsync());
        });

    public async Task ReloadAsync()
    {
        var generation = ++_loadGeneration;
        SetBusy(true);
        try
        {
            var page = await EngineClient.Instance.ListFaceReviewAsync(_personId, 0, FaceReviewPolicy.PageSize);
            if (_unloaded || generation != _loadGeneration) return;
            if (FaceReviewPolicy.Failure(page) is { } failure)
            {
                StatusText.Text = failure;
                return;
            }
            _rows.Clear();
            foreach (var item in _queue.Reset(page)) _rows.Add(new FaceReviewItemVm { Model = item });
            StatusText.Text = "";
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

    private async void OnMoreClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync("FaceReviewPanel.OnMoreClicked", async () =>
        {
            MoreButton.IsEnabled = false;
            try
            {
                var page = await EngineClient.Instance.ListFaceReviewAsync(_personId, _queue.NextOffset, FaceReviewPolicy.PageSize);
                if (_unloaded) return;
                if (FaceReviewPolicy.Failure(page) is { } failure) StatusText.Text = failure;
                foreach (var item in _queue.Append(page)) _rows.Add(new FaceReviewItemVm { Model = item });
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
        HeaderText.Text = FaceReviewPolicy.Header(_queue.Total);
        AutomationProperties.SetName(HeaderText, FaceReviewPolicy.Header(_queue.Total));
        EmptyText.Visibility = _queue.IsEmpty ? Visibility.Visible : Visibility.Collapsed;
        MoreButton.Visibility = _queue.HasMore ? Visibility.Visible : Visibility.Collapsed;
        TotalChanged?.Invoke(this, _queue.Total);
    }

    private void SetBusy(bool busy)
    {
        BusyPanel.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
        BusyRing.IsActive = busy;
    }
}
