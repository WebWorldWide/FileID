using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Workbenches;

internal sealed class CatalogWorkbench : WorkbenchPanel
{
    private const int EagerThumbnails = 30;
    private readonly TextBox _query = new() { PlaceholderText = "Search files and videos", MaxLength = 2000, MinWidth = 360 };
    private readonly ComboBox _mode = new() { ItemsSource = new[] { "keyword", "hybrid", "semantic" }, SelectedIndex = 0 };
    private readonly ListView _hits = new() { Height = 220, SelectionMode = ListViewSelectionMode.Multiple };
    private readonly ListView _chapters = new() { Height = 140 };
    private readonly ListView _jobs = new() { Height = 150 };
    private readonly ComboBox _timelineMode = new() { ItemsSource = new[] { "Quick", "Detailed" }, SelectedIndex = 0, Width = 200 };
    private readonly TextBox _title = new() { PlaceholderText = "Chapter title", MaxLength = 200 };
    private readonly TextBox _summary = new() { PlaceholderText = "Description", MaxLength = 4000 };
    private readonly NumberBox _start = new() { Header = "Start seconds", Value = 0, Minimum = 0, Width = 160 };
    private readonly NumberBox _end = new() { Header = "End seconds", Value = 0, Minimum = 0, Width = 160 };
    private readonly EvidencePanel _evidence;
    private readonly MomentTimeline _timeline;
    private readonly TextBlock _coverage = Text("Pick a result to play it, see which parts FileID has looked at, and add or fix chapters.");
    private readonly HashSet<(string ID, double UpdatedAt)> _refreshedTimelineJobs = new();
    private long _detailRevision;
    private readonly Button _pause;
    private readonly Button _cancel;
    private readonly Button _resume;
    private readonly Button _bestTakes;
    private readonly DispatcherTimer _poll = new() { Interval = TimeSpan.FromSeconds(3) };
    private readonly DispatcherTimer _playhead = new() { Interval = TimeSpan.FromMilliseconds(250) };
    private bool _polling;
    private long _jobsRevision;
    private string? _editingID;
    private readonly TextBlock _resultsCount = Meta(HitPresentation.ResultsMeta(0, searched: false));
    private readonly ProgressBar _searchBar = new() { IsIndeterminate = true, Width = 80, Visibility = Visibility.Collapsed };
    private readonly TextBlock _hitsEmpty = Meta("Results show here: matching file names, descriptions, chapters and moments inside videos.");
    private readonly Grid _notice;
    private readonly Border _playerPanel;
    private readonly Border _chaptersPanel;
    private readonly TextBlock _playerName;
    private readonly TextBlock _playerFolder;
    private readonly TextBlock _playerMeta;
    private readonly Brush _text1 = ThemeHelper.GetBrushSafe("Text1Brush");
    private readonly Brush _text2 = ThemeHelper.GetBrushSafe("Text2Brush");
    private readonly Brush _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
    private readonly Brush _ai = ThemeHelper.GetBrushSafe("AiBrush");
    private readonly Brush _thumbFill = new SolidColorBrush(Color.FromArgb(0x0A, 0xFF, 0xFF, 0xFF));
    private readonly Dictionary<Grid, (Image Image, string Path)> _pendingThumbs = new();
    private ThumbnailService? _thumbs;
    private IReadOnlyList<CatalogHit> _lastHits = Array.Empty<CatalogHit>();
    private IReadOnlyList<CatalogChapter> _chapterList = Array.Empty<CatalogChapter>();
    private CatalogHit? _selectedHit;
    private TimelineTracks _tracks = TimelineTracks.Empty;
    private long _tracksRevision;
    private long? _fileID;

    public CatalogWorkbench() : base("Search & Moments")
    {
        _evidence = new EvidencePanel(message => Status.Text = message);
        _evidence.HideLabel();
        _evidence.PlayerChanged += (_, _) => DebugLog.SafeRun("CatalogWorkbench.PlayerChanged", OnPlayerChanged);
        _timeline = new MomentTimeline(seconds => _ = DebugLog.SafeRunAsync("CatalogWorkbench.Seek", () => _evidence.SeekAsync(seconds)));

        _query.MinWidth = 0;
        _query.KeyDown += (_, e) =>
        {
            if (e.Key != Windows.System.VirtualKey.Enter) return;
            e.Handled = true;
            Run(SearchAsync);
        };
        _mode.HorizontalAlignment = HorizontalAlignment.Stretch;
        var searchTop = new StackPanel { Spacing = 10, Padding = new Thickness(14) };
        searchTop.Children.Add(_query);
        var searchControls = new Grid { ColumnSpacing = 6 };
        searchControls.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        searchControls.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        searchControls.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var search = Primary(ActionButton("Search", SearchAsync));
        var keyword = Secondary(ActionButton("Keyword", ct => SearchAsync("keyword", ct)));
        ToolTipService.SetToolTip(keyword, "Search names, tags and text only. Works without a model.");
        ToolTipService.SetToolTip(_timelineMode, "Quick looks at one frame every few seconds. Detailed watches short sequences and listens for speech, and takes longer.");
        var findMoments = Primary(ActionButton("Find moments", EnqueueTimelineAsync));
        ToolTipService.SetToolTip(findMoments, "Watch the chosen videos and suggest chapters. Uses the description model; if it isn't ready, describe one file on the Describe Files page first.");
        Grid.SetColumn(search, 1);
        Grid.SetColumn(keyword, 2);
        searchControls.Children.Add(_mode);
        searchControls.Children.Add(search);
        searchControls.Children.Add(keyword);
        searchTop.Children.Add(searchControls);

        _bestTakes = Secondary(new Button { Content = "Best takes", Height = 28, MinHeight = 28, IsEnabled = false });
        ToolTipService.SetToolTip(_bestTakes, TakeGroupsPolicy.SearchTooltip(_lastHits));
        _bestTakes.Click += (_, _) => DebugLog.SafeRun("CatalogWorkbench.BestTakes", () =>
        {
            if (TakeGroupsPolicy.CanOpenFromSearch(_lastHits)) AppViewModel.Instance.RequestBestTakes(_lastHits);
        });
        var resultsRow = new Grid { ColumnSpacing = 8 };
        resultsRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        resultsRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _resultsCount.FontSize = 12;
        _resultsCount.VerticalAlignment = VerticalAlignment.Center;
        _searchBar.HorizontalAlignment = HorizontalAlignment.Left;
        _searchBar.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(_bestTakes, 1);
        resultsRow.Children.Add(_resultsCount);
        resultsRow.Children.Add(_searchBar);
        resultsRow.Children.Add(_bestTakes);
        searchTop.Children.Add(resultsRow);

        var resultsPane = new Grid();
        resultsPane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        resultsPane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        resultsPane.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var hairline = new Border { Height = 1, Background = ThemeHelper.GetBrushSafe("HairlineBrush") };
        _hits.Height = double.NaN;
        _hits.Padding = new Thickness(6, 8, 6, 8);
        _hitsEmpty.TextAlignment = TextAlignment.Center;
        _hitsEmpty.Margin = new Thickness(24);
        _hitsEmpty.VerticalAlignment = VerticalAlignment.Center;
        _hitsEmpty.IsHitTestVisible = false;
        Grid.SetRow(hairline, 1);
        Grid.SetRow(_hits, 2);
        Grid.SetRow(_hitsEmpty, 2);
        resultsPane.Children.Add(searchTop);
        resultsPane.Children.Add(hairline);
        resultsPane.Children.Add(_hits);
        resultsPane.Children.Add(_hitsEmpty);
        UseSidePane(resultsPane, 300, paneOnLeft: true);

        _notice = DashedNoticeBox(
            "Find a moment",
            "Type a word on the left and press Enter. Pick a result to play it, see which parts FileID has looked at, and add or fix chapters.",
            out _,
            out _);
        Inputs.Children.Add(_notice);

        _playerName = TextLine("", 13, _text1, "MonoFontFamily");
        _playerName.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        _playerName.IsTextSelectionEnabled = true;
        _playerFolder = TextLine("", 11, _text3, "MonoFontFamily");
        _playerMeta = TextLine("", 12, _text3);
        var fileBlock = new StackPanel { Spacing = 3 };
        fileBlock.Children.Add(_playerName);
        fileBlock.Children.Add(_playerFolder);
        _playerPanel = V3Panel("Player", _playerMeta,
            fileBlock,
            _evidence,
            _timeline,
            _coverage,
            Row(_timelineMode, findMoments, Ghost(ActionButton("Refresh chapters", RefreshDetailAsync))),
            Meta("Pick one or more videos in the results, then press Find moments. Quick looks at one frame every ten seconds; Detailed also watches short four-second sequences and takes longer. With a speech model from the Models page (such as Qwen3-ASR), speech becomes searchable text on this PC. Nothing is downloaded automatically, and problems show under Video analysis."),
            Meta("Keyword search works without a model. Hybrid and semantic search need the Search by description model. FileID looks at frames, so very short moments can be missed."));
        _playerPanel.Visibility = Visibility.Collapsed;
        Inputs.Children.Add(_playerPanel);

        var undo = Ghost(ActionButton("Undo last edit", ct => MutateAsync(CatalogRequest("undoChapterEdit", fileID: RequireFile()), ct)));
        var addChapter = Secondary(ActionButton("+ Add chapter", _ => { ResetEditor(); return Task.CompletedTask; }));
        _chaptersPanel = V3Panel("Chapters", Row(undo, addChapter),
            Meta("Suggested chapters are drafts. Check what happens and when before you accept them."),
            _chapters,
            SectionLabel("Edit chapter"),
            _title,
            _summary,
            Row(_start, _end),
            Row(Primary(ActionButton("Save chapter", SaveAsync)), Ghost(ActionButton("Delete chapter", DeleteAsync))),
            Meta("Chapters are saved in FileID. The video file itself is never changed."));
        _chaptersPanel.Visibility = Visibility.Collapsed;
        Inputs.Children.Add(_chaptersPanel);

        _pause = Secondary(ActionButton("Pause", ct => JobAsync("pauseJob", ct)));
        _cancel = Ghost(ActionButton("Cancel", ct => JobAsync("cancelJob", ct)));
        _resume = Styled(ActionButton("Resume", ct => JobAsync("resumeJob", ct)), "ShellSecondaryButtonStyle");
        ToolTipService.SetToolTip(_resume, "Resume or retry the selected job");
        Inputs.Children.Add(V3Panel("Video analysis", Ghost(ActionButton("Refresh", RefreshJobsAsync)),
            Meta("Progress counts finished work, not event-detection coverage. FileID looks at frames, so very short moments can be missed."),
            _jobs,
            Row(_pause, _resume, _cancel)));
        _hits.ContainerContentChanging += (_, args) => DebugLog.SafeRun("CatalogWorkbench.RowRealized", () =>
        {
            if (!args.InRecycleQueue && args.Item is Grid row) LoadRowThumb(row);
        });
        _hits.SelectionChanged += (_, e) => DebugLog.SafeRun("CatalogWorkbench.HitSelected", () =>
        {
            if ((e.AddedItems.LastOrDefault() ?? _hits.SelectedItems.LastOrDefault()) is not FrameworkElement { Tag: CatalogHit hit })
            {
                _fileID = null;
                SelectHit(null);
                return;
            }
            if (_fileID == hit.FileID && ReferenceEquals(_selectedHit, hit)) return;
            _fileID = hit.FileID;
            SelectHit(hit);
            ResetEditor();
            _chapters.ItemsSource = null;
            Run(RefreshDetailAsync);
        });
        _chapters.SelectionChanged += (_, _) =>
        {
            if (_chapters.SelectedItem is not ChapterItem item) return;
            var chapter = item.Chapter;
            _editingID = chapter.Id;
            _title.Text = chapter.Title;
            _summary.Text = chapter.Summary;
            _start.Value = chapter.StartSeconds;
            _end.Value = chapter.EndSeconds;
        };
        _jobs.SelectionChanged += (_, _) => UpdateControls();
        _poll.Tick += (_, _) => _ = PollAsync();
        _playhead.Tick += (_, _) => DebugLog.SafeRun("CatalogWorkbench.Playhead", TickPlayhead);
        UpdateControls();
    }

    protected override void OnOpened()
    {
        _thumbs ??= new ThumbnailService();
        _poll.Start();
        _ = PollAsync();
        if (_evidence.Player is not null) _playhead.Start();
    }

    protected override void OnClosed()
    {
        _poll.Stop();
        _playhead.Stop();
        _thumbs?.Dispose();
        _thumbs = null;
    }

    protected override void UpdateControls()
    {
        var job = (_jobs.SelectedItem as JobItem)?.Job;
        _pause.IsEnabled = CatalogJobActions.CanPause(job?.State);
        _cancel.IsEnabled = CatalogJobActions.CanCancel(job?.State);
        _resume.IsEnabled = CatalogJobActions.CanResume(job?.Kind, job?.State);
    }
    private long RequireFile() => _fileID ?? throw new InvalidOperationException("Pick a result first.");
    private void ResetEditor() { _editingID = null; _title.Text = ""; _summary.Text = ""; _start.Value = 0; _end.Value = 0; }

    private IEnumerable<CatalogHit> SelectedHits() =>
        _hits.SelectedItems.OfType<FrameworkElement>().Select(row => row.Tag).OfType<CatalogHit>();

    // ---- Results ----------------------------------------------------------------

    private void RenderHits(IReadOnlyList<CatalogHit> hits, bool searched)
    {
        _lastHits = hits;
        _pendingThumbs.Clear();
        _hits.Items.Clear();
        for (var i = 0; i < hits.Count; i++)
        {
            var row = ResultRow(hits[i]);
            _hits.Items.Add(row);
            if (i < EagerThumbnails) LoadRowThumb(row);
        }
        _resultsCount.Text = HitPresentation.ResultsMeta(hits.Count, searched);
        _hitsEmpty.Visibility = hits.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        _bestTakes.IsEnabled = TakeGroupsPolicy.CanOpenFromSearch(hits);
        ToolTipService.SetToolTip(_bestTakes, TakeGroupsPolicy.SearchTooltip(hits));
    }

    /// <summary>Result rail row (DESIGN_V3_PORT "Search &amp; Moments", Results): thumbnail, file name,
    /// time / page / kind line (lavender for unchecked video frames) and the matched text.</summary>
    private Grid ResultRow(CatalogHit hit)
    {
        var image = new Image { Stretch = Stretch.UniformToFill };
        var thumb = new Border
        {
            Width = 40,
            Height = 40,
            CornerRadius = new CornerRadius(6),
            Background = _thumbFill,
            VerticalAlignment = VerticalAlignment.Top,
            Margin = new Thickness(0, 2, 0, 0),
            Child = image,
        };
        AutomationProperties.SetAccessibilityView(thumb, AccessibilityView.Raw);
        var name = TextLine(HitPresentation.FileName(hit.Path), 12.5, _text1, "MonoFontFamily");
        name.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        var meta = TextLine(HitPresentation.ResultMeta(hit), 11.5, HitPresentation.IsSampled(hit) ? _ai : _text3);
        var text = new StackPanel { Spacing = 3, VerticalAlignment = VerticalAlignment.Center };
        text.Children.Add(name);
        text.Children.Add(meta);
        if (!string.IsNullOrWhiteSpace(hit.Text))
        {
            var snippet = TextLine(hit.Text, 12, _text2);
            snippet.TextWrapping = TextWrapping.Wrap;
            snippet.MaxLines = 2;
            text.Children.Add(snippet);
        }
        var row = new Grid { ColumnSpacing = 10, Padding = new Thickness(2, 7, 2, 7), Tag = hit };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(text, 1);
        row.Children.Add(thumb);
        row.Children.Add(text);
        AutomationProperties.SetName(row, HitPresentation.ResultSpoken(hit));
        AutomationProperties.SetHelpText(row, hit.Text);
        ToolTipService.SetToolTip(row, hit.Path);
        _pendingThumbs[row] = (image, hit.Path);
        return row;
    }

    private void LoadRowThumb(Grid row)
    {
        if (!_pendingThumbs.Remove(row, out var pending) || _thumbs is not { } thumbs) return;
        var ui = DispatcherQueue;
        var ct = Lifetime;
        _ = DebugLog.SafeRunAsync("CatalogWorkbench.Thumbnail", async () =>
        {
            var bitmap = await thumbs.RequestAsync(pending.Path, null, ct).ConfigureAwait(false);
            if (bitmap is null || ct.IsCancellationRequested) return;
            ui.TryEnqueue(() =>
            {
                if (!ct.IsCancellationRequested) pending.Image.Source = bitmap;
            });
        });
    }

    // ---- Player -------------------------------------------------------------------

    private void SelectHit(CatalogHit? hit)
    {
        _selectedHit = hit;
        _chapterList = Array.Empty<CatalogChapter>();
        _tracks = TimelineTracks.Empty;
        Interlocked.Increment(ref _tracksRevision);
        _evidence.Select(hit);
        _timeline.Reset();
        var selected = hit is not null;
        _notice.Visibility = selected ? Visibility.Collapsed : Visibility.Visible;
        _playerPanel.Visibility = selected ? Visibility.Visible : Visibility.Collapsed;
        _chaptersPanel.Visibility = selected ? Visibility.Visible : Visibility.Collapsed;
        if (hit is null) return;
        _playerName.Text = HitPresentation.FileName(hit.Path);
        _playerFolder.Text = HitPresentation.Folder(hit.Path);
        ToolTipService.SetToolTip(_playerName, hit.Path);
        ToolTipService.SetToolTip(_playerFolder, HitPresentation.Folder(hit.Path));
        _timeline.Visibility = TakeGroupsPolicy.IsVideoPath(hit.Path) ? Visibility.Visible : Visibility.Collapsed;
        _playerMeta.Text = HitPresentation.ResultMeta(hit);
        LoadTracks(hit);
    }

    /// <summary>Reads the Examined and Speech lanes from the read-only catalog on a worker
    /// thread; a stale answer for an earlier selection is dropped.</summary>
    private void LoadTracks(CatalogHit hit)
    {
        if (!TakeGroupsPolicy.IsVideoPath(hit.Path)) return;
        var revision = Interlocked.Increment(ref _tracksRevision);
        var fileID = hit.FileID;
        var ct = Lifetime;
        var ui = DispatcherQueue;
        _ = DebugLog.SafeRunAsync("CatalogWorkbench.Tracks", async () =>
        {
            TimelineTracks tracks;
            try
            {
                tracks = await Task.Run(async () =>
                {
                    await using var store = new ReadStore(AppPaths.DbPath);
                    await store.OpenAsync(ct).ConfigureAwait(false);
                    return await store.TimelineTracksAsync(fileID, ct).ConfigureAwait(false);
                }, ct).ConfigureAwait(false);
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested)
            {
                return;
            }
            ui.TryEnqueue(() =>
            {
                if (ct.IsCancellationRequested || revision != Interlocked.Read(ref _tracksRevision) || _fileID != fileID) return;
                _tracks = tracks;
                _timeline.SetTracks(tracks.Examined, tracks.Speech, KnownDuration());
            });
        });
    }

    private void OnPlayerChanged()
    {
        if (_evidence.Player is not null)
        {
            _playhead.Start();
            return;
        }
        _playhead.Stop();
        _timeline.Update(MomentTimelinePolicy.Duration(null, _chapterList, _tracks.Examined, _tracks.Speech), 0);
        if (_selectedHit is { } hit) _playerMeta.Text = HitPresentation.ResultMeta(hit);
    }

    private void TickPlayhead()
    {
        if (_evidence.Player is not { } player)
        {
            _playhead.Stop();
            return;
        }
        double natural;
        double position;
        try
        {
            var session = player.PlaybackSession;
            natural = session.NaturalDuration.TotalSeconds;
            position = session.Position.TotalSeconds;
        }
        catch (Exception ex) when (ex is ObjectDisposedException or System.Runtime.InteropServices.COMException)
        {
            _playhead.Stop();
            return;
        }
        _timeline.Update(MomentTimelinePolicy.Duration(natural, _chapterList, _tracks.Examined, _tracks.Speech), position);
        if (natural > 0) _playerMeta.Text = MomentTimelinePolicy.PlayerMeta(position, natural);
    }

    private double KnownDuration()
    {
        try
        {
            return MomentTimelinePolicy.Duration(_evidence.Player?.PlaybackSession.NaturalDuration.TotalSeconds, _chapterList, _tracks.Examined, _tracks.Speech);
        }
        catch (Exception ex) when (ex is ObjectDisposedException or System.Runtime.InteropServices.COMException)
        {
            return MomentTimelinePolicy.Duration(null, _chapterList, _tracks.Examined, _tracks.Speech);
        }
    }

    // ---- Requests -----------------------------------------------------------------

    private Task SearchAsync(CancellationToken ct) => SearchAsync((string)_mode.SelectedItem, ct);
    private async Task SearchAsync(string mode, CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(_query.Text)) throw new InvalidOperationException("Type a word to search for first.");
        await UI(() =>
        {
            _resultsCount.Visibility = Visibility.Collapsed;
            _searchBar.Visibility = Visibility.Visible;
        }, ct);
        CatalogResponse result;
        try
        {
            result = await Engine.CatalogSearchAsync(_query.Text, mode, ct);
        }
        finally
        {
            await UI(() =>
            {
                _searchBar.Visibility = Visibility.Collapsed;
                _resultsCount.Visibility = Visibility.Visible;
            }, ct);
        }
        await UI(() =>
        {
            Status.Text = result.Message ?? (result.Status != "ok"
                ? "Search didn't finish."
                : result.Hits.Count == 0
                    ? "No matches. Try another word, or describe more files so there is more to search."
                    : $"{result.Hits.Count:N0} match{(result.Hits.Count == 1 ? "" : "es")}.");
            if (result.Status != "ok" && mode != "keyword") Status.Text += " Try Keyword instead.";
            IReadOnlyList<CatalogHit> hits = result.Status == "ok" ? result.Hits : Array.Empty<CatalogHit>();
            RenderHits(hits, searched: true);
            _fileID = null;
            Interlocked.Increment(ref _detailRevision);
            _coverage.Text = "Pick a result to play it, see which parts FileID has looked at, and add or fix chapters.";
            SelectHit(null);
            _chapters.ItemsSource = null;
            ResetEditor();
        }, ct);
    }
    private async Task RefreshDetailAsync(CancellationToken ct)
    {
        var file = RequireFile();
        var revision = Interlocked.Increment(ref _detailRevision);
        var result = await Engine.CatalogAsync(CatalogRequest("detail", fileID: file), ct);
        await UI(() =>
        {
            if (_fileID != file || revision != Interlocked.Read(ref _detailRevision)) return;
            Status.Text = result.Message ?? (result.Status == "ok" ? "Chapters loaded." : "Couldn't load chapters.");
            _coverage.Text = result.Chapters.Count == 0
                ? "No chapters yet. Press Find moments for suggestions, or add your own."
                : $"{result.Chapters.Count} chapter{(result.Chapters.Count == 1 ? "" : "s")} · {result.Chapters.Count(chapter => chapter.Stale)} out of date · {result.Chapters.Count(chapter => !chapter.UserEdited)} suggested. Suggested chapters come from a few frames, so short moments can be missed. Check them before relying on them.";
            _chapters.ItemsSource = result.Chapters.Select(chapter => new ChapterItem(chapter)).ToArray();
            _chapterList = result.Status == "ok" ? result.Chapters : Array.Empty<CatalogChapter>();
            _timeline.SetChapters(_chapterList, KnownDuration());
            if (_selectedHit is { } selected && selected.FileID == file) LoadTracks(selected);
        }, ct);
    }
    private Task SaveAsync(CancellationToken ct)
    {
        var file = RequireFile();
        if (string.IsNullOrWhiteSpace(_title.Text) || !double.IsFinite(_start.Value) || !double.IsFinite(_end.Value)
            || _start.Value < 0 || _end.Value < _start.Value)
        {
            throw new InvalidOperationException("Type a title, and set the end time at or after the start.");
        }

        var chapter = new CatalogChapter(_editingID ?? Guid.NewGuid().ToString(), file, _start.Value, _end.Value,
            _title.Text, _summary.Text, "", "user", 1, true, false);
        return MutateAsync(CatalogRequest("saveChapter", chapter: chapter), ct);
    }
    private async Task DeleteAsync(CancellationToken ct)
    {
        if (_editingID is null) throw new InvalidOperationException("Pick a chapter first.");
        if (!await ConfirmAsync("Delete chapter?", "This removes the chapter from FileID. Undo last edit puts it back.")) return;
        await MutateAsync(CatalogRequest("deleteChapter", fileID: RequireFile(), chapterID: _editingID), ct);
    }
    private async Task MutateAsync(CatalogRequest request, CancellationToken ct)
    {
        var result = await Engine.CatalogAsync(request, ct);
        await UI(() => Status.Text = result.Message ?? (result.Status == "ok" ? "Chapter saved." : "Couldn't save the chapter."), ct);
        if (result.Status != "ok") return;
        await RefreshDetailAsync(ct);
        await UI(ResetEditor, ct);
    }
    private async Task EnqueueTimelineAsync(CancellationToken ct)
    {
        var mode = _timelineMode.SelectedIndex == 1 ? "moments" : "sampled";
        var request = CatalogJobActions.TimelineRequest(SelectedHits(), mode);
        var revision = Interlocked.Increment(ref _jobsRevision);
        var response = await Engine.CatalogAsync(request, ct);
        await UI(() =>
        {
            Status.Text = response.Message ?? (response.Status == "ok"
                ? "Finding moments. Progress shows under Video analysis."
                : $"Couldn't start finding moments ({response.Status}).");
            if (response.Status == "ok" && revision == Interlocked.Read(ref _jobsRevision)) ApplyJobs(response.Jobs);
        }, ct);
    }
    private async Task JobAsync(string action, CancellationToken ct)
    {
        if (_jobs.SelectedItem is not JobItem item) return;
        var request = CatalogJobActions.ControlRequest(action, item.Job);
        var revision = Interlocked.Increment(ref _jobsRevision);
        var result = await Engine.CatalogAsync(request, ct);
        await UI(() =>
        {
            Status.Text = result.Message ?? $"Job action: {result.Status}";
            if (result.Status == "ok" && revision == Interlocked.Read(ref _jobsRevision)) ApplyJobs(result.Jobs);
        }, ct);
    }
    private async Task RefreshJobsAsync(CancellationToken ct)
    {
        var revision = Interlocked.Increment(ref _jobsRevision);
        var result = await Engine.CatalogAsync(CatalogRequest("jobs"), ct);
        var refreshDetail = false;
        await UI(() =>
        {
            if (revision != Interlocked.Read(ref _jobsRevision)) return;
            if (result.Status == "ok")
            {
                ApplyJobs(result.Jobs);
                refreshDetail = _fileID is { } file && result.Jobs.Any(job => job.Kind == "timelineSample"
                    && job.State == "completed" && job.FileIDs.Contains(file) && _refreshedTimelineJobs.Add((job.Id, job.UpdatedAt)));
            }
            else
            {
                Status.Text = result.Message ?? "Job refresh failed.";
            }
        }, ct);
        if (refreshDetail && _fileID is not null) await RefreshDetailAsync(ct);
    }
    private async Task PollAsync()
    {
        if (_polling || Busy || Lifetime.IsCancellationRequested) return;
        _polling = true;
        var ct = Lifetime;
        try { await RefreshJobsAsync(ct); }
        catch (OperationCanceledException) when (ct.IsCancellationRequested) { }
        catch (Exception ex) { await UI(() => Status.Text = ex.Message, ct); }
        finally { _polling = false; }
    }
    private void ApplyJobs(IReadOnlyList<CatalogJob> jobs)
    {
        var selected = (_jobs.SelectedItem as JobItem)?.Job.Id;
        var items = jobs.Select(job => new JobItem(job)).ToArray();
        _jobs.ItemsSource = items;
        _jobs.SelectedItem = items.FirstOrDefault(item => item.Job.Id == selected);
        UpdateControls();
    }
    private sealed record ChapterItem(CatalogChapter Chapter)
    {
        public override string ToString() => $"{Chapter.StartSeconds:0.0}–{Chapter.EndSeconds:0.0}s · {Chapter.Title}"
            + (Chapter.Stale ? " · Out of date" : "")
            + (!Chapter.UserEdited ? " · Suggested, check it" : " · Edited by you")
            + $"\n{Chapter.Summary}\n{Chapter.ModelVersion} · confidence {Chapter.Confidence:P0}";
    }
    private sealed record JobItem(CatalogJob Job)
    {
        public override string ToString() => $"{(Job.Kind == "catalogIndex" ? "Search index" : "Video analysis")} · {Job.State} · {Job.Progress:P0} · {Job.FileIDs.Count} file(s)\nJob {Job.Id} · Files: {string.Join(", ", Job.FileIDs)}\n{Job.Error}";
    }
}
