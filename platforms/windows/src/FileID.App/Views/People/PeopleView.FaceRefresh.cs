using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;

namespace FileID.Views.People;

public sealed partial class PeopleView
{
    private FaceRefreshStatus? _faceRefreshStatus;
    private CatalogJob? _faceRefreshJob;
    private string _faceRefreshMessage = "";
    private bool _faceRefreshLoading;
    private bool _faceRefreshDirty;
    private bool _faceRefreshBusy;

    private void OnFaceRefreshEngineChanged(PropertyChangedEventArgs e)
    {
        var response = EngineClient.Instance.LastCatalogResponse;
        if (!FaceQueuePolicy.IsJobUpdate(response)) return;
        DebugLog.Info($"[ENGINE-SUB:PeopleView] {e.PropertyName}=faceRefresh");
        var job = FaceQueuePolicy.Job(response!.Jobs);
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_unloaded) return;
            _faceRefreshJob = job;
            RenderFaceRefresh();
            _ = RefreshFaceRefreshStatusAsync();
        });
    }

    private async Task RefreshFaceRefreshStatusAsync()
    {
        if (_unloaded) return;
        if (_faceRefreshLoading)
        {
            _faceRefreshDirty = true;
            return;
        }
        _faceRefreshLoading = true;
        try
        {
            do
            {
                _faceRefreshDirty = false;
                var page = await EngineClient.Instance.FaceRefreshStatusAsync();
                if (_unloaded) return;
                if (FaceReviewPolicy.IsOk(page) && page.Refresh is { } status)
                {
                    _faceRefreshStatus = status;
                    _faceRefreshJob = FaceQueuePolicy.MergeJob(_faceRefreshJob, status);
                    RenderFaceRefresh();
                }
            }
            while (_faceRefreshDirty && !_unloaded);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("PeopleView face refresh status failed: " + ex.Message);
        }
        finally
        {
            _faceRefreshLoading = false;
        }
    }

    private void RenderFaceRefresh()
    {
        var view = FaceQueuePolicy.RefreshView(_faceRefreshStatus, _faceRefreshJob);
        var note = _faceRefreshMessage.Length > 0 ? _faceRefreshMessage : view.Note;
        var visible = view.Visible || note.Length > 0;
        var shown = FaceRefreshBar.Visibility == Visibility.Visible;
        FaceRefreshBar.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        if (!visible) return;
        FaceRefreshText.Text = view.Text;
        AutomationProperties.SetName(FaceRefreshText, view.Label);
        FaceRefreshNote.Text = note;
        FaceRefreshNote.Visibility = note.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        var active = FaceQueuePolicy.IsActive(_faceRefreshJob?.State);
        FaceRefreshProgress.Visibility = active ? Visibility.Visible : Visibility.Collapsed;
        FaceRefreshProgress.Value = FaceQueuePolicy.Percent(_faceRefreshJob?.Progress);
        FaceRefreshUpdateButton.Visibility = view.CanUpdate ? Visibility.Visible : Visibility.Collapsed;
        AutomationProperties.SetName(FaceRefreshUpdateButton, FaceQueuePolicy.UpdateLabel(_faceRefreshStatus?.Outdated ?? 0));
        FaceRefreshPauseButton.Visibility = view.CanPause ? Visibility.Visible : Visibility.Collapsed;
        FaceRefreshResumeButton.Visibility = view.CanResume ? Visibility.Visible : Visibility.Collapsed;
        FaceRefreshCancelButton.Visibility = view.CanCancel ? Visibility.Visible : Visibility.Collapsed;
        SetFaceRefreshButtonsEnabled(!_faceRefreshBusy);
        if (!shown && !ReducedMotion.Instance.IsReduced)
        {
            SpringEasing.SetScalar(FaceRefreshBar, "Opacity", 0f);
            SpringEasing.AnimateOpacity(FaceRefreshBar, 1f, 0.38, 0.8);
        }
    }

    private void SetFaceRefreshButtonsEnabled(bool enabled)
    {
        FaceRefreshUpdateButton.IsEnabled = enabled;
        FaceRefreshPauseButton.IsEnabled = enabled;
        FaceRefreshResumeButton.IsEnabled = enabled;
        FaceRefreshCancelButton.IsEnabled = enabled;
    }

    private async void OnUpdateFacesClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync("PeopleView.OnUpdateFacesClicked", () =>
            RunFaceRefreshRequestAsync(FaceQueuePolicy.EnqueueRequest(Guid.NewGuid().ToString())));

    private void OnFaceRefreshPauseClicked(object sender, RoutedEventArgs e) => ControlFaceRefresh("pauseJob");
    private void OnFaceRefreshResumeClicked(object sender, RoutedEventArgs e) => ControlFaceRefresh("resumeJob");
    private void OnFaceRefreshCancelClicked(object sender, RoutedEventArgs e) => ControlFaceRefresh("cancelJob");

    private async void ControlFaceRefresh(string action)
        => await DebugLog.SafeRunAsync("PeopleView.ControlFaceRefresh", async () =>
        {
            if (_faceRefreshJob is not { } job) return;
            CatalogRequest request;
            try
            {
                request = CatalogJobActions.ControlRequest(action, job);
            }
            catch (InvalidOperationException ex)
            {
                _faceRefreshMessage = ex.Message;
                RenderFaceRefresh();
                return;
            }
            await RunFaceRefreshRequestAsync(request);
        });

    private async Task RunFaceRefreshRequestAsync(CatalogRequest request)
    {
        if (_faceRefreshBusy || _unloaded) return;
        _faceRefreshBusy = true;
        SetFaceRefreshButtonsEnabled(false);
        try
        {
            var response = await EngineClient.Instance.CatalogAsync(request);
            if (_unloaded) return;
            if (response.Status != "ok")
            {
                _faceRefreshMessage = string.IsNullOrWhiteSpace(response.Message) ? "That didn't work. Try again in a moment." : response.Message;
                return;
            }
            _faceRefreshMessage = "";
            if (FaceQueuePolicy.Job(response.Jobs) is { } job) _faceRefreshJob = job;
        }
        catch (Exception ex)
        {
            if (!_unloaded) _faceRefreshMessage = "The face update didn't start: " + ex.Message;
        }
        finally
        {
            _faceRefreshBusy = false;
            if (!_unloaded)
            {
                RenderFaceRefresh();
                await RefreshFaceRefreshStatusAsync();
            }
        }
    }
}
