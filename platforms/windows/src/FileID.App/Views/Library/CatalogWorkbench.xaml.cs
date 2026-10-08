using System;
using System.ComponentModel;
using System.Globalization;
using System.IO;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Media.Core;
using Windows.Media.Playback;

namespace FileID.Views.Library;

public sealed partial class CatalogWorkbench : UserControl, IDisposable
{
    private string? _requestID;
    private string? _action;
    private int _requestGeneration;
    private CatalogHit? _selected;
    private string? _editingID;
    private MediaPlayer? _player;
    private double _seekSeconds;
    private bool _disposed;

    public CatalogWorkbench()
    {
        InitializeComponent();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += (_, _) => Dispose();
        SetBusy(false);
    }

    private async void Send(string action, string? query = null, CatalogChapter? chapter = null, string? chapterID = null)
    {
        if (_disposed || _requestID != null) return;
        var requestID = Guid.NewGuid().ToString();
        _requestID = requestID;
        _requestGeneration = EngineClient.Instance.SpawnGeneration;
        _action = action;
        SetBusy(true);
        Status.Text = "Working…";
        try
        {
            var request = new CatalogRequest(requestID, action, query, _selected?.FileID, chapter, chapterID, null, null);
            await EngineClient.Instance.SendCommandAsync(new CatalogRequestCommand(request));
        }
        catch (Exception)
        {
            DispatcherQueue.TryEnqueue(() =>
            {
                if (_disposed || _requestID != requestID) return;
                _requestID = null;
                SetBusy(false);
                Status.Text = "The engine could not receive this request. Restart it and refresh before retrying an edit.";
            });
        }
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs args)
        => DebugLog.SafeRun("CatalogWorkbench.OnEngineChanged", () =>
        {
            if (args.PropertyName is not nameof(EngineClient.SpawnGeneration) and not nameof(EngineClient.State) and not nameof(EngineClient.LastCatalogResponse)) return;
            DebugLog.Info($"[ENGINE-SUB:CatalogWorkbench] {args.PropertyName}");
            if (args.PropertyName is nameof(EngineClient.SpawnGeneration) or nameof(EngineClient.State))
            {
                DispatcherQueue.TryEnqueue(() =>
                {
                    if (_disposed || _requestID == null) return;
                    if (_requestGeneration == EngineClient.Instance.SpawnGeneration
                        && EngineClient.Instance.State == EngineClient.LifecycleState.Ready)
                    {
                        return;
                    }
                    _requestID = null;
                    SetBusy(false);
                    Status.Text = "The engine stopped or restarted. Refresh chapters before retrying an edit.";
                });
                return;
            }
            if (args.PropertyName != nameof(EngineClient.LastCatalogResponse)) return;
            var response = EngineClient.Instance.LastCatalogResponse;
            if (response == null) return;
            DispatcherQueue.TryEnqueue(() => DebugLog.SafeRun("CatalogWorkbench.Receive", () => Receive(response)));
        });

    private void Receive(CatalogResponse response)
    {
        if (_disposed || response.RequestID != _requestID || _requestGeneration != EngineClient.Instance.SpawnGeneration) return;
        var action = _action;
        _requestID = null;
        Status.Text = response.Message ?? (response.Status == "ok" ? "Ready." : "Request failed.");
        if (response.Status == "ok")
        {
            if (action == "search")
            {
                Hits.Items.Clear();
                foreach (var hit in response.Hits)
                {
                    var timestamp = hit.StartSeconds is { } seconds ? $" · {seconds:0.0}s" : "";
                    Hits.Items.Add(new ListViewItem { Content = $"{Path.GetFileName(hit.Path)}{timestamp}\n{hit.Text}", Tag = hit });
                }
                Status.Text = $"{response.Hits.Count} matches.";
            }
            else
            {
                Chapters.Items.Clear();
                foreach (var chapter in response.Chapters)
                    Chapters.Items.Add(new ListViewItem { Content = $"{chapter.StartSeconds:0.0}s · {chapter.Title}{(chapter.Stale ? " · stale" : "")}", Tag = chapter });
            }
        }
        if (response.Status == "ok" && action is "deleteChapter" or "undoChapterEdit")
        {
            _editingID = null;
            Add.Content = "Add chapter";
            ChapterTitle.Text = Summary.Text = "";
            Start.Text = End.Text = "0";
        }
        SetBusy(false);
    }

    private void SetBusy(bool busy)
    {
        Search.IsEnabled = !busy;
        Hits.IsEnabled = !busy;
        New.IsEnabled = Refresh.IsEnabled = Undo.IsEnabled = Add.IsEnabled = !busy && _selected != null;
        Delete.IsEnabled = !busy && Chapters.SelectedItem != null;
    }

    private void Play(string path, double seconds)
    {
        _seekSeconds = seconds;
        _player?.Pause();
        _player?.Dispose();
        _player = null;
        if (!Uri.TryCreate(path, UriKind.Absolute, out var uri) || !uri.IsFile) return;
        var player = new MediaPlayer { AutoPlay = false };
        _player = player;
        player.MediaOpened += (_, _) => DispatcherQueue.TryEnqueue(() =>
        {
            if (!_disposed && ReferenceEquals(_player, player))
                Seek(player, _seekSeconds);
        });
        player.MediaFailed += (_, _) => DispatcherQueue.TryEnqueue(() =>
        {
            if (!_disposed && ReferenceEquals(_player, player)) Status.Text = "This file cannot be played by the installed media codecs.";
        });
        Video.SetMediaPlayer(player);
        player.Source = MediaSource.CreateFromUri(uri);
    }

    private void OnSearch(object sender, RoutedEventArgs args)
    {
        if (!string.IsNullOrWhiteSpace(Query.Text)) Send("search", query: Query.Text.Trim());
    }

    private void OnHitSelected(object sender, SelectionChangedEventArgs args)
        => DebugLog.SafeRun("CatalogWorkbench.OnHitSelected", () =>
        {
            if (Hits.SelectedItem is not ListViewItem { Tag: CatalogHit hit }) return;
            _selected = hit;
            _editingID = null;
            Add.Content = "Add chapter";
            Filename.Text = Path.GetFileName(hit.Path);
            Start.Text = End.Text = (hit.StartSeconds ?? 0).ToString(CultureInfo.CurrentCulture);
            ChapterTitle.Text = Summary.Text = "";
            Chapters.Items.Clear();
            Play(hit.Path, hit.StartSeconds ?? 0);
            Send("detail");
        });

    private void OnChapterSelected(object sender, SelectionChangedEventArgs args)
        => DebugLog.SafeRun("CatalogWorkbench.OnChapterSelected", () =>
        {
            if (Chapters.SelectedItem is not ListViewItem { Tag: CatalogChapter chapter }) return;
            _editingID = chapter.Id;
            Add.Content = "Save chapter";
            ChapterTitle.Text = chapter.Title;
            Summary.Text = chapter.Summary;
            Start.Text = chapter.StartSeconds.ToString(CultureInfo.CurrentCulture);
            End.Text = chapter.EndSeconds.ToString(CultureInfo.CurrentCulture);
            _seekSeconds = chapter.StartSeconds;
            if (_player != null && _player.PlaybackSession.NaturalDuration > TimeSpan.Zero) Seek(_player, _seekSeconds);
            SetBusy(_requestID != null);
        });

    private void OnRefresh(object sender, RoutedEventArgs args) => Send("detail");
    private void OnNew(object sender, RoutedEventArgs args)
    {
        _editingID = null;
        Add.Content = "Add chapter";
        Chapters.SelectedItem = null;
        ChapterTitle.Text = Summary.Text = "";
        Start.Text = End.Text = (_player?.PlaybackSession.Position.TotalSeconds ?? _selected?.StartSeconds ?? 0).ToString(CultureInfo.CurrentCulture);
    }
    private void OnUndo(object sender, RoutedEventArgs args) => Send("undoChapterEdit");
    private void OnDelete(object sender, RoutedEventArgs args)
    {
        if (Chapters.SelectedItem is ListViewItem { Tag: CatalogChapter chapter }) Send("deleteChapter", chapterID: chapter.Id);
    }

    private void OnAdd(object sender, RoutedEventArgs args)
    {
        if (_selected == null || string.IsNullOrWhiteSpace(ChapterTitle.Text)
            || !double.TryParse(Start.Text, out var start) || !double.TryParse(End.Text, out var end)
            || !double.IsFinite(start) || !double.IsFinite(end) || start < 0 || end < start
            || end >= TimeSpan.MaxValue.TotalSeconds)
        {
            Status.Text = "Enter a title and valid start/end seconds.";
            return;
        }
        _editingID ??= Guid.NewGuid().ToString();
        Add.Content = "Save chapter";
        Send("saveChapter", chapter: new CatalogChapter(_editingID, _selected.FileID,
            start, end, ChapterTitle.Text.Trim(), Summary.Text, "", "user", 1, true, false));
    }

    private void Seek(MediaPlayer player, double seconds)
    {
        if (!double.IsFinite(seconds) || seconds < 0 || seconds >= TimeSpan.MaxValue.TotalSeconds)
        {
            Status.Text = "This marker has an invalid timestamp. Edit its seconds before playing it.";
            return;
        }
        try
        {
            var duration = player.PlaybackSession.NaturalDuration.TotalSeconds;
            if (duration > 0 && seconds > duration)
            {
                Status.Text = "This marker is past the end of the video. Edit its seconds before playing it.";
                return;
            }
            player.PlaybackSession.Position = TimeSpan.FromSeconds(seconds);
        }
        catch (Exception)
        {
            Status.Text = "This video cannot seek to the selected marker.";
        }
    }

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        _player?.Pause();
        _player?.Dispose();
        _player = null;
    }
}
