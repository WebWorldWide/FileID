using System.Globalization;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.Foundation;
using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.System;
using Windows.UI;

namespace FileID.Views.Workbenches;

/// <summary>Best Takes groups made by hand (DESIGN_V3_PORT "Best Takes", manual groups; macOS
/// TakeWorkbench, GTK takes_groups.rs): YOUR GROUPS, MAKE A GROUP BY HAND with clip search and Find
/// related, the group editor (Ctrl+S saves), the scoring job, the recommendation and the take cards
/// with Met / Missed / Unclear / Prefer / Undo review.</summary>
internal sealed partial class TakesWorkbench
{
    private IReadOnlyList<CatalogHit> _candidates = Array.Empty<CatalogHit>();
    private bool _fromSearch;
    private IReadOnlyList<CatalogEvent> _events = Array.Empty<CatalogEvent>();
    private CatalogEvent? _selectedEvent;
    private bool _manualActive;
    private IReadOnlyList<CatalogTake> _takes = Array.Empty<CatalogTake>();
    private CatalogTakeRecommendation? _recommendation;
    private IReadOnlyList<CatalogTakeGroupSuggestion> _suggestions = Array.Empty<CatalogTakeGroupSuggestion>();
    private readonly List<CatalogHit> _found = new();
    private readonly HashSet<long> _chosen = new();
    private string? _lastDeleted;
    private bool _manualPending;
    private CatalogRequest? _queuedManual;
    private string? _goalJobID;
    private string? _goalEventID;
    private readonly HashSet<long> _goalRequested = new();
    private readonly HashSet<string> _previousJobs = new();
    private IReadOnlyList<CatalogJob> _jobs = Array.Empty<CatalogJob>();
    private string? _failureShown;
    private bool _jobPolling;
    private bool _syncingEvents;
    private bool _syncingChecks;
    private long[] _clipIDs = Array.Empty<long>();
    private readonly List<(long FileID, CheckBox Check)> _clipChecks = new();
    private readonly List<TakeCard> _cards = new();
    private JobRow? _jobRow;
    private readonly DispatcherTimer _jobPoll = new() { Interval = TimeSpan.FromSeconds(2) };

    private readonly Brush _danger = ThemeHelper.GetBrushSafe("DangerBrush");
    private readonly Brush _warning = ThemeHelper.GetBrushSafe("WarningBrush");
    private readonly Brush _panelHeader = ThemeHelper.GetBrushSafe("PanelHeaderBrush");
    private readonly Brush _hairlineSoft = ThemeHelper.GetBrushSafe("HairlineSoftBrush");
    private readonly Brush _aiCard = new SolidColorBrush(Color.FromArgb(0x14, 0xB1, 0x9B, 0xCE));
    private readonly Brush _aiCardStroke = new SolidColorBrush(Color.FromArgb(0x4D, 0xB1, 0x9B, 0xCE));

    private readonly TextBlock _eventsHeader = SectionLabel("Your groups · 0");
    private readonly ListView _eventsList = new() { SelectionMode = ListViewSelectionMode.Single, Visibility = Visibility.Collapsed };
    private readonly TextBlock _eventsEmpty = Meta("Groups you make by hand, with a goal, show up here.");
    private readonly TextBox _clipSearch = new() { PlaceholderText = "Search your videos", MaxLength = 2000 };
    private readonly Button _findRelated = Secondary(new Button { Content = "Find related", Height = 28, MinHeight = 28, IsEnabled = false });
    private readonly Button _newGroup = Secondary(new Button { Content = "+ New group", Height = 28, MinHeight = 28 });
    private readonly StackPanel _suggestionsBox = new() { Spacing = 8 };
    private readonly Grid _deletedFooter = new() { ColumnSpacing = 8, Padding = new Thickness(14, 8, 14, 8), Visibility = Visibility.Collapsed };
    private readonly Button _undoDelete = Secondary(new Button { Content = "Undo", Height = 28, MinHeight = 28 });

    private readonly StackPanel _manualPage = new() { Spacing = 16, Visibility = Visibility.Collapsed };
    private readonly Button _backToSearch = Ghost(new Button { Content = "← Back to Search & Moments", MinHeight = 28, Visibility = Visibility.Collapsed });
    private readonly TextBlock _groupTitle = SectionLabel("New group");
    private readonly ProgressBar _manualBusy = new() { IsIndeterminate = true, Width = 60, VerticalAlignment = VerticalAlignment.Center, Visibility = Visibility.Collapsed };
    private readonly Button _undoEdit = Ghost(new Button { Content = "Undo edit", MinHeight = 28, Visibility = Visibility.Collapsed });
    private readonly Button _deleteGroup = Ghost(new Button { Content = "Delete", MinHeight = 28, Visibility = Visibility.Collapsed });
    private readonly TextBox _titleBox = new() { PlaceholderText = "Group name, e.g. Alex at bat", MaxLength = 200 };
    private readonly TextBox _goalBox = new() { PlaceholderText = "What should happen, e.g. Alex gets a hit", MaxLength = 500 };
    private readonly StackPanel _clipsSection = new() { Visibility = Visibility.Collapsed };
    private readonly TextBlock _clipsHeader = SectionLabel("Clips · 0 of 0 chosen");
    private readonly Button _chooseAll = Ghost(new Button { Content = "Choose all", MinHeight = 28 });
    private readonly StackPanel _clipRows = new() { Padding = new Thickness(14, 0, 14, 6) };
    private readonly Button _save = Primary(new Button { Content = "Save group", Height = 28, MinHeight = 28, IsEnabled = false });
    private readonly Button _score = Secondary(new Button { Content = "Score takes", Height = 28, MinHeight = 28, Visibility = Visibility.Collapsed });
    private readonly Button _refreshScores = Ghost(new Button { Content = "Refresh scores", MinHeight = 28, Visibility = Visibility.Collapsed });
    private readonly TextBlock _editorNote = Meta("");
    private readonly StackPanel _jobBox = new() { Visibility = Visibility.Collapsed };
    private readonly StackPanel _recommendationBox = new();
    private readonly TakeCardGrid _takeGrid = new() { Visibility = Visibility.Collapsed };
    private readonly Grid _manualIntro = DashedNoticeBox(
        "Make a group by hand",
        "1. Search for your clips on the left.  2. Tick two or more tries of the same shot and say what should happen.  3. Save, then score each try against that goal.",
        out _,
        out _);

    private bool ManualEngaged => _manualActive || _selectedEvent is not null;

    // ---- Building -------------------------------------------------------------

    private ScrollViewer BuildManualPane()
    {
        var stack = new StackPanel { Spacing = 2, Padding = new Thickness(0, 0, 0, 10) };
        stack.Children.Add(SoftHairline());
        _eventsHeader.Margin = new Thickness(14, 0, 14, 4);
        stack.Children.Add(_eventsHeader);
        _eventsEmpty.FontSize = 12;
        _eventsEmpty.Margin = new Thickness(14, 2, 14, 6);
        stack.Children.Add(_eventsEmpty);
        stack.Children.Add(_eventsList);
        stack.Children.Add(SoftHairline());
        var make = new StackPanel { Spacing = 10, Padding = new Thickness(14, 0, 14, 0) };
        make.Children.Add(SectionLabel("Make a group by hand"));
        AutomationProperties.SetName(_clipSearch, "Search your videos");
        make.Children.Add(_clipSearch);
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        buttons.Children.Add(_findRelated);
        buttons.Children.Add(_newGroup);
        make.Children.Add(buttons);
        make.Children.Add(_suggestionsBox);
        stack.Children.Add(make);

        ToolTipService.SetToolTip(_findRelated, TakeGroupsPolicy.FindRelatedTooltip(0));
        _clipSearch.KeyDown += (_, e) => DebugLog.SafeRun("TakesWorkbench.ClipSearch", () =>
        {
            if (e.Key != VirtualKey.Enter) return;
            e.Handled = true;
            var query = _clipSearch.Text.Trim();
            if (query.Length == 0) return;
            StartManual();
            SendManual(ManualRequest("search", query: query));
        });
        _findRelated.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.FindRelated", () =>
            SendManual(ManualRequest("suggestTakeGroups", fileIDs: Pool().Select(hit => hit.FileID).ToArray())));
        _newGroup.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.NewGroup", () =>
        {
            StartManual();
            NewGroupState();
        });
        _eventsList.SelectionChanged += (_, _) => DebugLog.SafeRun("TakesWorkbench.EventSelected", () =>
        {
            if (_syncingEvents || _eventsList.SelectedIndex < 0 || _eventsList.SelectedIndex >= _events.Count) return;
            var saved = _events[_eventsList.SelectedIndex];
            if (_current is null && _selectedEvent?.Id == saved.Id) return;
            OpenEvent(saved);
        });

        var deleted = Meta("Group deleted");
        deleted.FontSize = 12;
        deleted.VerticalAlignment = VerticalAlignment.Center;
        _undoDelete.Foreground = _gold;
        _undoDelete.BorderBrush = _gold;
        _undoDelete.Background = _goldWash;
        _deletedFooter.BorderBrush = _hairline;
        _deletedFooter.BorderThickness = new Thickness(0, 1, 0, 0);
        _deletedFooter.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _deletedFooter.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(_undoDelete, 1);
        _deletedFooter.Children.Add(deleted);
        _deletedFooter.Children.Add(_undoDelete);
        _undoDelete.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.UndoDelete", () =>
        {
            if (_lastDeleted is { } id) SendManual(ManualRequest("undoEventEdit", eventID: id));
        });

        return new ScrollViewer
        {
            Content = stack,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
        };
    }

    private StackPanel BuildManualPage()
    {
        _backToSearch.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.BackToSearch", () =>
            AppViewModel.Instance.ActiveTab = SidebarTab.Moments);
        _manualPage.Children.Add(_backToSearch);
        _manualPage.Children.Add(BuildGroupPanel());
        _manualPage.Children.Add(_jobBox);
        _manualPage.Children.Add(_recommendationBox);
        _manualPage.Children.Add(_takeGrid);
        _manualPage.Children.Add(_manualIntro);
        _jobPoll.Tick += (_, _) => DebugLog.SafeRun("TakesWorkbench.JobPoll", PollGoalJob);
        return _manualPage;
    }

    private Border BuildGroupPanel()
    {
        _groupTitle.VerticalAlignment = VerticalAlignment.Center;
        _deleteGroup.Foreground = _danger;
        var trailing = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Center };
        trailing.Children.Add(_manualBusy);
        trailing.Children.Add(_undoEdit);
        trailing.Children.Add(_deleteGroup);
        var header = new Grid
        {
            Padding = new Thickness(14, 6, 8, 6),
            MinHeight = 40,
            ColumnSpacing = 8,
            BorderBrush = _hairline,
            BorderThickness = new Thickness(0, 0, 0, 1),
        };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(trailing, 1);
        header.Children.Add(_groupTitle);
        header.Children.Add(trailing);

        var fields = new StackPanel { Spacing = 10, Padding = new Thickness(14, 12, 14, 12) };
        fields.Children.Add(FieldRow("Name", _titleBox));
        fields.Children.Add(FieldRow("Goal", _goalBox));

        var clipsHead = new Grid { ColumnSpacing = 8, Padding = new Thickness(14, 6, 8, 0) };
        clipsHead.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        clipsHead.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _clipsHeader.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(_chooseAll, 1);
        clipsHead.Children.Add(_clipsHeader);
        clipsHead.Children.Add(_chooseAll);
        _clipsSection.Children.Add(new Border { Height = 1, Background = _hairline });
        _clipsSection.Children.Add(clipsHead);
        _clipsSection.Children.Add(_clipRows);

        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, VerticalAlignment = VerticalAlignment.Center };
        actions.Children.Add(_save);
        actions.Children.Add(_score);
        actions.Children.Add(_refreshScores);
        _editorNote.FontSize = 11.5;
        _editorNote.MaxLines = 2;
        _editorNote.TextTrimming = TextTrimming.CharacterEllipsis;
        _editorNote.TextAlignment = TextAlignment.Right;
        _editorNote.VerticalAlignment = VerticalAlignment.Center;
        var footer = new Grid
        {
            ColumnSpacing = 12,
            Padding = new Thickness(14, 8, 14, 8),
            Background = _panelHeader,
            BorderBrush = _hairline,
            BorderThickness = new Thickness(0, 1, 0, 0),
        };
        footer.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        footer.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(_editorNote, 1);
        footer.Children.Add(actions);
        footer.Children.Add(_editorNote);

        var stack = new StackPanel();
        stack.Children.Add(header);
        stack.Children.Add(fields);
        stack.Children.Add(_clipsSection);
        stack.Children.Add(footer);

        _titleBox.TextChanged += (_, _) => DebugLog.SafeRun("TakesWorkbench.TitleChanged", RenderEditor);
        _goalBox.TextChanged += (_, _) => DebugLog.SafeRun("TakesWorkbench.GoalChanged", RenderEditor);
        _chooseAll.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.ChooseAll", () =>
        {
            var next = TakeGroupsPolicy.ToggleAll(_chosen, Pool());
            _chosen.Clear();
            _chosen.UnionWith(next);
            RenderEditor();
        });
        _save.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.Save", SaveGroup);
        var saveShortcut = new KeyboardAccelerator { Key = VirtualKey.S, Modifiers = VirtualKeyModifiers.Control };
        saveShortcut.Invoked += (_, args) => DebugLog.SafeRun("TakesWorkbench.SaveShortcut", () =>
        {
            args.Handled = true;
            SaveGroup();
        });
        _save.KeyboardAccelerators.Add(saveShortcut);
        _save.KeyboardAcceleratorPlacementMode = KeyboardAcceleratorPlacementMode.Hidden;
        ToolTipService.SetToolTip(_score, "Check each clip against the goal. Uses the description model from Describe Files.");
        _score.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.ScoreTakes", ScoreTakes);
        _refreshScores.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.RefreshScores", () =>
        {
            if (_selectedEvent is { } saved) SendManual(ManualRequest("takeGroup", eventID: saved.Id));
        });
        _undoEdit.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.UndoEdit", () =>
        {
            if (_selectedEvent is { } saved) SendManual(ManualRequest("undoEventEdit", eventID: saved.Id));
        });
        _deleteGroup.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.DeleteGroup", () =>
        {
            if (_selectedEvent is not { } saved) return;
            _lastDeleted = saved.Id;
            _deletedFooter.Visibility = Visibility.Visible;
            SendManual(ManualRequest("deleteEvent", eventID: saved.Id));
        });

        return new Border
        {
            Background = _panel,
            BorderBrush = _hairlineStrong,
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(10),
            Child = stack,
        };
    }

    private Grid FieldRow(string label, TextBox field)
    {
        var caption = TextLine(label, 12.5, _text2);
        caption.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        caption.VerticalAlignment = VerticalAlignment.Center;
        AutomationProperties.SetAccessibilityView(caption, AccessibilityView.Raw);
        AutomationProperties.SetName(field, label);
        var row = new Grid { ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(44) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(field, 1);
        row.Children.Add(caption);
        row.Children.Add(field);
        return row;
    }

    private Border SoftHairline() => new() { Height = 1, Background = _hairlineSoft, Margin = new Thickness(0, 8, 0, 8) };

    private static Button AccentButton(string text, Brush tone, Brush wash)
    {
        var button = Secondary(new Button { Content = text, Height = 28, MinHeight = 28 });
        button.Foreground = tone;
        button.BorderBrush = tone;
        button.Background = wash;
        return button;
    }

    private Brush ToneBrush(TakeGroupsPolicy.Tone tone) => tone switch
    {
        TakeGroupsPolicy.Tone.Gold => _gold,
        TakeGroupsPolicy.Tone.Ai => _ai,
        TakeGroupsPolicy.Tone.Done => _delight,
        TakeGroupsPolicy.Tone.Danger => _danger,
        TakeGroupsPolicy.Tone.Warning => _warning,
        TakeGroupsPolicy.Tone.Text2 => _text2,
        _ => _text3,
    };

    // ---- Requests ---------------------------------------------------------------

    private static CatalogRequest ManualRequest(string action, string? query = null, long? fileID = null, IReadOnlyList<long>? fileIDs = null,
        CatalogEvent? saved = null, string? eventID = null, CatalogTakeFeedback? feedback = null, string? timelineMode = null) =>
        new(Guid.NewGuid().ToString(), action, query, fileID, null, null, null, fileIDs,
            TimelineMode: timelineMode, Event: saved, EventID: eventID, TakeFeedback: feedback);

    /// <summary>Runs a manual-group request now, or right after the page's current request.</summary>
    private void SendManual(CatalogRequest request)
    {
        if (Busy)
        {
            _queuedManual = request;
            return;
        }
        Run(ct => ManualAsync(request, ct));
    }

    private void RunQueuedManual()
    {
        if (Busy || Lifetime.IsCancellationRequested || _queuedManual is not { } queued) return;
        _queuedManual = null;
        Run(ct => ManualAsync(queued, ct));
    }

    private async Task ListEventsAsync(CancellationToken ct)
    {
        try
        {
            await ManualAsync(ManualRequest("listEvents"), ct);
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            DebugLog.Warn("TakesWorkbench couldn't list groups: " + ex.Message);
        }
    }

    private async Task ManualAsync(CatalogRequest request, CancellationToken ct)
    {
        await UI(() =>
        {
            _manualPending = true;
            RenderEditor();
        }, ct);
        CatalogResponse response;
        try
        {
            response = await Engine.CatalogAsync(request, ct);
        }
        catch (Exception) when (request.Action == "deleteEvent")
        {
            await UI(ClearDeleted, ct);
            throw;
        }
        finally
        {
            await UI(() =>
            {
                _manualPending = false;
                RenderEditor();
            }, ct);
        }
        var relist = false;
        await UI(() =>
        {
            relist = Consume(request.Action, response);
            Status.Text = "";
        }, ct);
        if (relist)
        {
            await ManualAsync(ManualRequest("listEvents"), ct);
            await LoadProposalsAsync(null, ct);
        }
    }

    /// <summary>One catalog reply for the manual flow (macOS consume). True when the group list must be read again.</summary>
    private bool Consume(string action, CatalogResponse response)
    {
        var ok = response.Status == "ok";
        var message = response.Message ?? (ok ? "" : "The request failed.");
        if (message.Length > 0) ShowToast(message, null, PlainToastWindow);
        if (!ok)
        {
            if (action == "deleteEvent") ClearDeleted();
            RenderEditor();
            return false;
        }
        switch (action)
        {
            case "search":
                var found = TakeGroupsPolicy.FoundVideos(response.Hits, _candidates);
                if (found.Count == 0) ShowToast($"No videos matched “{_clipSearch.Text.Trim()}”", null, PlainToastWindow);
                _found.Clear();
                _found.AddRange(found);
                RenderEditor();
                return false;
            case "suggestTakeGroups":
                _suggestions = response.SuggestedTakeGroups ?? Array.Empty<CatalogTakeGroupSuggestion>();
                if (_suggestions.Count == 0 && message.Length == 0) ShowToast("No related takes found in these clips", null, PlainToastWindow);
                RenderSuggestions();
                break;
            case "enqueueTimeline":
                _goalJobID = TakeGroupsPolicy.NewGoalJob(response.Jobs, _goalRequested, _previousJobs)?.Id;
                _jobs = response.Jobs;
                RefreshGoalJob();
                break;
        }
        if (response.Events is { } events)
        {
            if (TakeGroupsPolicy.RepliesWithOpenGroup(action))
            {
                if (events.Count > 0) Adopt(events[0]);
            }
            else
            {
                _events = events;
                var restored = action == "undoEventEdit" && _selectedEvent is { } open
                    ? events.FirstOrDefault(saved => saved.Id == open.Id)
                    : null;
                if (restored is not null)
                {
                    Adopt(restored);
                    _queuedManual = ManualRequest("takeGroup", eventID: restored.Id);
                }
                else if (action == "deleteEvent" || (action == "undoEventEdit" && _selectedEvent is not null))
                {
                    NewGroupState();
                    if (ManualEngaged) ShowManualPage();
                    else ShowNotice();
                }
                RenderEvents();
            }
        }
        if (response.Takes is { } takes)
        {
            _takes = takes;
            _recommendation = response.Recommendation;
            _found.AddRange(TakeGroupsPolicy.MissingTakes(takes, Pool()));
            RenderTakes();
        }
        if (action == "undoEventEdit") ClearDeleted();
        RenderEditor();
        RenderEventsSelection();
        return TakeGroupsPolicy.Relists(action);
    }

    private CatalogJob? GoalJobRecord()
    {
        if (_goalJobID is not { } id || _selectedEvent is null || _goalEventID != _selectedEvent.Id) return null;
        return _jobs.FirstOrDefault(job => job.Id == id);
    }

    private void RefreshGoalJob()
    {
        var job = GoalJobRecord();
        RenderJob(job);
        if (job is null) return;
        if (job.State == "completed" && !Busy && _goalEventID is { } eventID)
        {
            _goalJobID = null;
            RenderJob(null);
            SendManual(ManualRequest("takeGroup", eventID: eventID));
        }
        else if (job.State == "failed" && _failureShown != job.Id)
        {
            _failureShown = job.Id;
            ShowToast(job.Error ?? "Scoring didn't finish. Scores you already reviewed are kept.", null, PlainToastWindow);
        }
        RenderEditor();
    }

    private void PollGoalJob()
    {
        if (_goalJobID is null || _jobPolling || Busy || Lifetime.IsCancellationRequested) return;
        _jobPolling = true;
        var ct = Lifetime;
        _ = DebugLog.SafeRunAsync("TakesWorkbench.PollJobs", async () =>
        {
            try
            {
                var response = await Engine.CatalogAsync(ManualRequest("jobs"), ct);
                await UI(() =>
                {
                    if (response.Status != "ok") return;
                    _jobs = response.Jobs;
                    RefreshGoalJob();
                }, ct);
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested)
            {
            }
            finally
            {
                _jobPolling = false;
            }
        });
    }

    private async Task JobControlAsync(string action, CatalogJob job, CancellationToken ct)
    {
        var request = CatalogJobActions.ControlRequest(action, job);
        var previous = _jobs.Select(known => known.Id).ToHashSet();
        var response = await Engine.CatalogAsync(request, ct);
        await UI(() =>
        {
            Status.Text = "";
            if (response.Status != "ok")
            {
                ShowToast(response.Message ?? "That didn't work. Try again in a moment.", null, PlainToastWindow);
                return;
            }
            _jobs = response.Jobs;
            if (request.Action == "enqueueTimeline")
            {
                _goalJobID = TakeGroupsPolicy.NewGoalJob(response.Jobs, _goalRequested, previous)?.Id ?? _goalJobID;
            }
            RefreshGoalJob();
        }, ct);
    }

    // ---- State ----------------------------------------------------------------

    private IReadOnlyList<CatalogHit> Pool() => TakeGroupsPolicy.Pool(_candidates, _found);

    private void OpenWithCandidates(IReadOnlyList<CatalogHit> candidates)
    {
        _candidates = candidates;
        _fromSearch = true;
        StartManual();
    }

    private void StartManual()
    {
        LeaveAuto();
        _manualActive = true;
        ShowManualPage();
    }

    /// <summary>Closes the open possible group because the manual editor is taking over.</summary>
    private void LeaveAuto()
    {
        StopPlayer(_left);
        StopPlayer(_right);
        _current = null;
        _qualities = Array.Empty<CatalogTakeQuality>();
        _keep.Clear();
        _review.Visibility = Visibility.Collapsed;
        _rendering = true;
        try
        {
            _groupList.SelectedIndex = -1;
        }
        finally
        {
            _rendering = false;
        }
        ClearProposalSelection();
    }

    /// <summary>Leaves the manual editor because a possible group was opened.</summary>
    private void LeaveManual()
    {
        _manualActive = false;
        _selectedEvent = null;
        StopCards();
        _manualPage.Visibility = Visibility.Collapsed;
        RenderEventsSelection();
    }

    private void ShowManualPage()
    {
        _notice.Visibility = Visibility.Collapsed;
        _firstLoad.Visibility = Visibility.Collapsed;
        _review.Visibility = Visibility.Collapsed;
        _manualPage.Visibility = Visibility.Visible;
        RenderEditor();
        RenderEventsSelection();
    }

    private void OpenEvent(CatalogEvent saved)
    {
        LeaveAuto();
        _manualActive = false;
        ClearDeleted();
        Adopt(saved);
        _takes = Array.Empty<CatalogTake>();
        _recommendation = null;
        RenderTakes();
        ShowManualPage();
        SendManual(ManualRequest("takeGroup", eventID: saved.Id));
    }

    private void Adopt(CatalogEvent saved)
    {
        _selectedEvent = saved;
        _chosen.Clear();
        _chosen.UnionWith(saved.FileIDs);
        if (_titleBox.Text != saved.Title) _titleBox.Text = saved.Title;
        if (_goalBox.Text != saved.Goal) _goalBox.Text = saved.Goal;
        SyncChecks();
    }

    private void NewGroupState()
    {
        _selectedEvent = null;
        _takes = Array.Empty<CatalogTake>();
        _recommendation = null;
        _chosen.Clear();
        _titleBox.Text = "";
        _goalBox.Text = "";
        RenderTakes();
        RenderEditor();
    }

    private void ClearDeleted()
    {
        _lastDeleted = null;
        _deletedFooter.Visibility = Visibility.Collapsed;
    }

    private void SetChosen(long fileID, bool on)
    {
        if (_syncingChecks) return;
        if (on) _chosen.Add(fileID);
        else _chosen.Remove(fileID);
        RenderEditor();
    }

    private void SaveGroup()
    {
        if (!ManualEngaged || !TakeGroupsPolicy.CanSave(_titleBox.Text, _goalBox.Text, _chosen.Count)) return;
        ClearDeleted();
        var saved = TakeGroupsPolicy.EventToSave(_selectedEvent, Guid.NewGuid().ToString(), _titleBox.Text, _goalBox.Text, _chosen);
        SendManual(ManualRequest("saveEvent", saved: saved));
    }

    private void ScoreTakes()
    {
        if (_selectedEvent is not { } saved) return;
        _goalEventID = saved.Id;
        _goalRequested.Clear();
        _goalRequested.UnionWith(saved.FileIDs);
        _previousJobs.Clear();
        _previousJobs.UnionWith(_jobs.Select(job => job.Id));
        SendManual(ManualRequest("enqueueTimeline", fileIDs: saved.FileIDs, timelineMode: "moments"));
    }

    private void ReviewSuggestion(CatalogTakeGroupSuggestion suggestion)
    {
        StartManual();
        NewGroupState();
        var inPool = Pool().Select(hit => hit.FileID).ToHashSet();
        _found.AddRange(TakeGroupsPolicy.SuggestionHits(suggestion).Where(hit => inPool.Add(hit.FileID)));
        _chosen.UnionWith(suggestion.Members.Select(member => member.FileID));
        RenderEditor();
        ShowToast("Name the group and describe the outcome, then save", null, PlainToastWindow);
    }

    // ---- Rendering --------------------------------------------------------------

    private void RenderEvents()
    {
        _syncingEvents = true;
        try
        {
            _eventsHeader.Text = string.Create(CultureInfo.InvariantCulture, $"YOUR GROUPS · {_events.Count}");
            _eventsList.Items.Clear();
            foreach (var saved in _events) _eventsList.Items.Add(EventRow(saved));
            _eventsEmpty.Visibility = _events.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
            _eventsList.Visibility = _events.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        }
        finally
        {
            _syncingEvents = false;
        }
        RenderEventsSelection();
    }

    private Grid EventRow(CatalogEvent saved)
    {
        var title = TextLine(saved.Title, 13, _text1);
        title.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        var meta = TextLine(TakeGroupsPolicy.EventMeta(saved), 11.5, _text3);
        meta.TextWrapping = TextWrapping.Wrap;
        meta.MaxLines = 2;
        var text = new StackPanel { Spacing = 3, VerticalAlignment = VerticalAlignment.Center };
        text.Children.Add(title);
        text.Children.Add(meta);
        var row = new Grid { Padding = new Thickness(2, 6, 2, 6) };
        row.Children.Add(text);
        AutomationProperties.SetName(row, TakeGroupsPolicy.EventSpoken(saved));
        return row;
    }

    private void RenderEventsSelection()
    {
        var open = _current is null ? _selectedEvent?.Id : null;
        var index = -1;
        for (var i = 0; i < _events.Count; i++)
        {
            if (_events[i].Id == open) index = i;
        }
        if (_eventsList.SelectedIndex == index) return;
        _syncingEvents = true;
        try
        {
            _eventsList.SelectedIndex = index;
        }
        finally
        {
            _syncingEvents = false;
        }
    }

    private void RenderSuggestions()
    {
        _suggestionsBox.Children.Clear();
        foreach (var suggestion in _suggestions)
        {
            var names = string.Join(", ", TakeGroupsPolicy.SuggestionNames(suggestion));
            var list = TextLine(names, 11.5, _text1, "MonoFontFamily");
            list.TextWrapping = TextWrapping.Wrap;
            list.MaxLines = 2;
            var meta = TextLine(TakeGroupsPolicy.SuggestionMeta(suggestion), 11.5, _ai);
            meta.VerticalAlignment = VerticalAlignment.Center;
            var review = AccentButton("Review", _ai, _aiWash);
            ToolTipService.SetToolTip(review, suggestion.Reason);
            var row = new Grid { ColumnSpacing = 8 };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            Grid.SetColumn(review, 1);
            row.Children.Add(meta);
            row.Children.Add(review);
            var body = new StackPanel { Spacing = 6 };
            body.Children.Add(list);
            body.Children.Add(row);
            var card = new Border
            {
                Background = _aiCard,
                BorderBrush = _aiCardStroke,
                BorderThickness = new Thickness(1),
                CornerRadius = new CornerRadius(8),
                Padding = new Thickness(10, 8, 10, 8),
                Child = body,
            };
            AutomationProperties.SetName(card, "Possible group: " + names);
            var captured = suggestion;
            review.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.ReviewSuggestion", () => ReviewSuggestion(captured));
            _suggestionsBox.Children.Add(card);
        }
    }

    private void RenderEditor()
    {
        var saved = _selectedEvent;
        _groupTitle.Text = saved is null ? "NEW GROUP" : "GROUP";
        _manualBusy.Visibility = _manualPending ? Visibility.Visible : Visibility.Collapsed;
        _undoEdit.Visibility = saved is null ? Visibility.Collapsed : Visibility.Visible;
        _deleteGroup.Visibility = saved is null ? Visibility.Collapsed : Visibility.Visible;
        _backToSearch.Visibility = _fromSearch ? Visibility.Visible : Visibility.Collapsed;
        var pool = Pool();
        _findRelated.IsEnabled = pool.Count >= 2;
        ToolTipService.SetToolTip(_findRelated, TakeGroupsPolicy.FindRelatedTooltip(pool.Count));
        var ids = pool.Select(hit => hit.FileID).ToArray();
        if (!_clipIDs.AsSpan().SequenceEqual(ids))
        {
            RebuildClips(pool);
            _clipIDs = ids;
        }
        _clipsSection.Visibility = pool.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        _clipsHeader.Text = TakeGroupsPolicy.ClipsHeader(_chosen.Count, pool.Count);
        _chooseAll.Content = TakeGroupsPolicy.ChooseLabel(_chosen, pool);
        SyncChecks();
        var canSave = TakeGroupsPolicy.CanSave(_titleBox.Text, _goalBox.Text, _chosen.Count);
        _save.IsEnabled = canSave;
        ToolTipService.SetToolTip(_save, TakeGroupsPolicy.SaveTooltip(canSave));
        _score.Visibility = saved is null ? Visibility.Collapsed : Visibility.Visible;
        _refreshScores.Visibility = saved is null ? Visibility.Collapsed : Visibility.Visible;
        if (saved is not null)
        {
            _score.IsEnabled = TakeGroupsPolicy.CanScore(saved, _goalBox.Text, _chosen, _takes, _manualPending, GoalJobRecord());
        }
        _editorNote.Text = TakeGroupsPolicy.EditorNote(pool.Count == 0);
        _manualIntro.Visibility = _takes.Count == 0 && saved is null ? Visibility.Visible : Visibility.Collapsed;
    }

    private void RebuildClips(IReadOnlyList<CatalogHit> pool)
    {
        _clipRows.Children.Clear();
        _clipChecks.Clear();
        foreach (var hit in pool)
        {
            var name = HitPresentation.FileName(hit.Path);
            var check = new CheckBox
            {
                Content = TextLine(name, 12, _text1, "MonoFontFamily"),
                MinHeight = 28,
                IsChecked = _chosen.Contains(hit.FileID),
            };
            ToolTipService.SetToolTip(check, hit.Path);
            AutomationProperties.SetName(check, name);
            var fileID = hit.FileID;
            check.Checked += (_, _) => DebugLog.SafeRun("TakesWorkbench.ClipChecked", () => SetChosen(fileID, true));
            check.Unchecked += (_, _) => DebugLog.SafeRun("TakesWorkbench.ClipUnchecked", () => SetChosen(fileID, false));
            _clipRows.Children.Add(check);
            _clipChecks.Add((fileID, check));
        }
    }

    private void SyncChecks()
    {
        _syncingChecks = true;
        try
        {
            foreach (var (fileID, check) in _clipChecks)
            {
                var on = _chosen.Contains(fileID);
                if (check.IsChecked != on) check.IsChecked = on;
            }
        }
        finally
        {
            _syncingChecks = false;
        }
    }

    private void RenderJob(CatalogJob? job)
    {
        if (job is null)
        {
            _jobBox.Visibility = Visibility.Collapsed;
            return;
        }
        RenderJobRow(_jobRow ??= BuildJobRow(_jobBox, "Scoring progress", JobControlAsync), _jobBox, job);
    }

    private void RenderJobRow(JobRow row, StackPanel box, CatalogJob job)
    {
        row.Job = job;
        var title = TakeGroupsPolicy.JobTitle(job);
        row.Title.Text = title;
        var (state, tone) = TakeGroupsPolicy.JobState(job);
        row.State.Text = state;
        row.State.Foreground = ToneBrush(tone);
        var percent = TakeGroupsPolicy.Percent(job.Progress);
        row.Percent.Text = string.Create(CultureInfo.InvariantCulture, $"{percent}%");
        row.Bar.Value = percent;
        row.Bar.Foreground = tone switch
        {
            TakeGroupsPolicy.Tone.Gold => _gold,
            TakeGroupsPolicy.Tone.Done => _delight,
            _ => _neutralBar,
        };
        row.Pause.Visibility = CatalogJobActions.CanPause(job.State) ? Visibility.Visible : Visibility.Collapsed;
        row.Resume.Visibility = job.State == "paused" ? Visibility.Visible : Visibility.Collapsed;
        row.Retry.Visibility = job.State is "failed" or "cancelled" ? Visibility.Visible : Visibility.Collapsed;
        row.Cancel.Visibility = CatalogJobActions.CanCancel(job.State) ? Visibility.Visible : Visibility.Collapsed;
        row.Error.Text = job.Error ?? "";
        row.Error.Visibility = string.IsNullOrEmpty(job.Error) ? Visibility.Collapsed : Visibility.Visible;
        AutomationProperties.SetName(row.Root, $"{title}, {state}");
        box.Visibility = Visibility.Visible;
    }

    private JobRow BuildJobRow(StackPanel host, string progressName, Func<string, CatalogJob, CancellationToken, Task> control)
    {
        var title = TextLine("", 13, _text1);
        title.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        title.VerticalAlignment = VerticalAlignment.Center;
        var state = TextLine("", 12, _text3);
        state.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        state.VerticalAlignment = VerticalAlignment.Center;
        var percent = TextLine("", 11.5, _text3, "MonoFontFamily");
        percent.VerticalAlignment = VerticalAlignment.Center;
        var pause = Secondary(new Button { Content = "Pause", Height = 28, MinHeight = 28 });
        var resume = AccentButton("Resume", _gold, _goldWash);
        var retry = Secondary(new Button { Content = "Retry", Height = 28, MinHeight = 28 });
        var cancel = Ghost(new Button { Content = "Cancel", MinHeight = 28 });
        var controls = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Center };
        controls.Children.Add(pause);
        controls.Children.Add(resume);
        controls.Children.Add(retry);
        controls.Children.Add(cancel);
        var head = new Grid { ColumnSpacing = 10 };
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        head.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(state, 1);
        Grid.SetColumn(percent, 3);
        Grid.SetColumn(controls, 4);
        head.Children.Add(title);
        head.Children.Add(state);
        head.Children.Add(percent);
        head.Children.Add(controls);
        var bar = new ProgressBar { Minimum = 0, Maximum = 100, Height = 4, MinHeight = 4 };
        AutomationProperties.SetName(bar, progressName);
        var error = Meta("");
        error.Foreground = _danger;
        error.FontSize = 12;
        var stack = new StackPanel { Spacing = 6, Padding = new Thickness(14, 10, 14, 10) };
        stack.Children.Add(head);
        stack.Children.Add(bar);
        stack.Children.Add(error);
        var root = new Border
        {
            Background = _panel,
            BorderBrush = _hairlineStrong,
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(10),
            Child = stack,
        };
        var row = new JobRow
        {
            Root = root,
            Title = title,
            State = state,
            Percent = percent,
            Bar = bar,
            Error = error,
            Pause = pause,
            Resume = resume,
            Retry = retry,
            Cancel = cancel,
        };
        void Wire(Button button, string action) => button.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.JobControl", () =>
        {
            if (row.Job is { } current) Run(ct => control(action, current, ct));
        });
        Wire(pause, "pauseJob");
        Wire(resume, "resumeJob");
        Wire(retry, "resumeJob");
        Wire(cancel, "cancelJob");
        host.Children.Add(root);
        return row;
    }

    private void RenderRecommendation()
    {
        _recommendationBox.Children.Clear();
        if (_recommendation is not { } recommendation) return;
        var tone = ToneBrush(TakeGroupsPolicy.RecommendationTone(recommendation));
        var header = SectionLabel(TakeGroupsPolicy.RecommendationHeader(recommendation));
        header.Foreground = tone;
        var reason = new TextBlock { Text = recommendation.Reason, FontSize = 13, Foreground = _text1, TextWrapping = TextWrapping.Wrap };
        SetFont(reason, "BodyFontFamily");
        var text = new StackPanel { Spacing = 4, Padding = new Thickness(12, 12, 14, 12) };
        text.Children.Add(header);
        text.Children.Add(reason);
        var rail = new Border { Width = 3, Background = tone };
        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(text, 1);
        grid.Children.Add(rail);
        grid.Children.Add(text);
        var root = new Border
        {
            Background = _panel,
            BorderBrush = _paneStroke,
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(10),
            Child = grid,
        };
        AutomationProperties.SetName(root, $"{TakeGroupsPolicy.RecommendationHeader(recommendation)}. {recommendation.Reason}");
        _recommendationBox.Children.Add(root);
    }

    private void RenderTakes()
    {
        StopCards();
        _cards.Clear();
        _takeGrid.Children.Clear();
        RenderRecommendation();
        var eventID = _selectedEvent?.Id;
        for (var i = 0; i < _takes.Count; i++)
        {
            var take = _takes[i];
            var card = BuildCard(i + 1, take, TakeGroupsPolicy.IsRecommended(_recommendation, take), eventID);
            _cards.Add(card);
            _takeGrid.Children.Add(card.Root);
            LoadThumb(card.Picture, take.Path);
        }
        _takeGrid.Visibility = _takes.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    private TakeCard BuildCard(int index, CatalogTake take, bool recommended, string? eventID)
    {
        var title = SectionLabel($"Take {index}");
        title.Foreground = recommended ? _gold : _label;
        title.VerticalAlignment = VerticalAlignment.Center;
        var star = new FontIcon
        {
            Glyph = "",
            FontSize = 10,
            Foreground = _gold,
            VerticalAlignment = VerticalAlignment.Center,
            Visibility = recommended ? Visibility.Visible : Visibility.Collapsed,
        };
        AutomationProperties.SetName(star, "Recommended");
        var (dotTone, dotTooltip, dotSpoken) = TakeGroupsPolicy.ReviewDot(take);
        var dot = new Microsoft.UI.Xaml.Shapes.Ellipse { Width = 7, Height = 7, Fill = ToneBrush(dotTone), VerticalAlignment = VerticalAlignment.Center };
        ToolTipService.SetToolTip(dot, dotTooltip);
        AutomationProperties.SetName(dot, dotSpoken);
        var header = new Grid
        {
            Padding = new Thickness(12, 9, 12, 9),
            ColumnSpacing = 8,
            BorderBrush = _hairline,
            BorderThickness = new Thickness(0, 0, 0, 1),
        };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(star, 1);
        Grid.SetColumn(dot, 3);
        header.Children.Add(title);
        header.Children.Add(star);
        header.Children.Add(dot);

        var picture = new Image { Stretch = Stretch.UniformToFill, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center };
        AutomationProperties.SetAccessibilityView(picture, AccessibilityView.Raw);
        var player = new MediaPlayerElement { AreTransportControlsEnabled = true, Stretch = Stretch.Uniform, Visibility = Visibility.Collapsed };
        AutomationProperties.SetName(player, "Video player");
        var play = Primary(new Button
        {
            Width = 36,
            Height = 36,
            MinHeight = 36,
            Padding = new Thickness(0),
            CornerRadius = new CornerRadius(18),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Content = new FontIcon { Glyph = "", FontSize = 14 },
        });
        ToolTipService.SetToolTip(play, $"Play take {index}");
        AutomationProperties.SetName(play, $"Play take {index}");
        var well = new Grid { Background = _well, Height = 146 };
        well.Children.Add(picture);
        well.Children.Add(player);
        well.Children.Add(play);
        well.SizeChanged += (_, e) => DebugLog.SafeRun("TakesWorkbench.CardWellSized", () =>
        {
            var height = Math.Round(e.NewSize.Width * 9 / 16);
            if (height > 0 && Math.Abs(well.Height - height) > 0.5) well.Height = height;
            well.Clip = new RectangleGeometry { Rect = new Rect(0, 0, e.NewSize.Width, e.NewSize.Height) };
        });

        var name = TextLine(HitPresentation.FileName(take.Path), 12, _text1, "MonoFontFamily");
        name.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        ToolTipService.SetToolTip(name, take.Path);
        var (headline, tone) = TakeGroupsPolicy.Headline(take);
        var outcome = TextLine(headline, 13, ToneBrush(tone));
        outcome.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        var body = new StackPanel { Spacing = 8, Padding = new Thickness(12) };
        body.Children.Add(name);
        body.Children.Add(outcome);
        if (!string.IsNullOrEmpty(take.Explanation))
        {
            var why = new TextBlock
            {
                Text = take.Explanation,
                FontSize = 12,
                Foreground = _text3,
                TextWrapping = TextWrapping.Wrap,
                MaxLines = 3,
                TextTrimming = TextTrimming.CharacterEllipsis,
            };
            SetFont(why, "BodyFontFamily");
            body.Children.Add(why);
        }
        if (take.QualityScore is { } quality) body.Children.Add(SharpnessRow(quality));

        var stack = new StackPanel();
        stack.Children.Add(header);
        stack.Children.Add(well);
        stack.Children.Add(body);
        if (eventID is not null) stack.Children.Add(CardFooter(eventID, take));
        var root = new Border
        {
            Background = _panel,
            CornerRadius = new CornerRadius(10),
            BorderBrush = recommended ? _gold : _paneStroke,
            BorderThickness = new Thickness(recommended ? 2 : 1),
            Child = stack,
        };
        AutomationProperties.SetName(root, TakeGroupsPolicy.CardSpoken(index, take, recommended));
        var card = new TakeCard { Root = root, Picture = picture, Player = player, Play = play, Take = take };
        play.Click += (_, _) => _ = DebugLog.SafeRunAsync("TakesWorkbench.PlayTake", () => PlayCardAsync(card));
        return card;
    }

    private Grid SharpnessRow(double quality)
    {
        var percent = TakeGroupsPolicy.Percent(quality);
        var label = TextLine("Sharpness", 11.5, _text3);
        label.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        label.VerticalAlignment = VerticalAlignment.Center;
        var fraction = Math.Clamp(quality, 0, 1);
        var bar = new Grid { Height = 4, CornerRadius = new CornerRadius(2), Background = _barTrack, VerticalAlignment = VerticalAlignment.Center };
        bar.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(fraction, GridUnitType.Star) });
        bar.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1 - fraction, GridUnitType.Star) });
        bar.Children.Add(new Border { CornerRadius = new CornerRadius(2), Background = _neutralBar });
        var value = TextLine(percent.ToString(CultureInfo.CurrentCulture), 11, _text3, "MonoFontFamily");
        value.TextAlignment = TextAlignment.Right;
        value.VerticalAlignment = VerticalAlignment.Center;
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(64) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(26) });
        Grid.SetColumn(bar, 1);
        Grid.SetColumn(value, 2);
        row.Children.Add(label);
        row.Children.Add(bar);
        row.Children.Add(value);
        AutomationProperties.SetName(row, $"Sharpness {percent} percent");
        return row;
    }

    private Border CardFooter(string eventID, CatalogTake take)
    {
        var selected = TakeGroupsPolicy.SelectedOutcome(take);
        var buttons = new FileID.Theme.Controls.FlowPanel { Spacing = 6 };
        foreach (var outcome in TakeGroupsPolicy.Outcomes)
        {
            var on = selected == outcome;
            var plain = new Button { Content = TakeGroupsPolicy.Label(outcome), Height = 28, MinHeight = 28, Padding = new Thickness(10, 0, 10, 0) };
            var button = on ? Primary(plain) : Secondary(plain);
            AutomationProperties.SetName(button, TakeGroupsPolicy.Spoken(outcome));
            if (on) AutomationProperties.SetItemStatus(button, "Selected");
            var feedback = TakeGroupsPolicy.OutcomeFeedback(eventID, take, outcome);
            button.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.Outcome", () =>
                SendManual(ManualRequest("setTakeFeedback", feedback: feedback)));
            buttons.Children.Add(button);
        }
        var prefer = take.Preferred
            ? AccentButton("★ Prefer", _gold, _goldWash)
            : Secondary(new Button { Content = "☆ Prefer", Height = 28, MinHeight = 28 });
        AutomationProperties.SetName(prefer, take.Preferred ? "Preferred" : "Prefer this take");
        if (take.Preferred) AutomationProperties.SetItemStatus(prefer, "Selected");
        var preferFeedback = TakeGroupsPolicy.PreferFeedback(eventID, take);
        prefer.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.Prefer", () =>
            SendManual(ManualRequest("setTakeFeedback", feedback: preferFeedback)));
        buttons.Children.Add(prefer);
        var undo = Ghost(new Button
        {
            Width = 28,
            Height = 28,
            MinHeight = 28,
            Padding = new Thickness(0),
            Content = new FontIcon { Glyph = "", FontSize = 12 },
        });
        undo.Foreground = _text3;
        ToolTipService.SetToolTip(undo, "Undo review");
        AutomationProperties.SetName(undo, "Undo review");
        var fileID = take.FileID;
        undo.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.UndoReview", () =>
            SendManual(ManualRequest("undoTakeFeedback", fileID: fileID, eventID: eventID)));
        buttons.Children.Add(undo);
        return new Border
        {
            Padding = new Thickness(10, 8, 10, 8),
            Background = _panelHeader,
            BorderBrush = _hairline,
            BorderThickness = new Thickness(0, 1, 0, 0),
            Child = buttons,
        };
    }

    // ---- Playback -------------------------------------------------------------

    private async Task PlayCardAsync(TakeCard card)
    {
        var path = card.Take.Path;
        if (!TakeGroupsPolicy.IsVideoPath(path))
        {
            if (!SafeOpen.TryOpenFile(path)) SafeOpen.Reveal(path);
            return;
        }
        Windows.Storage.StorageFile file;
        try
        {
            file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
        }
        catch (Exception)
        {
            ShowToast("This clip can't be found. Its drive may be disconnected.", null, PlainToastWindow);
            return;
        }
        if (!_cards.Contains(card)) return;
        StopCards();
        var ui = DispatcherQueue;
        var player = new MediaPlayer { AutoPlay = true };
        player.MediaFailed += (_, _) => DebugLog.SafeRun("TakesWorkbench.TakeMediaFailed", () => ui.TryEnqueue(() =>
        {
            if (!ReferenceEquals(card.MediaPlayer, player)) return;
            StopCard(card);
            if (!SafeOpen.TryOpenFile(path)) SafeOpen.Reveal(path);
        }));
        card.MediaPlayer = player;
        card.Player.SetMediaPlayer(player);
        card.Player.Visibility = Visibility.Visible;
        card.Picture.Visibility = Visibility.Collapsed;
        card.Play.Visibility = Visibility.Collapsed;
        player.Source = MediaSource.CreateFromStorageFile(file);
    }

    private void StopCards()
    {
        foreach (var card in _cards) StopCard(card);
    }

    private static void StopCard(TakeCard card)
    {
        if (card.MediaPlayer is { } player)
        {
            card.MediaPlayer = null;
            card.Player.SetMediaPlayer(null);
            try { player.Dispose(); }
            catch (Exception ex) { DebugLog.Warn("TakesWorkbench take player dispose: " + ex.Message); }
        }
        card.Player.Visibility = Visibility.Collapsed;
        card.Picture.Visibility = Visibility.Visible;
        card.Play.Visibility = Visibility.Visible;
    }

    private sealed class TakeCard
    {
        public required Border Root { get; init; }
        public required Image Picture { get; init; }
        public required MediaPlayerElement Player { get; init; }
        public required Button Play { get; init; }
        public required CatalogTake Take { get; init; }
        public MediaPlayer? MediaPlayer { get; set; }
    }

    private sealed class JobRow
    {
        public required Border Root { get; init; }
        public required TextBlock Title { get; init; }
        public required TextBlock State { get; init; }
        public required TextBlock Percent { get; init; }
        public required ProgressBar Bar { get; init; }
        public required TextBlock Error { get; init; }
        public required Button Pause { get; init; }
        public required Button Resume { get; init; }
        public required Button Retry { get; init; }
        public required Button Cancel { get; init; }
        public CatalogJob? Job { get; set; }
    }

    /// <summary>Adaptive take-card grid (macOS GridItem.adaptive(260...420), gap 12); each row is as tall as its tallest card.</summary>
    private sealed class TakeCardGrid : Panel
    {
        protected override Size MeasureOverride(Size availableSize)
        {
            var (columns, width) = TakeGroupsPolicy.CardColumns(availableSize.Width);
            double height = 0;
            double rowHeight = 0;
            for (var i = 0; i < Children.Count; i++)
            {
                var child = Children[i];
                child.Measure(new Size(width, double.PositiveInfinity));
                rowHeight = Math.Max(rowHeight, child.DesiredSize.Height);
                if ((i + 1) % columns != 0 && i != Children.Count - 1) continue;
                height += rowHeight + (i >= columns ? TakeGroupsPolicy.CardGap : 0);
                rowHeight = 0;
            }
            var used = columns * width + TakeGroupsPolicy.CardGap * (columns - 1);
            return new Size(double.IsFinite(availableSize.Width) ? Math.Min(availableSize.Width, used) : used, height);
        }

        protected override Size ArrangeOverride(Size finalSize)
        {
            var (columns, width) = TakeGroupsPolicy.CardColumns(finalSize.Width);
            double y = 0;
            for (var start = 0; start < Children.Count; start += columns)
            {
                var end = Math.Min(start + columns, Children.Count);
                double rowHeight = 0;
                for (var i = start; i < end; i++) rowHeight = Math.Max(rowHeight, Children[i].DesiredSize.Height);
                for (var i = start; i < end; i++)
                {
                    Children[i].Arrange(new Rect((i - start) * (width + TakeGroupsPolicy.CardGap), y, width, rowHeight));
                }
                y += rowHeight + TakeGroupsPolicy.CardGap;
            }
            return finalSize;
        }
    }
}
