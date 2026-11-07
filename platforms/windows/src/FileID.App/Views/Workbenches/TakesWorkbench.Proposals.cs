using System.ComponentModel;
using System.Numerics;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Hosting;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Workbenches;

/// <summary>Library-wide related tries (macOS RelatedTriesSection): Find related tries queues the
/// takeGrouping job, its row shows progress with pause/resume/cancel, and the stored proposals open in
/// the same review page as possible groups, with a relation chip, the evidence and Save as group.</summary>
internal sealed partial class TakesWorkbench
{
    private const double ProposalResponse = 0.4;
    private const double ProposalDamping = 0.8;
    private const int SprungRows = 12;

    private IReadOnlyList<CatalogTakeAutoGroup> _proposals = Array.Empty<CatalogTakeAutoGroup>();
    private string? _proposalKey;
    private CatalogJob? _groupingJob;
    private JobRow? _groupingJobRow;
    private bool _proposalsLoaded;
    private bool _proposalsStale;

    private readonly Brush _info = ThemeHelper.GetBrushSafe("InfoBrush");
    private readonly Brush _infoWash = new SolidColorBrush(Color.FromArgb(0x1F, 0xA0, 0xE2, 0xEA));
    private readonly TextBlock _proposalHeader = SectionLabel("Related tries · 0");
    private readonly Button _findRelated = Secondary(new Button { Content = "Find related tries", Height = 28, MinHeight = 28 });
    private readonly StackPanel _groupingJobBox = new() { Visibility = Visibility.Collapsed, Margin = new Thickness(8, 0, 8, 6) };
    private readonly TextBlock _proposalsEmpty = Meta("");
    private readonly ListView _proposalList = new() { SelectionMode = ListViewSelectionMode.Single, MaxHeight = 300, Padding = new Thickness(0, 4, 0, 4) };
    private readonly Border _relationPill = new() { Padding = new Thickness(6, 2, 6, 2), CornerRadius = new CornerRadius(4), BorderThickness = new Thickness(1), Visibility = Visibility.Collapsed };
    private readonly TextBlock _relationText = new() { FontSize = 10.5, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
    private readonly TextBlock _reviewIntro = Meta("");
    private readonly Button _saveAsGroup = Secondary(new Button { Content = "Save as group", Height = 28, MinHeight = 28 });

    private StackPanel BuildProposalPane()
    {
        _proposalHeader.Foreground = _gold;
        _proposalHeader.VerticalAlignment = VerticalAlignment.Center;
        _findRelated.Foreground = _gold;
        _findRelated.BorderBrush = _gold;
        _findRelated.Background = _goldWash;
        ToolTipService.SetToolTip(_findRelated, "Look through the whole library for related tries. Runs on this PC.");
        AutomationProperties.SetName(_findRelated, "Find related tries across the library");
        _findRelated.Click += (_, _) => Run(FindRelatedAsync);
        var header = new Grid { ColumnSpacing = 8, Padding = new Thickness(14, 10, 8, 4) };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(_findRelated, 1);
        header.Children.Add(_proposalHeader);
        header.Children.Add(_findRelated);
        var intro = Meta("Tries and other angles of the same moment, found across your whole library. Suggestions never change your files.");
        intro.FontSize = 11.5;
        intro.Margin = new Thickness(14, 0, 14, 6);
        _proposalsEmpty.Margin = new Thickness(14, 6, 14, 6);
        _proposalsEmpty.Visibility = Visibility.Collapsed;
        AutomationProperties.SetName(_proposalList, "Related tries");
        _proposalList.ContainerContentChanging += (_, args) => DebugLog.SafeRun("TakesWorkbench.ProposalRealized", () =>
        {
            if (!args.InRecycleQueue && args.Item is Grid row) LoadRowThumbs(row);
        });
        _proposalList.SelectionChanged += (_, _) => DebugLog.SafeRun("TakesWorkbench.ProposalSelected", () =>
        {
            if (_rendering || _proposalList.SelectedIndex < 0 || _proposalList.SelectedIndex >= _proposals.Count) return;
            OpenProposal(_proposals[_proposalList.SelectedIndex]);
        });

        _relationText.Foreground = _text3;
        _relationPill.Child = _relationText;
        _relationPill.VerticalAlignment = VerticalAlignment.Bottom;
        _relationPill.Margin = new Thickness(0, 0, 0, 6);
        _reviewIntro.FontSize = 13;
        _saveAsGroup.Foreground = _gold;
        _saveAsGroup.BorderBrush = _gold;
        _saveAsGroup.Background = _goldWash;
        _saveAsGroup.Visibility = Visibility.Collapsed;
        ToolTipService.SetToolTip(_saveAsGroup, "Keep these together as a group with a name and a goal");
        _saveAsGroup.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.SaveProposal", () =>
        {
            if (_current is { } group && _proposalKey == group.GroupKey) SaveProposalAsGroup(group);
        });

        var pane = new StackPanel();
        pane.Children.Add(header);
        pane.Children.Add(intro);
        pane.Children.Add(_groupingJobBox);
        pane.Children.Add(_proposalsEmpty);
        pane.Children.Add(_proposalList);
        pane.Children.Add(new Border { Height = 1, Background = _hairline, Margin = new Thickness(0, 8, 0, 0) });
        return pane;
    }

    private void OnProposalEngineChanged(PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(EngineClient.LastCatalogResponse)) return;
        var response = Engine.LastCatalogResponse;
        if (!TakeProposalPolicy.IsJobPush(response)) return;
        DebugLog.Debug($"[ENGINE-SUB:TakesWorkbench] {e.PropertyName}=takeGrouping");
        var job = TakeProposalPolicy.Job(response!.Jobs);
        DispatcherQueue.TryEnqueue(() =>
        {
            if (!Lifetime.IsCancellationRequested) ApplyGroupingJob(job);
        });
    }

    private void ApplyGroupingJob(CatalogJob? job)
    {
        if (job is null) return;
        var previous = _groupingJob;
        _groupingJob = job;
        RenderGroupingJob();
        if (TakeProposalPolicy.Finished(previous, job)) _proposalsStale = true;
        RunStaleProposals();
    }

    private void RunStaleProposals()
    {
        if (!_proposalsStale || Busy || Lifetime.IsCancellationRequested) return;
        _proposalsStale = false;
        Run(ct => LoadProposalsAsync(null, ct));
    }

    private async Task FindRelatedAsync(CancellationToken ct)
    {
        var response = await Engine.CatalogAsync(TakeProposalPolicy.EnqueueRequest(Guid.NewGuid().ToString()), ct);
        await UI(() =>
        {
            Status.Text = "";
            if (!string.IsNullOrEmpty(response.Message)) ShowToast(response.Message, null, PlainToastWindow);
            if (response.Status == "ok" && TakeProposalPolicy.Job(response.Jobs) is { } job) ApplyGroupingJob(job);
        }, ct);
    }

    private async Task GroupingControlAsync(string action, CatalogJob job, CancellationToken ct)
    {
        var response = await Engine.CatalogAsync(CatalogJobActions.ControlRequest(action, job), ct);
        await UI(() =>
        {
            Status.Text = "";
            if (response.Status != "ok")
            {
                ShowToast(response.Message ?? "That didn't work. Try again in a moment.", null, PlainToastWindow);
                return;
            }
            if (TakeProposalPolicy.Job(response.Jobs) is { } next) ApplyGroupingJob(next);
        }, ct);
    }

    /// <summary>Reads the stored proposals. After <paramref name="resolved"/> is handled, opens the
    /// proposal that followed it so review flows on.</summary>
    private async Task LoadProposalsAsync(string? resolved, CancellationToken ct)
    {
        var response = await Engine.CatalogAsync(TakeProposalPolicy.ProposalsRequest(Guid.NewGuid().ToString()), ct);
        if (response.Status != "ok") return;
        var groups = response.TakeGroups ?? Array.Empty<CatalogTakeAutoGroup>();
        CatalogTakeAutoGroup? next = null;
        await UI(() =>
        {
            _proposalsLoaded = true;
            if (TakeProposalPolicy.Job(response.Jobs) is { } job) _groupingJob = job;
            RenderGroupingJob();
            next = RenderProposals(groups, resolved);
            Status.Text = "";
        }, ct);
        if (next is not null) await ScoreAsync(next, focusStrip: true, ct);
    }

    private CatalogTakeAutoGroup? RenderProposals(IReadOnlyList<CatalogTakeAutoGroup> groups, string? resolved)
    {
        var before = _proposals;
        _rendering = true;
        try
        {
            _proposals = groups;
            _proposalHeader.Text = TakeProposalPolicy.Header(groups);
            var showEmpty = _proposalsLoaded && groups.Count == 0 && _groupingJob?.State != "running";
            _proposalsEmpty.Text = TakeProposalPolicy.EmptyText(_groupingJob);
            _proposalsEmpty.Visibility = showEmpty ? Visibility.Visible : Visibility.Collapsed;
            _proposalList.Visibility = groups.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
            _proposalList.Items.Clear();
            for (var i = 0; i < groups.Count; i++)
            {
                var row = ProposalRow(groups[i]);
                _proposalList.Items.Add(row);
                if (i < EagerRowThumbnails) LoadRowThumbs(row);
                if (i < SprungRows && !before.Any(g => g.GroupKey == groups[i].GroupKey)) SpringIn(row);
            }
            if (_proposalKey is not { } key) return null;
            if (resolved is not null && key == resolved)
            {
                if (TakeProposalPolicy.NextAfter(before, groups, resolved) is { } index)
                {
                    _proposalKey = groups[index].GroupKey;
                    _proposalList.SelectedIndex = index;
                    return groups[index];
                }
                _proposalKey = null;
                ShowNotice();
                return null;
            }
            for (var i = 0; i < groups.Count; i++)
            {
                if (groups[i].GroupKey != key) continue;
                _proposalList.SelectedIndex = i;
                return null;
            }
            _proposalKey = null;
            if (_current?.GroupKey == key) ShowNotice();
            return null;
        }
        finally
        {
            _rendering = false;
        }
    }

    private Grid ProposalRow(CatalogTakeAutoGroup group)
    {
        var row = GroupRow(group);
        if (row.Children.OfType<StackPanel>().LastOrDefault() is { } text)
        {
            if (TakeProposalPolicy.RelationLabel(group.Relation) is not null) text.Children.Insert(1, RelationPill(group.Relation));
            var evidence = new TextBlock
            {
                Text = TakeProposalPolicy.EvidenceText(group.Evidence),
                FontSize = 11.5,
                Foreground = _text2,
                TextWrapping = TextWrapping.Wrap,
                MaxLines = 2,
                TextTrimming = TextTrimming.CharacterEllipsis,
            };
            SetFont(evidence, "BodyFontFamily");
            text.Children.Add(evidence);
        }
        AutomationProperties.SetName(row, TakeProposalPolicy.Spoken(group, TakeReviewPolicy.DateText(group.CapturedAt)));
        return row;
    }

    private Border RelationPill(string? relation)
    {
        var label = new TextBlock { FontSize = 10.5, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
        SetFont(label, "BodyFontFamily");
        var pill = new Border
        {
            Padding = new Thickness(6, 1, 6, 1),
            Margin = new Thickness(0, 2, 0, 2),
            CornerRadius = new CornerRadius(4),
            BorderThickness = new Thickness(1),
            HorizontalAlignment = HorizontalAlignment.Left,
            Child = label,
        };
        StyleRelation(pill, label, relation);
        return pill;
    }

    private void StyleRelation(Border pill, TextBlock label, string? relation)
    {
        var text = TakeProposalPolicy.RelationLabel(relation);
        pill.Visibility = text is null ? Visibility.Collapsed : Visibility.Visible;
        if (text is null) return;
        var (tone, wash) = TakeProposalPolicy.Tone(relation) switch
        {
            TakeProposalPolicy.RelationTone.Gold => (_gold, _goldWash),
            TakeProposalPolicy.RelationTone.Info => (_info, _infoWash),
            _ => (_ai, _aiWash),
        };
        label.Text = text;
        label.Foreground = tone;
        pill.BorderBrush = tone;
        pill.Background = wash;
        var help = TakeProposalPolicy.RelationHelp(relation);
        ToolTipService.SetToolTip(pill, help);
        AutomationProperties.SetName(pill, text);
        AutomationProperties.SetHelpText(pill, help ?? "");
    }

    private void RenderGroupingJob()
    {
        var job = _groupingJob;
        _findRelated.IsEnabled = TakeProposalPolicy.CanFind(job);
        ToolTipService.SetToolTip(_findRelated, TakeProposalPolicy.CanFind(job)
            ? "Look through the whole library for related tries. Runs on this PC."
            : "Already looking");
        if (job is null || !TakeProposalPolicy.ShowJob(job))
        {
            _groupingJobBox.Visibility = Visibility.Collapsed;
            return;
        }
        var shown = _groupingJobBox.Visibility == Visibility.Visible;
        var row = _groupingJobRow ??= BuildJobRow(_groupingJobBox, "Related tries progress", GroupingControlAsync);
        RenderJobRow(row, _groupingJobBox, job);
        if (!shown) SpringIn(row.Root);
    }

    private void OpenProposal(CatalogTakeAutoGroup group)
    {
        if (_proposalKey == group.GroupKey && _current?.GroupKey == group.GroupKey) return;
        LeaveManual();
        _proposalKey = group.GroupKey;
        _rendering = true;
        try
        {
            _groupList.SelectedIndex = -1;
        }
        finally
        {
            _rendering = false;
        }
        Run(ct => ScoreAsync(group, focusStrip: false, ct));
    }

    private void ClearProposalSelection()
    {
        _proposalKey = null;
        _rendering = true;
        try
        {
            _proposalList.SelectedIndex = -1;
        }
        finally
        {
            _rendering = false;
        }
    }

    private void SaveProposalAsGroup(CatalogTakeAutoGroup group)
    {
        StartManual();
        NewGroupState();
        var inPool = Pool().Select(hit => hit.FileID).ToHashSet();
        var hits = TakeProposalPolicy.SaveHits(group);
        _found.AddRange(hits.Where(hit => inPool.Add(hit.FileID)));
        _chosen.UnionWith(hits.Select(hit => hit.FileID));
        _titleBox.Text = TakeReviewPolicy.DateText(group.CapturedAt);
        RenderEditor();
        ShowToast("Name the group and describe the outcome, then save", null, PlainToastWindow);
    }

    /// <summary>Header pieces that differ for a proposal: the relation chip, the evidence intro and Save as group.</summary>
    private void RenderProposalHeader(CatalogTakeAutoGroup group)
    {
        StyleRelation(_relationPill, _relationText, group.Relation);
        _reviewIntro.Text = TakeProposalPolicy.Summary(group);
        _saveAsGroup.Visibility = _proposalKey == group.GroupKey ? Visibility.Visible : Visibility.Collapsed;
        AutomationProperties.SetName(_saveAsGroup, $"Save these {TakeReviewPolicy.Noun(group, 2)} as a group");
    }

    /// <summary>After a group is kept, skipped, trashed or put back, reads both lists again; a resolved
    /// proposal hands review on to the next proposal.</summary>
    private async Task AfterResolveAsync(string? key, CancellationToken ct)
    {
        if (_proposalKey is not null)
        {
            await LoadProposalsAsync(key, ct);
            await LoadGroupsAsync(keepCurrent: true, ct);
            return;
        }
        await LoadGroupsAsync(keepCurrent: false, ct);
        await LoadProposalsAsync(null, ct);
    }

    private static void SpringIn(UIElement element)
    {
        ElementCompositionPreview.SetIsTranslationEnabled(element, true);
        var visual = ElementCompositionPreview.GetElementVisual(element);
        visual.Properties.InsertVector3("Translation", new Vector3(0, 8, 0));
        visual.Opacity = 0;
        SpringEasing.AnimateOpacity(element, 1f, ProposalResponse, ProposalDamping);
        SpringEasing.AnimateScalar(element, "Translation.Y", 0f, ProposalResponse, ProposalDamping);
    }
}
