using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Workbenches;

internal sealed class CatalogWorkbench : WorkbenchPanel
{
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
    private readonly TextBlock _coverage = Text("Type a word on the left and press Enter. Pick a result to play it, see which parts FileID has looked at, and add or fix chapters.");
    private readonly HashSet<(string ID, double UpdatedAt)> _refreshedTimelineJobs = new();
    private long _detailRevision;
    private readonly Button _pause;
    private readonly Button _cancel;
    private readonly Button _resume;
    private readonly DispatcherTimer _poll = new() { Interval = TimeSpan.FromSeconds(3) };
    private bool _polling;
    private long _jobsRevision;
    private string? _editingID;
    private readonly TextBlock _resultsCount = Meta("");
    private long? _fileID;

    public CatalogWorkbench() : base("Search & Moments")
    {
        _evidence = new EvidencePanel(message => Status.Text = message);

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
        searchTop.Children.Add(_resultsCount);
        var resultsPane = new Grid();
        resultsPane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        resultsPane.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        _hits.Height = double.NaN;
        _hits.Padding = new Thickness(6, 8, 6, 8);
        Grid.SetRow(_hits, 1);
        resultsPane.Children.Add(searchTop);
        resultsPane.Children.Add(_hits);
        UseSidePane(resultsPane, 300, paneOnLeft: true);

        Inputs.Children.Add(V3Panel("Player", null,
            _evidence,
            _coverage,
            Row(_timelineMode, findMoments, Ghost(ActionButton("Refresh chapters", RefreshDetailAsync))),
            Meta("Pick one or more videos in the results, then press Find moments. Quick looks at one frame every ten seconds; Detailed also watches short four-second sequences and takes longer. With a speech model from the Models page (such as Qwen3-ASR), speech becomes searchable text on this PC. Nothing is downloaded automatically, and problems show under Video analysis."),
            Meta("Keyword search works without a model. Hybrid and semantic search need the Search by description model. FileID looks at frames, so very short moments can be missed.")));

        var undo = Ghost(ActionButton("Undo last edit", ct => MutateAsync(CatalogRequest("undoChapterEdit", fileID: RequireFile()), ct)));
        var addChapter = Secondary(ActionButton("+ Add chapter", _ => { ResetEditor(); return Task.CompletedTask; }));
        Inputs.Children.Add(V3Panel("Chapters", Row(undo, addChapter),
            Meta("Suggested chapters are drafts. Check what happens and when before you accept them."),
            _chapters,
            SectionLabel("Edit chapter"),
            _title,
            _summary,
            Row(_start, _end),
            Row(Primary(ActionButton("Save chapter", SaveAsync)), Ghost(ActionButton("Delete chapter", DeleteAsync))),
            Meta("Chapters are saved in FileID. The video file itself is never changed.")));

        _pause = Secondary(ActionButton("Pause", ct => JobAsync("pauseJob", ct)));
        _cancel = Ghost(ActionButton("Cancel", ct => JobAsync("cancelJob", ct)));
        _resume = Styled(ActionButton("Resume", ct => JobAsync("resumeJob", ct)), "ShellSecondaryButtonStyle");
        ToolTipService.SetToolTip(_resume, "Resume or retry the selected job");
        Inputs.Children.Add(V3Panel("Video analysis", Ghost(ActionButton("Refresh", RefreshJobsAsync)),
            Meta("Progress counts finished work, not event-detection coverage. FileID looks at frames, so very short moments can be missed."),
            _jobs,
            Row(_pause, _resume, _cancel)));
        _hits.SelectionChanged += (_, e) =>
        {
            if ((e.AddedItems.LastOrDefault() ?? _hits.SelectedItems.LastOrDefault()) is not HitItem item)
            {
                _fileID = null;
                _evidence.Select(null);
                _chapters.ItemsSource = null;
                return;
            }
            _fileID = item.Hit.FileID;
            _evidence.Select(item.Hit);
            ResetEditor();
            _chapters.ItemsSource = null;
            Run(RefreshDetailAsync);
        };
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
        UpdateControls();
    }

    protected override void OnOpened() { _poll.Start(); _ = PollAsync(); }
    protected override void OnClosed() => _poll.Stop();
    protected override void UpdateControls()
    {
        var job = (_jobs.SelectedItem as JobItem)?.Job;
        _pause.IsEnabled = CatalogJobActions.CanPause(job?.State);
        _cancel.IsEnabled = CatalogJobActions.CanCancel(job?.State);
        _resume.IsEnabled = CatalogJobActions.CanResume(job?.Kind, job?.State);
    }
    private long RequireFile() => _fileID ?? throw new InvalidOperationException("Pick a result first.");
    private void ResetEditor() { _editingID = null; _title.Text = ""; _summary.Text = ""; _start.Value = 0; _end.Value = 0; }

    private Task SearchAsync(CancellationToken ct) => SearchAsync((string)_mode.SelectedItem, ct);
    private async Task SearchAsync(string mode, CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(_query.Text)) throw new InvalidOperationException("Type a word to search for first.");
        var result = await Engine.CatalogSearchAsync(_query.Text, mode, ct);
        await UI(() =>
        {
            Status.Text = result.Message ?? (result.Status != "ok"
                ? "Search didn't finish."
                : result.Hits.Count == 0
                    ? "No matches. Try another word, or describe more files so there is more to search."
                    : $"{result.Hits.Count:N0} match{(result.Hits.Count == 1 ? "" : "es")}.");
            if (result.Status != "ok" && mode != "keyword") Status.Text += " Try Keyword instead.";
            _hits.ItemsSource = result.Status == "ok" ? result.Hits.Select(hit => new HitItem(hit)).ToArray() : Array.Empty<HitItem>();
            var count = result.Status == "ok" ? result.Hits.Count : 0;
            _resultsCount.Text = $"{count:N0} result{(count == 1 ? "" : "s")}";
            _fileID = null;
            Interlocked.Increment(ref _detailRevision);
            _coverage.Text = "Pick a result to play it, see which parts FileID has looked at, and add or fix chapters.";
            _evidence.Select(null);
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
        var request = CatalogJobActions.TimelineRequest(_hits.SelectedItems.Cast<HitItem>().Select(item => item.Hit), mode);
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
