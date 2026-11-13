using System.ComponentModel;
using System.Globalization;
using System.Numerics;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Hosting;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.System;
using Windows.UI;

namespace FileID.Views.Workbenches;

/// <summary>Best Takes (DESIGN_V3_PORT "Best Takes"): the engine groups bursts and repeated clips by
/// itself and scores each try without a model. The user checks the starred best try, keeps all, skips,
/// or moves the rest to Trash with Undo. Groups made by hand live in TakesWorkbench.Manual.cs. The
/// goal-ranking panel is left out: the shared engine reports it as unavailable.</summary>
internal sealed partial class TakesWorkbench : WorkbenchPanel
{
    private static readonly TimeSpan TrashUndoWindow = TimeSpan.FromSeconds(10);
    private static readonly TimeSpan DecisionUndoWindow = TimeSpan.FromSeconds(8);
    private static readonly TimeSpan PlainToastWindow = TimeSpan.FromSeconds(6);
    private const double TileWidth = 112;
    private const double TileGap = 8;
    private const int EagerRowThumbnails = 40;

    private IReadOnlyList<CatalogTakeAutoGroup> _groups = Array.Empty<CatalogTakeAutoGroup>();
    private CatalogTakeAutoGroup? _current;
    private IReadOnlyList<CatalogTakeQuality> _qualities = Array.Empty<CatalogTakeQuality>();
    private readonly HashSet<long> _keep = new();
    private int _focusedIndex;
    private bool _zoom;
    private bool _rendering;
    private bool _loadedOnce;
    private bool _trashing;
    private bool _focusStripWhenReady;
    private ThumbnailService? _thumbs;
    private Func<CancellationToken, Task>? _undoAction;
    private readonly DispatcherTimer _toastTimer = new();
    private readonly List<Tile> _tiles = new();
    private readonly Dictionary<Grid, (Image[] Images, string[] Paths)> _pendingRowThumbs = new();

    private readonly Brush _gold = ThemeHelper.GetBrushSafe("GoldBrush");
    private readonly Brush _ai = ThemeHelper.GetBrushSafe("AiBrush");
    private readonly Brush _delight = ThemeHelper.GetBrushSafe("DelightBrush");
    private readonly Brush _text1 = ThemeHelper.GetBrushSafe("Text1Brush");
    private readonly Brush _text2 = ThemeHelper.GetBrushSafe("Text2Brush");
    private readonly Brush _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
    private readonly Brush _label = ThemeHelper.GetBrushSafe("LabelBrush");
    private readonly Brush _onGold = ThemeHelper.GetBrushSafe("OnGoldBrush");
    private readonly Brush _hairline = ThemeHelper.GetBrushSafe("HairlineBrush");
    private readonly Brush _hairlineStrong = ThemeHelper.GetBrushSafe("HairlineStrongBrush");
    private readonly Brush _panel = ThemeHelper.GetBrushSafe("PanelBrush");
    private readonly Brush _barTrack = ThemeHelper.GetBrushSafe("ProgressTrackBrush");
    private readonly Brush _paneStroke = new SolidColorBrush(Color.FromArgb(0x1D, 0xFF, 0xFF, 0xFF));
    private readonly Brush _goldWash = new SolidColorBrush(Color.FromArgb(0x1F, 0xFF, 0xCC, 0x00));
    private readonly Brush _aiWash = new SolidColorBrush(Color.FromArgb(0x1F, 0xB1, 0x9B, 0xCE));
    private readonly Brush _neutralWash = new SolidColorBrush(Color.FromArgb(0x12, 0xFF, 0xFF, 0xFF));
    private readonly Brush _well = new SolidColorBrush(Color.FromArgb(0x59, 0x00, 0x00, 0x00));
    private readonly Brush _badgeBacking = new SolidColorBrush(Color.FromArgb(0x99, 0x00, 0x00, 0x00));
    private readonly Brush _thumbFill = new SolidColorBrush(Color.FromArgb(0x0A, 0xFF, 0xFF, 0xFF));
    private readonly Brush _dash = new SolidColorBrush(Color.FromArgb(0x29, 0xFF, 0xFF, 0xFF));
    private readonly Brush _pillStroke = new SolidColorBrush(Color.FromArgb(0x99, 0xA6, 0xAC, 0xB3));
    private readonly Brush _neutralBar = new SolidColorBrush(Color.FromArgb(0x99, 0xCF, 0xD3, 0xD8));
    private readonly Brush _focusRing = new SolidColorBrush(Color.FromArgb(0xE6, 0xFF, 0xCC, 0x00));

    private readonly TextBlock _groupHeader = SectionLabel("Possible groups");
    private readonly ProgressBar _groupBar = new() { IsIndeterminate = true, Width = 40, Visibility = Visibility.Collapsed };
    private readonly ListView _groupList = new() { SelectionMode = ListViewSelectionMode.Single, Padding = new Thickness(0, 8, 0, 8) };
    private readonly TextBlock _groupsEmpty = Meta("None yet. Scan a folder with photo bursts or several tries of a clip.");

    private readonly Border _toast;
    private readonly TextBlock _toastText = new() { FontSize = 12.5, TextWrapping = TextWrapping.Wrap, VerticalAlignment = VerticalAlignment.Center };
    private readonly Button _toastUndo = Secondary(new Button { Content = "Undo", MinHeight = 28, Height = 28 });

    private readonly Grid _notice;
    private readonly TextBlock _noticeTitle = new() { FontSize = 28, TextWrapping = TextWrapping.Wrap };
    private readonly TextBlock _noticeBody = Meta("");
    private readonly StackPanel _firstLoad = new() { Spacing = 10, Padding = new Thickness(0, 24, 0, 24), Visibility = Visibility.Collapsed };

    private readonly StackPanel _review = new() { Spacing = 16, Visibility = Visibility.Collapsed };
    private readonly TextBlock _title = new() { FontSize = 28 };
    private readonly TextBlock _date = new() { FontSize = 13, VerticalAlignment = VerticalAlignment.Bottom, Margin = new Thickness(0, 0, 0, 5) };
    private readonly Border _skipped;
    private readonly Button _trash = Primary(new Button { Content = "Keep the best, move the other 0 to Trash", Height = 30, MinHeight = 30 });
    private readonly Button _keepAll = Secondary(new Button { Content = "Keep all", Height = 28 });
    private readonly Button _skip = Ghost(new Button { Content = "Skip group", MinHeight = 28 });
    private readonly ProgressBar _trashBar = new() { IsIndeterminate = true, Width = 60, VerticalAlignment = VerticalAlignment.Center, Visibility = Visibility.Collapsed };
    private readonly StackPanel _scoring = new() { Spacing = 10, Padding = new Thickness(0, 24, 0, 24), Visibility = Visibility.Collapsed };
    private readonly TextBlock _scoringText = new() { FontSize = 13, TextWrapping = TextWrapping.Wrap };
    private readonly Grid _failure;
    private readonly TextBlock _failureTitle = new() { FontSize = 28, TextWrapping = TextWrapping.Wrap };
    private readonly TextBlock _failureBody = Meta("");
    private readonly Grid _compare = new() { ColumnSpacing = 12, Visibility = Visibility.Collapsed };
    private readonly ComparePane _left;
    private readonly ComparePane _right;
    private readonly StackPanel _filmstrip = new() { Spacing = 8, Visibility = Visibility.Collapsed };
    private readonly TextBlock _stripHeader = SectionLabel("All 0 photos · 0 to keep");
    private readonly Button _zoomButton = Ghost(new Button { Content = "Zoom to face", MinHeight = 28 });
    private readonly Button _askChat = Ghost(new Button { Content = "Ask about all", MinHeight = 28 });
    private readonly StackPanel _strip = new() { Orientation = Orientation.Horizontal, Spacing = TileGap, Padding = new Thickness(4) };
    private readonly ScrollViewer _stripScroll;
    private readonly ContentControl _stripHost;
    private readonly TextBlock _hint = Meta("");

    public TakesWorkbench() : base("Best Takes")
    {
        _groupHeader.Foreground = _ai;
        var lookAgain = Ghost(new Button
        {
            Width = 28,
            Height = 28,
            MinHeight = 28,
            Padding = new Thickness(0),
            Content = new FontIcon { Glyph = "\uE72C", FontSize = 11.5 },
        });
        lookAgain.Foreground = _text2;
        ToolTipService.SetToolTip(lookAgain, "Look again");
        AutomationProperties.SetName(lookAgain, "Look again");
        lookAgain.Click += (_, _) => Run(ct => LoadGroupsAsync(keepCurrent: true, ct));
        var header = new Grid { ColumnSpacing = 8, Padding = new Thickness(14, 10, 8, 4) };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _groupHeader.VerticalAlignment = VerticalAlignment.Center;
        _groupBar.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(_groupBar, 1);
        Grid.SetColumn(lookAgain, 2);
        header.Children.Add(_groupHeader);
        header.Children.Add(_groupBar);
        header.Children.Add(lookAgain);
        var intro = Meta("Bursts and repeated clips from your scans. Biggest and newest first.");
        intro.FontSize = 11.5;
        intro.Margin = new Thickness(14, 0, 14, 6);
        _groupsEmpty.Margin = new Thickness(14, 6, 14, 6);
        _groupsEmpty.Visibility = Visibility.Collapsed;
        var pane = new Grid();
        pane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        pane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        pane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        pane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        pane.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        pane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        pane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var manualPane = BuildManualPane();
        var proposalPane = BuildProposalPane();
        Grid.SetRow(header, 1);
        Grid.SetRow(intro, 2);
        Grid.SetRow(_groupsEmpty, 3);
        Grid.SetRow(_groupList, 4);
        Grid.SetRow(manualPane, 5);
        Grid.SetRow(_deletedFooter, 6);
        pane.Children.Add(proposalPane);
        pane.Children.Add(header);
        pane.Children.Add(intro);
        pane.Children.Add(_groupsEmpty);
        pane.Children.Add(_groupList);
        pane.Children.Add(manualPane);
        pane.Children.Add(_deletedFooter);
        pane.SizeChanged += (_, e) => DebugLog.SafeRun("TakesWorkbench.PaneSized", () =>
            manualPane.MaxHeight = Math.Max(180, e.NewSize.Height * 0.55));
        UseSidePane(pane, 280, paneOnLeft: true);

        _groupList.ContainerContentChanging += (_, args) => DebugLog.SafeRun("TakesWorkbench.RowRealized", () =>
        {
            if (!args.InRecycleQueue && args.Item is Grid row) LoadRowThumbs(row);
        });
        _groupList.SelectionChanged += (_, _) => DebugLog.SafeRun("TakesWorkbench.GroupSelected", () =>
        {
            if (_rendering || _groupList.SelectedIndex < 0 || _groupList.SelectedIndex >= _groups.Count) return;
            var group = _groups[_groupList.SelectedIndex];
            if (_current?.GroupKey == group.GroupKey && _proposalKey is null) return;
            LeaveManual();
            ClearProposalSelection();
            Run(ct => ScoreAsync(group, focusStrip: false, ct));
        });

        _toastUndo.Margin = new Thickness(12, 0, 0, 0);
        _toastUndo.Foreground = _gold;
        _toastUndo.BorderBrush = _gold;
        _toastUndo.Background = _goldWash;
        _toastUndo.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.ToastUndo", () =>
        {
            if (_undoAction is not { } undo) return;
            HideToast();
            Run(undo);
        });
        var toastRow = new Grid { ColumnSpacing = 8 };
        toastRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        toastRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(_toastUndo, 1);
        toastRow.Children.Add(_toastText);
        toastRow.Children.Add(_toastUndo);
        _toast = new Border
        {
            Padding = new Thickness(14, 8, 10, 8),
            CornerRadius = new CornerRadius(10),
            Background = _panel,
            BorderBrush = _hairlineStrong,
            BorderThickness = new Thickness(1),
            Child = toastRow,
            Visibility = Visibility.Collapsed,
        };
        _toastText.Foreground = _text1;
        SetFont(_toastText, "BodyFontFamily");
        _toastTimer.Tick += (_, _) => DebugLog.SafeRun("TakesWorkbench.ToastTimeout", HideToast);

        SetFont(_noticeTitle, "DisplayFontFamily");
        _noticeTitle.Foreground = _text1;
        _notice = DashedNotice(_noticeTitle, _noticeBody);
        var firstLoadText = new TextBlock { Text = "Looking for bursts and repeated clips…", FontSize = 13, Foreground = _text3 };
        SetFont(firstLoadText, "BodyFontFamily");
        _firstLoad.Children.Add(new ProgressBar { IsIndeterminate = true, Width = 160, HorizontalAlignment = HorizontalAlignment.Left });
        _firstLoad.Children.Add(firstLoadText);

        SetFont(_title, "DisplayFontFamily");
        _title.Foreground = _text1;
        SetFont(_date, "BodyFontFamily");
        _date.Foreground = _text2;
        _skipped = StatusPill("SKIPPED");
        _skipped.VerticalAlignment = VerticalAlignment.Bottom;
        _skipped.Margin = new Thickness(0, 0, 0, 6);
        _skipped.Visibility = Visibility.Collapsed;
        var heading = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        heading.Children.Add(_title);
        heading.Children.Add(_date);
        heading.Children.Add(_relationPill);
        heading.Children.Add(_skipped);
        _review.Children.Add(heading);
        _review.Children.Add(_reviewIntro);
        ToolTipService.SetToolTip(_keepAll, "Keep every one here and stop suggesting this group");
        ToolTipService.SetToolTip(_skip, "Decide later. Skipped groups move to the bottom of the list.");
        var actions = Row(_trash, _keepAll, _skip, _saveAsGroup, _trashBar);
        actions.VerticalAlignment = VerticalAlignment.Center;
        _review.Children.Add(actions);

        SetFont(_scoringText, "BodyFontFamily");
        _scoringText.Foreground = _text3;
        _scoring.Children.Add(new ProgressBar { IsIndeterminate = true, Width = 160, HorizontalAlignment = HorizontalAlignment.Left });
        _scoring.Children.Add(_scoringText);
        _review.Children.Add(_scoring);
        SetFont(_failureTitle, "DisplayFontFamily");
        _failureTitle.Foreground = _text1;
        _failure = DashedNotice(_failureTitle, _failureBody);
        _failure.Visibility = Visibility.Collapsed;
        _review.Children.Add(_failure);

        _compare.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _compare.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        _left = BuildPane();
        _right = BuildPane();
        Grid.SetColumn(_right.Root, 1);
        _compare.Children.Add(_left.Root);
        _compare.Children.Add(_right.Root);
        _review.Children.Add(_compare);

        var stripTop = new Grid { ColumnSpacing = 10 };
        stripTop.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        stripTop.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        stripTop.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _stripHeader.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(_askChat, 1);
        Grid.SetColumn(_zoomButton, 2);
        ToolTipService.SetToolTip(_zoomButton, "Z");
        ToolTipService.SetToolTip(_askChat, "Open Chat with these tries attached, then ask for the exact moment you want");
        _askChat.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.AskChat", AskChat);
        stripTop.Children.Add(_stripHeader);
        stripTop.Children.Add(_askChat);
        stripTop.Children.Add(_zoomButton);
        _filmstrip.Children.Add(stripTop);
        _stripScroll = new ScrollViewer
        {
            Content = _strip,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
            HorizontalScrollMode = ScrollMode.Enabled,
            VerticalScrollMode = ScrollMode.Disabled,
            IsTabStop = false,
        };
        _stripHost = new ContentControl
        {
            Content = _stripScroll,
            IsTabStop = true,
            UseSystemFocusVisuals = true,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            CornerRadius = new CornerRadius(8),
            FocusVisualMargin = new Thickness(-3),
            FocusVisualPrimaryThickness = new Thickness(2),
            FocusVisualSecondaryThickness = new Thickness(0),
            FocusVisualPrimaryBrush = _focusRing,
        };
        _stripHost.KeyDown += OnStripKeyDown;
        _filmstrip.Children.Add(_stripHost);
        _hint.FontSize = 11.5;
        _filmstrip.Children.Add(_hint);
        _review.Children.Add(_filmstrip);

        _trash.Click += (_, _) => _ = DebugLog.SafeRunAsync("TakesWorkbench.Trash", ConfirmTrashAsync);
        _keepAll.Click += (_, _) => Run(ct => DecideAsync("keepAll", ct));
        _skip.Click += (_, _) => Run(ct => DecideAsync("skip", ct));
        _zoomButton.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.Zoom", ToggleZoom);
        KeyDown += OnPageKeyDown;

        Inputs.Children.Add(_toast);
        Inputs.Children.Add(_firstLoad);
        Inputs.Children.Add(_notice);
        Inputs.Children.Add(_review);
        Inputs.Children.Add(BuildManualPage());
        ShowNotice();
    }

    protected override void OnOpened()
    {
        _thumbs = ThumbnailService.Shared;
        Engine.PropertyChanged += OnEngineChanged;
        if (AppViewModel.Instance.TakeBestTakesCandidates() is { Count: > 0 } handed) OpenWithCandidates(handed);
        _jobPoll.Start();
        Run(async ct =>
        {
            await ListEventsAsync(ct);
            await LoadGroupsAsync(keepCurrent: true, ct);
            await LoadProposalsAsync(null, ct);
        });
    }

    protected override void OnClosed()
    {
        Engine.PropertyChanged -= OnEngineChanged;
        _toastTimer.Stop();
        _jobPoll.Stop();
        StopPlayer(_left);
        StopPlayer(_right);
        StopCards();
        _thumbs = null;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("TakesWorkbench.OnEngineChanged", () =>
        {
            OnProposalEngineChanged(e);
            if (e.PropertyName != nameof(EngineClient.Phase) || Engine.Phase != ScanPhase.Completed) return;
            DebugLog.Info($"[ENGINE-SUB:TakesWorkbench] {e.PropertyName}=Completed");
            DispatcherQueue.TryEnqueue(() =>
            {
                if (!Lifetime.IsCancellationRequested) Run(ct => LoadGroupsAsync(keepCurrent: true, ct));
            });
        });

    private static FileID.IpcSchema.CatalogRequest Request(string action, IReadOnlyList<long>? fileIds = null, CatalogTakeDecision? decision = null, int? limit = null) =>
        new(Guid.NewGuid().ToString(), action, null, null, null, null, null, fileIds, Limit: limit, TakeDecision: decision);

    protected override void UpdateControls()
    {
        RunQueuedManual();
        RunStaleProposals();
        if (!_focusStripWhenReady || _filmstrip.Visibility != Visibility.Visible) return;
        _focusStripWhenReady = false;
        _stripHost.Focus(FocusState.Programmatic);
    }

    private bool IsVideo => _current?.Kind == "videos";

    private long? BestId => TakeReviewPolicy.Best(_qualities)?.PrimaryFileID;

    // ---- Loading ------------------------------------------------------------

    private async Task LoadGroupsAsync(bool keepCurrent, CancellationToken ct)
    {
        await UI(() =>
        {
            if (!keepCurrent) _current = null;
            _groupBar.Visibility = Visibility.Visible;
            if (!_loadedOnce && !ManualEngaged)
            {
                _notice.Visibility = Visibility.Collapsed;
                _firstLoad.Visibility = Visibility.Visible;
            }
        }, ct);
        try
        {
            var response = await Engine.CatalogAsync(Request("autoTakeGroups", limit: 100), ct);
            if (response.Status != "ok") throw new InvalidOperationException(response.Message ?? "Couldn't look for groups.");
            var groups = response.TakeGroups ?? Array.Empty<CatalogTakeAutoGroup>();
            CatalogTakeAutoGroup? next = null;
            await UI(() =>
            {
                _loadedOnce = true;
                next = RenderGroups(groups);
                Status.Text = "";
            }, ct);
            if (next is not null) await ScoreAsync(next, focusStrip: true, ct);
        }
        finally
        {
            await UI(() =>
            {
                _groupBar.Visibility = Visibility.Collapsed;
                _firstLoad.Visibility = Visibility.Collapsed;
                if (_current is null) ShowNotice();
            }, ct);
        }
    }

    private CatalogTakeAutoGroup? RenderGroups(IReadOnlyList<CatalogTakeAutoGroup> groups)
    {
        _rendering = true;
        try
        {
            _groups = groups;
            _groupHeader.Text = $"POSSIBLE GROUPS · {TakeReviewPolicy.OpenCount(groups)}";
            _groupsEmpty.Visibility = groups.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
            _pendingRowThumbs.Clear();
            _groupList.Items.Clear();
            for (var i = 0; i < groups.Count; i++)
            {
                var row = GroupRow(groups[i]);
                _groupList.Items.Add(row);
                if (i < EagerRowThumbnails) LoadRowThumbs(row);
            }
            if (_proposalKey is not null)
            {
                _groupList.SelectedIndex = -1;
                return null;
            }
            var currentKey = _current?.GroupKey;
            for (var i = 0; i < groups.Count; i++)
            {
                if (groups[i].GroupKey != currentKey) continue;
                _groupList.SelectedIndex = i;
                return null;
            }
            _current = null;
            _qualities = Array.Empty<CatalogTakeQuality>();
            if (!ManualEngaged && TakeReviewPolicy.FirstOpen(groups) is { } first)
            {
                _groupList.SelectedIndex = first;
                return groups[first];
            }
            _groupList.SelectedIndex = -1;
            ShowNotice();
            return null;
        }
        finally
        {
            _rendering = false;
        }
    }

    private Grid GroupRow(CatalogTakeAutoGroup group)
    {
        var open = TakeReviewPolicy.IsOpen(group);
        var date = TakeReviewPolicy.DateText(group.CapturedAt);
        var paths = group.Shots.Take(3).Select(s => s.Path).ToArray();
        var images = new Image[paths.Length];
        var thumbs = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        AutomationProperties.SetAccessibilityView(thumbs, AccessibilityView.Raw);
        for (var i = 0; i < paths.Length; i++)
        {
            images[i] = new Image { Stretch = Stretch.UniformToFill };
            thumbs.Children.Add(new Border
            {
                Width = 28,
                Height = 28,
                CornerRadius = new CornerRadius(4),
                Background = _thumbFill,
                Child = images[i],
            });
        }
        var title = new TextBlock
        {
            Text = TakeReviewPolicy.GroupTitle(group),
            FontSize = 13,
            FontWeight = Microsoft.UI.Text.FontWeights.Medium,
            Foreground = open ? _text1 : _text3,
        };
        SetFont(title, "BodyFontFamily");
        var meta = new TextBlock
        {
            Text = open ? date : $"{date} · Skipped",
            FontSize = 11.5,
            Foreground = _text3,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        SetFont(meta, "BodyFontFamily");
        var text = new StackPanel { Spacing = 1, VerticalAlignment = VerticalAlignment.Center };
        text.Children.Add(title);
        text.Children.Add(meta);
        var row = new Grid { ColumnSpacing = 10, Padding = new Thickness(2, 6, 2, 6) };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(text, 1);
        row.Children.Add(thumbs);
        row.Children.Add(text);
        AutomationProperties.SetName(row, TakeReviewPolicy.GroupSpoken(group, date));
        _pendingRowThumbs[row] = (images, paths);
        return row;
    }

    private void LoadRowThumbs(Grid row)
    {
        if (!_pendingRowThumbs.Remove(row, out var pending)) return;
        for (var i = 0; i < pending.Images.Length; i++) LoadThumb(pending.Images[i], pending.Paths[i]);
    }

    private void LoadThumb(Image target, string path)
    {
        var thumbs = _thumbs;
        if (thumbs is null) return;
        var ui = DispatcherQueue;
        var ct = Lifetime;
        _ = DebugLog.SafeRunAsync("TakesWorkbench.Thumbnail", async () =>
        {
            var bitmap = await thumbs.RequestAsync(path, null, ct).ConfigureAwait(false);
            if (bitmap is null || ct.IsCancellationRequested) return;
            ui.TryEnqueue(() =>
            {
                if (!ct.IsCancellationRequested) target.Source = bitmap;
            });
        });
    }

    private async Task ScoreAsync(CatalogTakeAutoGroup group, bool focusStrip, CancellationToken ct)
    {
        await UI(() => ShowScoring(group), ct);
        var ids = group.Shots.SelectMany(s => s.FileIDs).Take(800).ToArray();
        var response = await Engine.CatalogAsync(Request("scoreTakes", ids), ct);
        await UI(() =>
        {
            if (_current?.GroupKey != group.GroupKey) return;
            var qualities = response.TakeQualities ?? Array.Empty<CatalogTakeQuality>();
            if (response.Status != "ok" || qualities.Count == 0)
            {
                ShowFailure(group, response.Message ?? "FileID didn't answer in time. Try again in a moment.");
            }
            else
            {
                RenderReview(group, qualities);
                _focusStripWhenReady = focusStrip;
            }
            Status.Text = "";
        }, ct);
    }

    // ---- States ---------------------------------------------------------------

    private void ShowNotice()
    {
        _current = null;
        _qualities = Array.Empty<CatalogTakeQuality>();
        _review.Visibility = Visibility.Collapsed;
        if (ManualEngaged)
        {
            ShowManualPage();
            return;
        }
        _manualPage.Visibility = Visibility.Collapsed;
        if (_firstLoad.Visibility == Visibility.Visible) return;
        var anyGroups = _groups.Count > 0;
        _noticeTitle.Text = anyGroups ? "Pick a group" : "No tries to compare yet";
        _noticeBody.Text = anyGroups
            ? "Choose a possible group on the left to compare its tries."
            : "After a scan, Best Takes groups bursts and repeated clips here by itself. Scan a folder with photo bursts, or make a group by hand on the left.";
        _notice.Visibility = Visibility.Visible;
    }

    private void ShowScoring(CatalogTakeAutoGroup group)
    {
        StopPlayer(_left);
        StopPlayer(_right);
        _current = group;
        _qualities = Array.Empty<CatalogTakeQuality>();
        _keep.Clear();
        _zoom = false;
        RenderHeader(group);
        _notice.Visibility = Visibility.Collapsed;
        _manualPage.Visibility = Visibility.Collapsed;
        _review.Visibility = Visibility.Visible;
        _scoringText.Text = $"Checking sharpness, faces and exposure in {group.Shots.Count} {TakeReviewPolicy.Noun(group, group.Shots.Count)}…";
        _scoring.Visibility = Visibility.Visible;
        _failure.Visibility = Visibility.Collapsed;
        _compare.Visibility = Visibility.Collapsed;
        _filmstrip.Visibility = Visibility.Collapsed;
        UpdateActions();
    }

    private void ShowFailure(CatalogTakeAutoGroup group, string message)
    {
        _scoring.Visibility = Visibility.Collapsed;
        _failureTitle.Text = $"Couldn't score these {TakeReviewPolicy.Noun(group, 2)}";
        _failureBody.Text = message;
        _failure.Visibility = Visibility.Visible;
        UpdateActions();
    }

    private void RenderHeader(CatalogTakeAutoGroup group)
    {
        _title.Text = TakeReviewPolicy.GroupTitle(group);
        _date.Text = TakeReviewPolicy.DateText(group.CapturedAt);
        _skipped.Visibility = TakeReviewPolicy.IsOpen(group) ? Visibility.Collapsed : Visibility.Visible;
        RenderProposalHeader(group);
    }

    private void RenderReview(CatalogTakeAutoGroup group, IReadOnlyList<CatalogTakeQuality> qualities)
    {
        _qualities = qualities;
        _keep.Clear();
        foreach (var id in TakeReviewPolicy.DefaultKeep(qualities)) _keep.Add(id);
        var best = BestId;
        _focusedIndex = 0;
        for (var i = 0; i < qualities.Count; i++)
        {
            if (qualities[i].PrimaryFileID == best) _focusedIndex = i;
        }
        _scoring.Visibility = Visibility.Collapsed;
        _failure.Visibility = Visibility.Collapsed;
        _compare.Visibility = Visibility.Visible;
        _filmstrip.Visibility = Visibility.Visible;
        _hint.Text = TakeReviewPolicy.HintLine(group);
        AutomationProperties.SetName(_stripHost, $"{qualities.Count} {TakeReviewPolicy.Noun(group, qualities.Count)}. Left and right arrows move, Space marks keep.");
        RebuildStrip();
        UpdateAll();
        ScrollToFocused();
    }

    // ---- Updates --------------------------------------------------------------

    private void UpdateAll()
    {
        UpdateActions();
        UpdateTiles();
        UpdateCompare();
    }

    private void UpdateActions()
    {
        var group = _current;
        var ready = group is not null && _qualities.Count > 0 && !_trashing;
        var (label, reason) = TakeReviewPolicy.TrashButton(_qualities, _keep);
        _trash.Content = label;
        _trash.IsEnabled = ready && reason is null;
        ToolTipService.SetToolTip(_trash, group is null ? null : TakeReviewPolicy.PrimaryHelp(group, _qualities, _keep));
        _keepAll.IsEnabled = ready;
        _saveAsGroup.IsEnabled = group is not null && !_trashing;
        _skip.IsEnabled = ready && TakeReviewPolicy.IsOpen(group!);
        if (group is not null && _qualities.Count > 0)
        {
            _stripHeader.Text = TakeReviewPolicy.FilmstripHeader(group, _keep, _qualities.Count);
            _zoomButton.Content = TakeReviewPolicy.ZoomLabel(group, _zoom);
            var (_, ask) = ChatSelectionPolicy.TakeTargets(_qualities.Select(q => q.PrimaryFileID).ToArray(), _keep);
            _askChat.Content = ask;
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_askChat, $"{ask}: open Chat with these tries attached");
        }
    }

    private void AskChat()
    {
        if (_current is not { } group) return;
        var (ids, _) = ChatSelectionPolicy.TakeTargets(_qualities.Select(q => q.PrimaryFileID).ToArray(), _keep);
        var kind = group.Kind == "videos" ? "video" : "image";
        AppViewModel.Instance.RequestChatScope(ChatSelectionPolicy.Selected(ids, ids.Select(_ => kind).ToArray()));
    }

    private void RebuildStrip()
    {
        _strip.Children.Clear();
        _tiles.Clear();
        for (var i = 0; i < _qualities.Count; i++)
        {
            var tile = BuildTile(_qualities[i], i);
            _tiles.Add(tile);
            _strip.Children.Add(tile.Root);
            LoadThumb(tile.Picture, _qualities[i].Path);
        }
        _stripScroll.ChangeView(0, null, null, true);
    }

    private Tile BuildTile(CatalogTakeQuality quality, int index)
    {
        var picture = new Image { Stretch = Stretch.UniformToFill };
        var number = new TextBlock { Text = (index + 1).ToString(CultureInfo.CurrentCulture), FontSize = 10.5, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
        SetFont(number, "MonoFontFamily");
        var badge = new Border { Padding = new Thickness(5, 1, 5, 1), CornerRadius = new CornerRadius(3), Child = number };
        var star = new FontIcon { Glyph = "\uE735", FontSize = 9, Foreground = _gold, VerticalAlignment = VerticalAlignment.Center };
        var badges = new StackPanel
        {
            Orientation = Orientation.Horizontal,
            Spacing = 4,
            Margin = new Thickness(5),
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Top,
        };
        badges.Children.Add(badge);
        badges.Children.Add(star);
        var check = new Border
        {
            Width = 15,
            Height = 15,
            CornerRadius = new CornerRadius(7.5),
            Background = _delight,
            Margin = new Thickness(5),
            HorizontalAlignment = HorizontalAlignment.Right,
            VerticalAlignment = VerticalAlignment.Top,
            Child = new FontIcon { Glyph = "\uE73E", FontSize = 9, Foreground = _onGold },
        };
        var stroke = new Border { CornerRadius = new CornerRadius(8), IsHitTestVisible = false };
        var root = new Grid
        {
            Width = TileWidth,
            Height = 76,
            CornerRadius = new CornerRadius(8),
            Background = _thumbFill,
        };
        root.Children.Add(picture);
        root.Children.Add(badges);
        root.Children.Add(check);
        root.Children.Add(stroke);
        var name = System.IO.Path.GetFileName(quality.Path);
        ToolTipService.SetToolTip(root, quality.Reasons.Count == 0 ? name : $"{name} · {string.Join(", ", quality.Reasons)}");
        AutomationProperties.SetHelpText(root, string.Join(", ", quality.Reasons));
        root.Tapped += (_, _) => DebugLog.SafeRun("TakesWorkbench.TileTapped", () =>
        {
            _stripHost.Focus(FocusState.Pointer);
            FocusTry(index);
        });
        root.DoubleTapped += (_, _) => DebugLog.SafeRun("TakesWorkbench.TileDoubleTapped", () => ToggleKeep(index));
        return new Tile { Root = root, Picture = picture, Badge = badge, Number = number, Star = star, Check = check, Stroke = stroke };
    }

    private void UpdateTiles()
    {
        if (_current is not { } group) return;
        var best = BestId;
        for (var i = 0; i < _tiles.Count && i < _qualities.Count; i++)
        {
            var tile = _tiles[i];
            var quality = _qualities[i];
            var kept = _keep.Contains(quality.PrimaryFileID);
            var isBest = quality.PrimaryFileID == best;
            var focused = i == _focusedIndex;
            tile.Picture.Opacity = kept ? 1 : 0.62;
            tile.Badge.Background = isBest ? _gold : _badgeBacking;
            tile.Number.Foreground = isBest ? _onGold : _text1;
            tile.Star.Visibility = isBest ? Visibility.Visible : Visibility.Collapsed;
            tile.Check.Visibility = kept ? Visibility.Visible : Visibility.Collapsed;
            tile.Stroke.BorderBrush = focused ? _text1 : isBest ? _gold : _hairlineStrong;
            tile.Stroke.BorderThickness = new Thickness(focused || isBest ? 2 : 1);
            AutomationProperties.SetName(tile.Root, TakeReviewPolicy.TileSpoken(group, quality, i + 1, _qualities.Count, isBest, kept));
            if (tile.Focused != focused)
            {
                tile.Focused = focused;
                ScaleTile(tile.Root, focused ? 1.04f : 1f);
            }
        }
    }

    private static void ScaleTile(UIElement root, float scale)
    {
        var visual = ElementCompositionPreview.GetElementVisual(root);
        visual.CenterPoint = new Vector3((float)(TileWidth / 2), 38, 0);
        if (ReducedMotion.Instance.IsReduced)
        {
            SpringEasing.SetScalar(root, "Scale.X", scale);
            SpringEasing.SetScalar(root, "Scale.Y", scale);
        }
        else
        {
            SpringEasing.AnimateScalar(root, "Scale.X", scale, SpringEasing.Tokens.Tight.Response, SpringEasing.Tokens.Tight.DampingFraction);
            SpringEasing.AnimateScalar(root, "Scale.Y", scale, SpringEasing.Tokens.Tight.Response, SpringEasing.Tokens.Tight.DampingFraction);
        }
    }

    private void UpdateCompare()
    {
        if (_current is not { } group || _qualities.Count == 0) return;
        var focusedId = _qualities[Math.Clamp(_focusedIndex, 0, _qualities.Count - 1)].PrimaryFileID;
        if (TakeReviewPolicy.ComparePair(_qualities, focusedId) is { } pair)
        {
            var other = _qualities[pair.Other];
            ShowPane(_left, group, _qualities[pair.Best], pair.Best, "Best", isBest: true);
            ShowPane(_right, group, other, pair.Other, other.PrimaryFileID == focusedId ? "This try" : "Runner-up", isBest: false);
            return;
        }
        var bestIndex = 0;
        for (var i = 0; i < _qualities.Count; i++)
        {
            if (_qualities[i].PrimaryFileID == BestId) bestIndex = i;
        }
        ShowPane(_left, group, _qualities[bestIndex], bestIndex, "Best", isBest: true);
        StopPlayer(_right);
        _right.Root.Visibility = Visibility.Collapsed;
    }

    // ---- Compare panes ----------------------------------------------------------

    private ComparePane BuildPane()
    {
        var title = SectionLabel("Best");
        title.VerticalAlignment = VerticalAlignment.Center;
        var star = new FontIcon { Glyph = "\uE735", FontSize = 10, Foreground = _gold, VerticalAlignment = VerticalAlignment.Center };
        AutomationProperties.SetAccessibilityView(star, AccessibilityView.Raw);
        var keepButton = Ghost(new Button { MinHeight = 28, Padding = new Thickness(6, 0, 6, 0) });
        var header = new Grid
        {
            Height = 36,
            Padding = new Thickness(12, 0, 6, 0),
            ColumnSpacing = 8,
            BorderBrush = _hairline,
            BorderThickness = new Thickness(0, 0, 0, 1),
        };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(star, 1);
        Grid.SetColumn(keepButton, 3);
        header.Children.Add(title);
        header.Children.Add(star);
        header.Children.Add(keepButton);

        var picture = new Image
        {
            Stretch = Stretch.Uniform,
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
        };
        AutomationProperties.SetAccessibilityView(picture, AccessibilityView.Raw);
        var player = new MediaPlayerElement
        {
            AreTransportControlsEnabled = true,
            Stretch = Stretch.Uniform,
            Visibility = Visibility.Collapsed,
        };
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
            Content = new FontIcon { Glyph = "\uE768", FontSize = 14 },
            Visibility = Visibility.Collapsed,
        });
        var well = new Grid { Height = 340, Background = _well };
        well.Children.Add(picture);
        well.Children.Add(player);
        well.Children.Add(play);

        var name = new TextBlock
        {
            FontSize = 12,
            FontWeight = Microsoft.UI.Text.FontWeights.Medium,
            Foreground = _text1,
            TextTrimming = TextTrimming.CharacterEllipsis,
            VerticalAlignment = VerticalAlignment.Center,
        };
        SetFont(name, "MonoFontFamily");
        var raw = Chip("+ RAW");
        raw.VerticalAlignment = VerticalAlignment.Center;
        var score = new TextBlock
        {
            FontSize = 12,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            Foreground = _text2,
            VerticalAlignment = VerticalAlignment.Center,
        };
        SetFont(score, "MonoFontFamily");
        var nameRow = new Grid { ColumnSpacing = 8 };
        nameRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        nameRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        nameRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(raw, 1);
        Grid.SetColumn(score, 2);
        nameRow.Children.Add(name);
        nameRow.Children.Add(raw);
        nameRow.Children.Add(score);
        var barFill = new Border { CornerRadius = new CornerRadius(2) };
        var bar = new Grid { Height = 4, CornerRadius = new CornerRadius(2), Background = _barTrack };
        bar.ColumnDefinitions.Add(new ColumnDefinition());
        bar.ColumnDefinitions.Add(new ColumnDefinition());
        bar.Children.Add(barFill);
        AutomationProperties.SetAccessibilityView(bar, AccessibilityView.Raw);
        var chips = new FileID.Theme.Controls.FlowPanel { Spacing = 6 };
        var noChips = Meta(TakeReviewPolicy.NoReasons);
        noChips.FontSize = 11.5;
        var body = new StackPanel { Spacing = 8, Padding = new Thickness(12) };
        body.Children.Add(nameRow);
        body.Children.Add(bar);
        body.Children.Add(chips);
        body.Children.Add(noChips);

        var stack = new StackPanel();
        stack.Children.Add(header);
        stack.Children.Add(well);
        stack.Children.Add(body);
        var root = new Border
        {
            Background = _panel,
            CornerRadius = new CornerRadius(10),
            BorderBrush = _paneStroke,
            BorderThickness = new Thickness(1),
            Child = stack,
            VerticalAlignment = VerticalAlignment.Stretch,
        };
        var pane = new ComparePane
        {
            Root = root,
            Title = title,
            Star = star,
            KeepButton = keepButton,
            Well = well,
            Picture = picture,
            Player = player,
            Play = play,
            Name = name,
            Raw = raw,
            Score = score,
            Bar = bar,
            BarFill = barFill,
            Chips = chips,
            NoChips = noChips,
        };
        keepButton.Click += (_, _) => DebugLog.SafeRun("TakesWorkbench.PaneKeep", () =>
        {
            if (pane.Quality is not { } quality) return;
            for (var i = 0; i < _qualities.Count; i++)
            {
                if (_qualities[i].PrimaryFileID == quality.PrimaryFileID) ToggleKeep(i);
            }
        });
        play.Click += (_, _) => _ = DebugLog.SafeRunAsync("TakesWorkbench.Play", () => PlayAsync(pane));
        well.SizeChanged += (_, e) => DebugLog.SafeRun("TakesWorkbench.WellSized", () =>
        {
            well.Clip = new RectangleGeometry { Rect = new Windows.Foundation.Rect(0, 0, e.NewSize.Width, e.NewSize.Height) };
            ApplyZoom(pane);
        });
        picture.SizeChanged += (_, _) => DebugLog.SafeRun("TakesWorkbench.PictureSized", () => ApplyZoom(pane));
        return pane;
    }

    private void ShowPane(ComparePane pane, CatalogTakeAutoGroup group, CatalogTakeQuality quality, int index, string title, bool isBest)
    {
        var count = _qualities.Count;
        var name = System.IO.Path.GetFileName(quality.Path);
        var kept = _keep.Contains(quality.PrimaryFileID);
        pane.Root.Visibility = Visibility.Visible;
        pane.Quality = quality;
        pane.Title.Text = $"{title} · {index + 1} of {count}".ToUpperInvariant();
        pane.Title.Foreground = isBest ? _gold : _label;
        pane.Star.Visibility = isBest ? Visibility.Visible : Visibility.Collapsed;
        pane.Root.BorderBrush = isBest ? _gold : _paneStroke;
        pane.Root.BorderThickness = new Thickness(isBest ? 2 : 1);
        RenderKeepButton(pane.KeepButton, group, kept);
        pane.Name.Text = name;
        ToolTipService.SetToolTip(pane.Name, quality.Path);
        pane.Raw.Visibility = TakeReviewPolicy.HasRawTwin(quality) ? Visibility.Visible : Visibility.Collapsed;
        var score = TakeReviewPolicy.Score(quality);
        pane.Score.Text = score.ToString(CultureInfo.CurrentCulture);
        AutomationProperties.SetName(pane.Score, $"Score {score} of 100");
        var fraction = Math.Clamp(quality.Score, 0, 1);
        pane.Bar.ColumnDefinitions[0].Width = new GridLength(fraction, GridUnitType.Star);
        pane.Bar.ColumnDefinitions[1].Width = new GridLength(1 - fraction, GridUnitType.Star);
        pane.BarFill.Background = isBest ? _gold : _neutralBar;
        RenderChips(pane, quality);
        AutomationProperties.SetName(pane.Root, TakeReviewPolicy.PaneSpoken(title, group, index + 1, count, name));
        ShowPicture(pane, quality);
        ApplyZoom(pane);
    }

    private void RenderKeepButton(Button button, CatalogTakeAutoGroup group, bool kept)
    {
        var label = new TextBlock { Text = "Keep", FontSize = 11.5, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center };
        SetFont(label, "BodyFontFamily");
        var content = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        if (kept)
        {
            content.Children.Add(new FontIcon { Glyph = "\uE73E", FontSize = 11, Foreground = _delight });
            label.Foreground = _delight;
        }
        else
        {
            label.Text = "Mark keep";
            label.Foreground = _text3;
        }
        content.Children.Add(label);
        button.Content = content;
        var noun = TakeReviewPolicy.Noun(group, 1);
        ToolTipService.SetToolTip(button, kept ? $"Don't keep this {noun} (Space)" : $"Keep this {noun} (Space)");
        AutomationProperties.SetName(button, kept ? "Don't keep" : "Keep");
    }

    private void RenderChips(ComparePane pane, CatalogTakeQuality quality)
    {
        pane.Chips.Children.Clear();
        var reasons = TakeReviewPolicy.Reasons(quality);
        foreach (var reason in reasons)
        {
            var tone = TakeReviewPolicy.Tone(reason);
            var text = new TextBlock
            {
                Text = reason,
                FontSize = 11.5,
                FontWeight = Microsoft.UI.Text.FontWeights.Medium,
                Foreground = tone switch
                {
                    TakeReviewPolicy.ReasonTone.Positive => _gold,
                    TakeReviewPolicy.ReasonTone.Goal => _ai,
                    _ => _text2,
                },
            };
            SetFont(text, "BodyFontFamily");
            pane.Chips.Children.Add(new Border
            {
                Padding = new Thickness(7, 2, 7, 2),
                CornerRadius = new CornerRadius(4),
                Background = tone switch
                {
                    TakeReviewPolicy.ReasonTone.Positive => _goldWash,
                    TakeReviewPolicy.ReasonTone.Goal => _aiWash,
                    _ => _neutralWash,
                },
                Child = text,
            });
        }
        AutomationProperties.SetName(pane.Chips, string.Join(", ", reasons));
        pane.Chips.Visibility = reasons.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        pane.NoChips.Visibility = reasons.Count > 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    private void ShowPicture(ComparePane pane, CatalogTakeQuality quality)
    {
        var path = quality.Path;
        if (pane.LoadedPath == path) return;
        StopPlayer(pane);
        pane.LoadedPath = path;
        pane.Picture.Source = null;
        pane.Play.Visibility = IsVideo ? Visibility.Visible : Visibility.Collapsed;
        ToolTipService.SetToolTip(pane.Play, quality.PeakSeconds is { } peak ? $"Play from the most action, {Clock(peak)}" : "Play");
        AutomationProperties.SetName(pane.Play, "Play");
        if (IsVideo)
        {
            LoadPanePoster(pane, path);
            return;
        }
        try
        {
            var bitmap = new BitmapImage { DecodePixelWidth = 1600 };
            bitmap.ImageFailed += (_, _) => DebugLog.SafeRun("TakesWorkbench.PictureFailed", () =>
            {
                if (pane.LoadedPath == path) LoadShellPicture(pane, path, 1024);
            });
            bitmap.UriSource = new Uri(path);
            pane.Picture.Source = bitmap;
        }
        catch (Exception ex) when (ex is UriFormatException or ArgumentException)
        {
            LoadShellPicture(pane, path, 1024);
        }
    }

    private void LoadPanePoster(ComparePane pane, string path)
    {
        var thumbs = _thumbs;
        if (thumbs is null) return;
        var ui = DispatcherQueue;
        var ct = Lifetime;
        _ = DebugLog.SafeRunAsync("TakesWorkbench.Poster", async () =>
        {
            var bitmap = await thumbs.RequestAsync(path, null, ct).ConfigureAwait(false);
            if (bitmap is null || ct.IsCancellationRequested) return;
            ui.TryEnqueue(() =>
            {
                if (!ct.IsCancellationRequested && pane.LoadedPath == path) pane.Picture.Source = bitmap;
            });
        });
    }

    private void LoadShellPicture(ComparePane pane, string path, uint size)
    {
        if (ThumbnailService.SkipShellThumbnailForExtension(path)) return;
        var ui = DispatcherQueue;
        _ = DebugLog.SafeRunAsync("TakesWorkbench.ShellPicture", async () =>
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            var thumb = await file.GetThumbnailAsync(
                Windows.Storage.FileProperties.ThumbnailMode.SingleItem,
                size,
                Windows.Storage.FileProperties.ThumbnailOptions.ResizeThumbnail);
            if (thumb is null) return;
            if (!ui.TryEnqueue(async () =>
            {
                try
                {
                    if (pane.LoadedPath != path) return;
                    var bitmap = new BitmapImage();
                    await bitmap.SetSourceAsync(thumb);
                    if (pane.LoadedPath == path) pane.Picture.Source = bitmap;
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("TakesWorkbench preview failed: " + ex.Message);
                }
                finally
                {
                    thumb.Dispose();
                }
            }))
            {
                thumb.Dispose();
            }
        });
    }

    private async Task PlayAsync(ComparePane pane)
    {
        if (pane.Quality is not { } quality) return;
        var path = quality.Path;
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
        if (pane.LoadedPath != path) return;
        StopPlayer(pane);
        var ui = DispatcherQueue;
        var start = quality.PeakSeconds is { } peak && double.IsFinite(peak) ? Math.Max(0, peak - 1) : 0;
        var player = new MediaPlayer { AutoPlay = false };
        player.MediaOpened += (_, _) => DebugLog.SafeRun("TakesWorkbench.MediaOpened", () => ui.TryEnqueue(() =>
        {
            if (!ReferenceEquals(pane.MediaPlayer, player)) return;
            player.PlaybackSession.Position = TimeSpan.FromSeconds(start);
            player.Play();
        }));
        player.MediaFailed += (_, _) => DebugLog.SafeRun("TakesWorkbench.MediaFailed", () => ui.TryEnqueue(() =>
        {
            if (!ReferenceEquals(pane.MediaPlayer, player)) return;
            StopPlayer(pane);
            if (!SafeOpen.TryOpenFile(path)) SafeOpen.Reveal(path);
        }));
        pane.MediaPlayer = player;
        pane.Player.SetMediaPlayer(player);
        pane.Player.Visibility = Visibility.Visible;
        pane.Picture.Visibility = Visibility.Collapsed;
        pane.Play.Visibility = Visibility.Collapsed;
        player.Source = MediaSource.CreateFromStorageFile(file);
    }

    private void StopPlayer(ComparePane pane)
    {
        if (pane.MediaPlayer is { } player)
        {
            pane.MediaPlayer = null;
            pane.Player.SetMediaPlayer(null);
            try { player.Dispose(); }
            catch (Exception ex) { DebugLog.Warn("TakesWorkbench player dispose: " + ex.Message); }
        }
        pane.Player.Visibility = Visibility.Collapsed;
        pane.Picture.Visibility = Visibility.Visible;
        pane.Play.Visibility = IsVideo && pane.Quality is not null ? Visibility.Visible : Visibility.Collapsed;
    }

    private void ApplyZoom(ComparePane pane)
    {
        if (pane.Quality is not { } quality) return;
        var box = _zoom ? TakeReviewPolicy.ZoomBox(quality) : null;
        var (scale, x, y) = TakeReviewPolicy.ZoomTransform(
            pane.Well.ActualWidth, pane.Well.ActualHeight, pane.Picture.ActualWidth, pane.Picture.ActualHeight, box);
        ElementCompositionPreview.SetIsTranslationEnabled(pane.Picture, true);
        var visual = ElementCompositionPreview.GetElementVisual(pane.Picture);
        visual.CenterPoint = Vector3.Zero;
        if (ReducedMotion.Instance.IsReduced)
        {
            visual.StopAnimation("Scale.X");
            visual.StopAnimation("Scale.Y");
            visual.StopAnimation("Translation.X");
            visual.StopAnimation("Translation.Y");
            visual.Scale = new Vector3((float)scale, (float)scale, 1);
            visual.Properties.InsertVector3("Translation", new Vector3((float)x, (float)y, 0));
            return;
        }
        var spring = SpringEasing.Tokens.Standard;
        SpringEasing.AnimateScalar(pane.Picture, "Scale.X", (float)scale, spring.Response, spring.DampingFraction);
        SpringEasing.AnimateScalar(pane.Picture, "Scale.Y", (float)scale, spring.Response, spring.DampingFraction);
        SpringEasing.AnimateScalar(pane.Picture, "Translation.X", (float)x, spring.Response, spring.DampingFraction);
        SpringEasing.AnimateScalar(pane.Picture, "Translation.Y", (float)y, spring.Response, spring.DampingFraction);
    }

    // ---- Interaction ------------------------------------------------------------

    private void OnStripKeyDown(object sender, KeyRoutedEventArgs e)
        => DebugLog.SafeRun("TakesWorkbench.StripKey", () =>
        {
            switch (e.Key)
            {
                case VirtualKey.Left:
                    FocusTry(_focusedIndex - 1);
                    break;
                case VirtualKey.Right:
                    FocusTry(_focusedIndex + 1);
                    break;
                case VirtualKey.Space:
                    ToggleKeep(_focusedIndex);
                    break;
                case VirtualKey.Z:
                    ToggleZoom();
                    break;
                case VirtualKey.Enter:
                    _ = DebugLog.SafeRunAsync("TakesWorkbench.StripReturn", ConfirmTrashAsync);
                    break;
                default:
                    return;
            }
            e.Handled = true;
        });

    private void OnPageKeyDown(object sender, KeyRoutedEventArgs e)
        => DebugLog.SafeRun("TakesWorkbench.PageKey", () =>
        {
            if (e.Handled || e.Key != VirtualKey.Enter || e.OriginalSource is TextBox or Microsoft.UI.Xaml.Controls.Primitives.ButtonBase or ListViewItem) return;
            e.Handled = true;
            _ = DebugLog.SafeRunAsync("TakesWorkbench.PageReturn", ConfirmTrashAsync);
        });

    private void FocusTry(int index)
    {
        if (_qualities.Count == 0) return;
        index = Math.Clamp(index, 0, _qualities.Count - 1);
        if (index != _focusedIndex)
        {
            _focusedIndex = index;
            StopPlayer(_left);
            StopPlayer(_right);
        }
        UpdateTiles();
        UpdateCompare();
        ScrollToFocused();
    }

    private void ScrollToFocused()
    {
        var center = 4 + _focusedIndex * (TileWidth + TileGap) + TileWidth / 2;
        var target = Math.Max(0, center - _stripScroll.ViewportWidth / 2);
        _stripScroll.ChangeView(target, null, null, ReducedMotion.Instance.IsReduced);
    }

    private void ToggleKeep(int index)
    {
        if (index < 0 || index >= _qualities.Count || _trashing) return;
        var quality = _qualities[index];
        TakeReviewPolicy.Toggle(_keep, quality.PrimaryFileID);
        UpdateAll();
        Announce(TakeReviewPolicy.KeepAnnouncement(quality, _keep.Contains(quality.PrimaryFileID)));
    }

    private void ToggleZoom()
    {
        if (_current is not { } group || _qualities.Count == 0) return;
        _zoom = !_zoom;
        _zoomButton.Content = TakeReviewPolicy.ZoomLabel(group, _zoom);
        ApplyZoom(_left);
        ApplyZoom(_right);
    }

    private void Announce(string text)
    {
        try
        {
            var peer = FrameworkElementAutomationPeer.FromElement(_stripHost)
                ?? FrameworkElementAutomationPeer.CreatePeerForElement(_stripHost);
            peer?.RaiseNotificationEvent(
                AutomationNotificationKind.ActionCompleted,
                AutomationNotificationProcessing.ImportantMostRecent,
                text,
                "FileID.BestTakes");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("TakesWorkbench announcement failed: " + ex.Message);
        }
    }

    // ---- Toast ----------------------------------------------------------------

    private void ShowToast(string text, Func<CancellationToken, Task>? undo, TimeSpan window)
    {
        _toastTimer.Stop();
        _toastText.Text = text;
        _undoAction = undo;
        _toastUndo.Visibility = undo is null ? Visibility.Collapsed : Visibility.Visible;
        _toast.Visibility = Visibility.Visible;
        _toastTimer.Interval = window;
        _toastTimer.Start();
        Announce(text);
    }

    private void HideToast()
    {
        _toastTimer.Stop();
        _toast.Visibility = Visibility.Collapsed;
        _undoAction = null;
    }

    // ---- Actions --------------------------------------------------------------

    private async Task ConfirmTrashAsync()
    {
        if (_current is not { } group || _trashing || Busy || _qualities.Count == 0) return;
        if (TakeReviewPolicy.TrashButton(_qualities, _keep).DisabledReason is not null) return;
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = TakeReviewPolicy.ConfirmHeading(group, _qualities, _keep),
            Content = TakeReviewPolicy.ConfirmBody(_qualities, _keep),
            PrimaryButtonText = "Move to Trash",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        ContentDialogResult result;
        try
        {
            result = await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("TakesWorkbench confirmation unavailable: " + ex.Message);
            return;
        }
        if (result == ContentDialogResult.Primary) Run(TrashAsync);
    }

    private async Task TrashAsync(CancellationToken ct)
    {
        if (_current is not { } group) return;
        var qualities = _qualities;
        var keep = _keep.ToHashSet();
        var ids = TakeReviewPolicy.TrashIds(qualities, keep);
        if (ids.Count == 0) return;
        var doomed = qualities.Where(q => !keep.Contains(q.PrimaryFileID)).ToArray();
        await UI(() =>
        {
            _trashing = true;
            _trashBar.Visibility = Visibility.Visible;
            UpdateActions();
        }, ct);
        try
        {
            BulkActionResult result;
            try
            {
                result = await Engine.WaitForBulkActionResultAsync(
                    "trashFiles",
                    () => Engine.TrashFilesAsync(ids),
                    TimeSpan.FromSeconds(120),
                    ct: ct);
            }
            catch (TimeoutException)
            {
                await UI(() => ShowToast("Moving to Trash didn't finish. Check the Trash before trying again.", null, PlainToastWindow), ct);
                return;
            }
            if (result.Succeeded == 0)
            {
                var why = TrashFailureText.Summary(result);
                var reason = why.Length > 0 ? why : "Nothing was moved.";
                await UI(() => ShowToast($"Couldn't move to Trash. {reason}", null, PlainToastWindow), ct);
                return;
            }
            var keptTogether = TakeReviewPolicy.KeptTogether(group, qualities, keep, "keepAll");
            if (keptTogether is not null) await Engine.CatalogAsync(Request("decideTakeGroup", decision: keptTogether), ct);
            var movedIds = result.Messages?.Where(m => m.Ok && m.FileId is not null).Select(m => m.FileId!.Value).ToArray() ?? Array.Empty<long>();
            var moved = movedIds.Length == 0 ? doomed.Length : TakeReviewPolicy.MovedShots(doomed, movedIds);
            var text = TakeReviewPolicy.MovedText(group, moved, result.Failed);
            if (result.Failed > 0 && TrashFailureText.Summary(result) is { Length: > 0 } left) text = $"{TakeReviewPolicy.MovedText(group, moved, 0)}. {left}";
            var batch = result.Action.Contains(':', StringComparison.Ordinal) ? result.Action[(result.Action.IndexOf(':', StringComparison.Ordinal) + 1)..] : null;
            var reopen = TakeReviewPolicy.KeptTogether(group, qualities, keep, "reopen");
            await UI(() => ShowToast(
                text,
                batch is null ? null : c => UndoTrashAsync(batch, ids.Count, reopen, c),
                TrashUndoWindow), ct);
        }
        finally
        {
            await UI(() =>
            {
                _trashing = false;
                _trashBar.Visibility = Visibility.Collapsed;
            }, ct);
        }
        await AfterResolveAsync(group.GroupKey, ct);
    }

    private async Task UndoTrashAsync(string batch, int files, CatalogTakeDecision? reopen, CancellationToken ct)
    {
        var restored = await Engine.RestoreFromTrashAsync(batch);
        if (reopen is not null) await Engine.CatalogAsync(Request("decideTakeGroup", decision: reopen), ct);
        await UI(() => ShowToast(
            TakeReviewPolicy.PutBackText(restored ? (uint)files : 0, restored ? null : "Some files couldn't be put back. Check the Trash."),
            null,
            PlainToastWindow), ct);
        await AfterResolveAsync(null, ct);
    }

    private async Task DecideAsync(string kind, CancellationToken ct)
    {
        if (_current is not { } group) return;
        var response = await Engine.CatalogAsync(Request("decideTakeGroup", decision: TakeReviewPolicy.Decision(group, kind)), ct);
        if (response.Status != "ok")
        {
            await UI(() => ShowToast(response.Message ?? "That didn't save. Try again.", null, PlainToastWindow), ct);
            return;
        }
        var reopen = TakeReviewPolicy.Decision(group, "reopen");
        var text = kind == "keepAll" ? TakeReviewPolicy.KeptAllText(group) : TakeReviewPolicy.SkippedText;
        await UI(() => ShowToast(text, c => ReopenAsync(reopen, c), DecisionUndoWindow), ct);
        await AfterResolveAsync(group.GroupKey, ct);
    }

    private async Task ReopenAsync(CatalogTakeDecision reopen, CancellationToken ct)
    {
        await Engine.CatalogAsync(Request("decideTakeGroup", decision: reopen), ct);
        await AfterResolveAsync(null, ct);
    }

    // ---- Pieces ---------------------------------------------------------------

    private Grid DashedNotice(TextBlock title, TextBlock subtitle)
    {
        var outline = new Microsoft.UI.Xaml.Shapes.Rectangle
        {
            RadiusX = 10,
            RadiusY = 10,
            Stroke = _dash,
            StrokeThickness = 1,
            StrokeDashArray = new DoubleCollection { 5, 4 },
        };
        title.TextAlignment = TextAlignment.Center;
        subtitle.TextAlignment = TextAlignment.Center;
        subtitle.FontSize = 13;
        subtitle.MaxWidth = 460;
        var stack = new StackPanel
        {
            Spacing = 8,
            Padding = new Thickness(24, 32, 24, 32),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
        };
        stack.Children.Add(title);
        stack.Children.Add(subtitle);
        var grid = new Grid { MinHeight = 220 };
        grid.Children.Add(outline);
        grid.Children.Add(stack);
        return grid;
    }

    private Border StatusPill(string text)
    {
        var label = new TextBlock { Text = text, FontSize = 10.5, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Foreground = _text3 };
        SetFont(label, "BodyFontFamily");
        return new Border
        {
            Padding = new Thickness(6, 2, 6, 2),
            CornerRadius = new CornerRadius(4),
            BorderBrush = _pillStroke,
            BorderThickness = new Thickness(1),
            Child = label,
        };
    }

    private static void SetFont(TextBlock text, string key)
    {
        try
        {
            if (Application.Current.Resources[key] is FontFamily family) text.FontFamily = family;
        }
        catch (Exception)
        {
            // Missing font resource: keep the default face.
        }
    }

    private static string Clock(double seconds)
    {
        var total = (int)Math.Round(seconds);
        return total >= 3600
            ? $"{total / 3600}:{total / 60 % 60:00}:{total % 60:00}"
            : $"{total / 60}:{total % 60:00}";
    }

    private sealed class ComparePane
    {
        public required Border Root { get; init; }
        public required TextBlock Title { get; init; }
        public required FontIcon Star { get; init; }
        public required Button KeepButton { get; init; }
        public required Grid Well { get; init; }
        public required Image Picture { get; init; }
        public required MediaPlayerElement Player { get; init; }
        public required Button Play { get; init; }
        public required TextBlock Name { get; init; }
        public required Border Raw { get; init; }
        public required TextBlock Score { get; init; }
        public required Grid Bar { get; init; }
        public required Border BarFill { get; init; }
        public required FileID.Theme.Controls.FlowPanel Chips { get; init; }
        public required TextBlock NoChips { get; init; }
        public CatalogTakeQuality? Quality { get; set; }
        public string? LoadedPath { get; set; }
        public MediaPlayer? MediaPlayer { get; set; }
    }

    private sealed class Tile
    {
        public required Grid Root { get; init; }
        public required Image Picture { get; init; }
        public required Border Badge { get; init; }
        public required TextBlock Number { get; init; }
        public required FontIcon Star { get; init; }
        public required Border Check { get; init; }
        public required Border Stroke { get; init; }
        public bool Focused { get; set; }
    }
}
