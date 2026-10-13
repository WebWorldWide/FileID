using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Workbenches;

internal sealed class CatalogWorkbench : WorkbenchPanel
{
    private readonly TextBox _query = new() { PlaceholderText = "Search indexed names, descriptions, chapters or sampled frames", MaxLength = 2000, MinWidth = 360 };
    private readonly ComboBox _mode = new() { ItemsSource = new[] { "keyword", "hybrid", "semantic" }, SelectedIndex = 0 };
    private readonly ListView _hits = new() { Height = 220, SelectionMode = ListViewSelectionMode.Multiple };
    private readonly ListView _chapters = new() { Height = 140 };
    private readonly ListView _jobs = new() { Height = 150 };
    private readonly ComboBox _timelineMode = new() { ItemsSource = new[] { "Quick samples", "Significant moments" }, SelectedIndex = 0, Width = 200 };
    private readonly TextBox _title = new() { PlaceholderText = "Chapter title", MaxLength = 200 };
    private readonly TextBox _summary = new() { PlaceholderText = "Description", MaxLength = 4000 };
    private readonly NumberBox _start = new() { Header = "Start seconds", Value = 0, Minimum = 0, Width = 160 };
    private readonly NumberBox _end = new() { Header = "End seconds", Value = 0, Minimum = 0, Width = 160 };
    private readonly EvidencePanel _evidence;
    private readonly TextBlock _coverage = Text("No timeline detail selected. Sampling coverage is not full-video event verification.");
    private readonly HashSet<(string ID, double UpdatedAt)> _refreshedTimelineJobs = new();
    private long _detailRevision;
    private readonly Button _pause;
    private readonly Button _cancel;
    private readonly Button _resume;
    private readonly DispatcherTimer _poll = new() { Interval = TimeSpan.FromSeconds(3) };
    private bool _polling;
    private long _jobsRevision;
    private string? _editingID;
    private long? _fileID;

    public CatalogWorkbench() : base("Search & Moments")
    {
        _evidence = new EvidencePanel(message => Status.Text = message);
        Inputs.Children.Add(Row(_query, _mode, ActionButton("Search", SearchAsync), ActionButton("Search keyword", ct => SearchAsync("keyword", ct))));
        Inputs.Children.Add(Text("Keyword works without a model. Hybrid and semantic search require a real installed CLIP text model and matching engine index; unavailable models or indexing are reported explicitly. Search keyword is always available as a fallback. Sparse sampled frames are unverified and can miss brief events."));
        Inputs.Children.Add(_hits);
        Inputs.Children.Add(Row(_timelineMode, ActionButton("Analyze selected timelines / generate chapters", EnqueueTimelineAsync), ActionButton("Refresh selected chapters", RefreshDetailAsync)));
        Inputs.Children.Add(Text("Select video rows to enqueue their unique catalog files. Install a local visual model in Deep Analyze first; nothing is downloaded automatically. Quick samples caption one moment per ten seconds; significant moments also examine overlapping four-second frame sequences and take longer. Speech transcription is not yet available on Windows. Model, runtime and media failures are reported in jobs, not treated as completion."));
        Inputs.Children.Add(_evidence);
        Inputs.Children.Add(_coverage);
        Inputs.Children.Add(Text("Chapters · stale markers remain visible; draft suggestions from sparse samples require review."));
        Inputs.Children.Add(_chapters);
        Inputs.Children.Add(_title);
        Inputs.Children.Add(_summary);
        Inputs.Children.Add(Row(_start, _end));
        Inputs.Children.Add(Row(ActionButton("New chapter", _ => { ResetEditor(); return Task.CompletedTask; }),
            ActionButton("Add / save chapter", SaveAsync), ActionButton("Delete selected chapter", DeleteAsync),
            ActionButton("Undo last chapter edit", ct => MutateAsync(CatalogRequest("undoChapterEdit", fileID: RequireFile()), ct))));
        Inputs.Children.Add(Text("Markers are stored in the internal catalog. Original files are never changed. Generated chapter suggestions require review; job completion and sparse sampling do not establish full-video coverage."));
        Inputs.Children.Add(Text("Durable analysis jobs · progress is the engine's worker snapshot, not event-detection coverage"));
        Inputs.Children.Add(_jobs);
        _pause = ActionButton("Pause selected job", ct => JobAsync("pauseJob", ct));
        _cancel = ActionButton("Cancel selected job", ct => JobAsync("cancelJob", ct));
        _resume = ActionButton("Resume / retry selected job", ct => JobAsync("resumeJob", ct));
        Inputs.Children.Add(Row(ActionButton("Refresh jobs", RefreshJobsAsync), _pause, _resume, _cancel));
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
    private long RequireFile() => _fileID ?? throw new InvalidOperationException("Select a catalog file first.");
    private void ResetEditor() { _editingID = null; _title.Text = ""; _summary.Text = ""; _start.Value = 0; _end.Value = 0; }

    private Task SearchAsync(CancellationToken ct) => SearchAsync((string)_mode.SelectedItem, ct);
    private async Task SearchAsync(string mode, CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(_query.Text)) throw new InvalidOperationException("Enter a search query.");
        var result = await Engine.CatalogSearchAsync(_query.Text, mode, ct);
        await UI(() =>
        {
            Status.Text = result.Message ?? (result.Status == "ok" ? $"{result.Hits.Count} {mode} evidence matches." : "Search failed.");
            if (result.Status != "ok" && mode != "keyword") Status.Text += " Use Search keyword as a fallback.";
            _hits.ItemsSource = result.Status == "ok" ? result.Hits.Select(hit => new HitItem(hit)).ToArray() : Array.Empty<HitItem>();
            _fileID = null;
            Interlocked.Increment(ref _detailRevision);
            _coverage.Text = "Select evidence for chapter detail. No full-video coverage metric is available in this contract.";
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
            Status.Text = result.Message ?? (result.Status == "ok" ? "Catalog chapter markers loaded." : "Detail failed.");
            _coverage.Text = $"File {file} · {result.Chapters.Count} chapter marker(s) · {result.Chapters.Count(chapter => chapter.Stale)} stale · {result.Chapters.Count(chapter => !chapter.UserEdited)} generated. Sparse samples can miss brief events; speech may be absent or incomplete. Review generated chapters before relying on them.";
            _chapters.ItemsSource = result.Chapters.Select(chapter => new ChapterItem(chapter)).ToArray();
        }, ct);
    }
    private Task SaveAsync(CancellationToken ct)
    {
        var file = RequireFile();
        if (string.IsNullOrWhiteSpace(_title.Text) || !double.IsFinite(_start.Value) || !double.IsFinite(_end.Value)
            || _start.Value < 0 || _end.Value < _start.Value)
        {
            throw new InvalidOperationException("Enter a title and finite times with end at or after start.");
        }

        var chapter = new CatalogChapter(_editingID ?? Guid.NewGuid().ToString(), file, _start.Value, _end.Value,
            _title.Text, _summary.Text, "", "user", 1, true, false);
        return MutateAsync(CatalogRequest("saveChapter", chapter: chapter), ct);
    }
    private async Task DeleteAsync(CancellationToken ct)
    {
        if (_editingID is null) throw new InvalidOperationException("Select a chapter to delete.");
        if (!await ConfirmAsync("Delete chapter?", "This removes the internal catalog marker. You can undo the edit.")) return;
        await MutateAsync(CatalogRequest("deleteChapter", fileID: RequireFile(), chapterID: _editingID), ct);
    }
    private async Task MutateAsync(CatalogRequest request, CancellationToken ct)
    {
        var result = await Engine.CatalogAsync(request, ct);
        await UI(() => Status.Text = result.Message ?? (result.Status == "ok" ? "Chapter edit confirmed." : "Chapter edit failed."), ct);
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
            Status.Text = response.Message ?? $"Timeline enqueue: {response.Status}";
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
            + (Chapter.Stale ? " · STALE" : "")
            + (!Chapter.UserEdited ? " · GENERATED DRAFT (review required)" : " · USER EDITED")
            + $"\n{Chapter.Summary}\n{Chapter.ModelVersion} · confidence {Chapter.Confidence:P0}";
    }
    private sealed record JobItem(CatalogJob Job)
    {
        public override string ToString() => $"{Job.Kind} · {Job.State} · {Job.Progress:P0} · {Job.FileIDs.Count} file(s)\nJob {Job.Id} · Files: {string.Join(", ", Job.FileIDs)}\n{Job.Error}";
    }
}
