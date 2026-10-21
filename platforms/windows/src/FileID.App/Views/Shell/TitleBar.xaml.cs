using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views.Shell;

public sealed partial class TitleBar : UserControl
{
    private sealed record SearchItem(string Label, string Hint, Brush Color, Action Run);

    private readonly List<SearchItem> _suggestions = new();

    private readonly Brush _gold;
    private readonly Brush _ai;
    private readonly Brush _info;
    private readonly Brush _delight;
    private readonly Brush _text1;
    private readonly Brush _text2;
    private readonly Brush _text3;
    private readonly Brush _goldBar;
    private readonly Brush _aiBar;
    private readonly Brush _infoBar;
    private readonly Brush _doneBar;
    private readonly Brush _pausedBar;
    private readonly Brush _controlFill;
    private readonly Brush _controlBorder;
    private readonly Brush _chipSelected;
    private readonly FontFamily? _bodyFont;

    public TitleBar()
    {
        InitializeComponent();
        _gold = ThemeHelper.GetBrushSafe("GoldBrush");
        _ai = ThemeHelper.GetBrushSafe("AiBrush");
        _info = ThemeHelper.GetBrushSafe("InfoBrush");
        _delight = ThemeHelper.GetBrushSafe("DelightBrush");
        _text1 = ThemeHelper.GetBrushSafe("Text1Brush");
        _text2 = ThemeHelper.GetBrushSafe("Text2Brush");
        _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
        _goldBar = ThemeHelper.GetBrushSafe("GoldProgressBrush");
        _aiBar = ThemeHelper.GetBrushSafe("AiProgressBrush");
        _infoBar = ThemeHelper.GetBrushSafe("InfoProgressBrush");
        _doneBar = ThemeHelper.GetBrushSafe("DoneProgressBrush");
        _pausedBar = ThemeHelper.GetBrushSafe("PausedBrush");
        _controlFill = ThemeHelper.GetBrushSafe("ShellControlFillStrongBrush");
        _controlBorder = ThemeHelper.GetBrushSafe("ShellControlBorderBrush");
        _chipSelected = ThemeHelper.GetBrushSafe("ShellChipSelectedBrush");
        try { _bodyFont = Application.Current.Resources["BodyFontFamily"] as FontFamily; }
        catch (Exception ex) { DebugLog.Warn("TitleBar: BodyFontFamily unavailable: " + ex.Message); }

        InteractiveElements = new FrameworkElement[]
        {
            SidebarToggleButton, FolderButton, SearchField, ActivityButton, ChatButton, SettingsButton,
        };
        foreach (var element in InteractiveElements)
        {
            element.SizeChanged += OnInteractiveSizeChanged;
        }
        Root.SizeChanged += OnInteractiveSizeChanged;
        FolderPanel.DismissRequested += OnFolderPanelDismissRequested;

        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
    }

    /// <summary>Controls that must receive pointer input inside the
    /// non-client title-bar region.</summary>
    internal IReadOnlyList<FrameworkElement> InteractiveElements { get; }

    /// <summary>Raised when an interactive control may have moved or resized.</summary>
    public event EventHandler? InteractiveLayoutChanged;

    public void SetCaptionInset(double width) => CaptionColumn.Width = new GridLength(Math.Max(0, width));

    public void FocusSearch() => SearchBox.Focus(FocusState.Keyboard);

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.PropertyChanged -= OnAppChanged;
        AppViewModel.Instance.PropertyChanged += OnAppChanged;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        SyncSidebarToggle();
        SyncFolder();
        SyncPageButtons();
        SyncActivity();
        InteractiveLayoutChanged?.Invoke(this, EventArgs.Empty);
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.PropertyChanged -= OnAppChanged;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void OnInteractiveSizeChanged(object sender, SizeChangedEventArgs e)
        => InteractiveLayoutChanged?.Invoke(this, EventArgs.Empty);

    private void OnFolderPanelDismissRequested(object? sender, EventArgs e)
        => DebugLog.SafeRun("TitleBar.OnFolderPanelDismissRequested", () => FolderFlyout.Hide());

    private void OnAppChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("TitleBar.OnAppChanged", () =>
        {
            switch (e.PropertyName)
            {
                case nameof(AppViewModel.SidebarVisible):
                    DispatcherQueue.TryEnqueue(SyncSidebarToggle);
                    break;
                case nameof(AppViewModel.FolderPath):
                case nameof(AppViewModel.FolderDisplay):
                case nameof(AppViewModel.HasFolder):
                    DispatcherQueue.TryEnqueue(SyncFolder);
                    break;
                case nameof(AppViewModel.ActiveTab):
                    DispatcherQueue.TryEnqueue(SyncPageButtons);
                    break;
            }
        });

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("TitleBar.OnEngineChanged", () =>
        {
            if (e.PropertyName is nameof(EngineClient.LastProgress)
                              or nameof(EngineClient.Phase)
                              or nameof(EngineClient.IsPaused)
                              or nameof(EngineClient.DeepAnalyzeProgress)
                              or nameof(EngineClient.DeepAnalyzeStarting)
                              or nameof(EngineClient.DeepAnalyzeComplete)
                              or nameof(EngineClient.QueueState))
            {
                DebugLog.Debug($"[ENGINE-SUB:TitleBar] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(SyncActivity);
            }
        });

    private void OnSidebarToggleClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnSidebarToggleClicked), () => AppViewModel.Instance.ToggleSidebar());

    private void OnChatClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnChatClicked), () => AppViewModel.Instance.ActiveTab = SidebarTab.Chat);

    private void OnSettingsClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnSettingsClicked), () => AppViewModel.Instance.ActiveTab = SidebarTab.Settings);

    private void SyncSidebarToggle()
    {
        var visible = AppViewModel.Instance.SidebarVisible;
        var label = visible ? "Hide sidebar" : "Show sidebar";
        SidebarToggleGlyph.Foreground = visible ? _text2 : _gold;
        ToolTipService.SetToolTip(SidebarToggleButton, $"{label} (Ctrl+Shift+S)");
        AutomationProperties.SetName(SidebarToggleButton, label);
    }

    private void SyncFolder()
    {
        var vm = AppViewModel.Instance;
        FolderNameText.Text = vm.HasFolder ? vm.FolderDisplay ?? vm.FolderPath ?? string.Empty : "Choose a folder";
        FolderNameText.Foreground = vm.HasFolder ? _text1 : _text3;
        AutomationProperties.SetName(FolderButton, vm.HasFolder ? $"Folder {FolderNameText.Text}" : "Choose a folder");
    }

    private void SyncPageButtons()
    {
        var id = AppViewModel.Instance.ActiveTab.Id;
        StylePageButton(ChatButton, ChatGlyph, id == SidebarTab.Chat.Id);
        StylePageButton(SettingsButton, SettingsGlyph, id == SidebarTab.Settings.Id);
    }

    private void StylePageButton(Button button, FontIcon glyph, bool selected)
    {
        button.Background = selected ? _chipSelected : _controlFill;
        button.BorderBrush = selected ? _gold : _controlBorder;
        glyph.Foreground = selected ? _gold : _text2;
    }

    // ─── Activity ────────────────────────────────────────────────────────

    private void SyncActivity()
    {
        var engine = EngineClient.Instance;
        var progress = engine.LastProgress;
        var phase = engine.Phase ?? progress?.Phase;
        var scanning = phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan;
        var paused = scanning && engine.IsPaused;
        var deep = engine.DeepAnalyzeProgress;
        var deepRunning = deep is not null || engine.DeepAnalyzeStarting is not null;
        var faces = engine.QueueState?.Running?.Category == JobCategory.FaceCluster;

        string label;
        Brush color;
        Brush bar;
        double? fraction;
        if (scanning)
        {
            label = paused ? "Paused" : "Scanning";
            color = paused ? _text3 : _gold;
            bar = paused ? _pausedBar : _goldBar;
            fraction = ScanFraction(progress);
        }
        else if (deepRunning)
        {
            label = "Describing";
            color = _ai;
            bar = _aiBar;
            fraction = DeepFraction(deep);
        }
        else if (faces)
        {
            label = "Faces";
            color = _ai;
            bar = _aiBar;
            fraction = null;
        }
        else
        {
            label = "Idle";
            color = _info;
            bar = _infoBar;
            fraction = 1;
        }

        var idle = label == "Idle";
        ActivityDot.Fill = color;
        ActivityLabel.Text = label;
        ActivityBar.Foreground = bar;
        SetBar(ActivityBar, fraction);
        ActivityPercent.Visibility = !idle && fraction is not null ? Visibility.Visible : Visibility.Collapsed;
        ActivityPercent.Text = fraction is { } f ? Percent(f) : string.Empty;
        ActivityPercent.Foreground = color;
        AutomationProperties.SetName(ActivityButton, idle ? "Activity, idle" : $"Activity, {label} {ActivityPercent.Text}");

        SyncScanRow(engine, progress, phase, scanning, paused);
        SyncDeepRow(engine, deep, deepRunning);
        FacesRow.Visibility = faces ? Visibility.Visible : Visibility.Collapsed;
        FacesRowBar.IsIndeterminate = faces;

        var running = (scanning && !paused ? 1 : 0) + (deepRunning ? 1 : 0) + (faces ? 1 : 0);
        ActivitySummary.Text = running > 0 ? $"{running} running" : "Nothing running";
        ActivityEmpty.Visibility = ScanRow.Visibility == Visibility.Collapsed
                                   && DeepRow.Visibility == Visibility.Collapsed
                                   && !faces
            ? Visibility.Visible
            : Visibility.Collapsed;
    }

    private void SyncScanRow(EngineClient engine, ScanProgress? progress, ScanPhase? phase, bool scanning, bool paused)
    {
        if (progress is null || phase is null or ScanPhase.Idle)
        {
            ScanRow.Visibility = Visibility.Collapsed;
            return;
        }

        var completed = phase == ScanPhase.Completed;
        double? fraction = completed ? 1.0 : ScanFraction(progress);
        if (fraction is null && !scanning) fraction = 0;
        var total = Math.Max(progress.Total, progress.Discovered);
        ScanRow.Visibility = Visibility.Visible;
        ScanRowSub.Text = scanning
            ? $"{progress.Processed:N0} of {total:N0} files"
            : $"{progress.Processed:N0} files";
        ScanRowState.Text = completed
            ? "DONE"
            : scanning
                ? (paused ? "PAUSED " : string.Empty) + (fraction is { } f ? Percent(f) : "…")
                : PhaseLabel(phase);
        ScanRowState.Foreground = completed ? _delight : paused || !scanning ? _text3 : _gold;
        ScanRowBar.Foreground = completed ? _doneBar : paused || !scanning ? _pausedBar : _goldBar;
        SetBar(ScanRowBar, fraction);
        ScanRowAction.Visibility = scanning ? Visibility.Visible : Visibility.Collapsed;
        ScanRowAction.Content = engine.IsPaused ? "Resume" : "Pause";
    }

    private void SyncDeepRow(EngineClient engine, DeepAnalyzeProgress? deep, bool deepRunning)
    {
        if (deepRunning)
        {
            var fraction = DeepFraction(deep);
            DeepRow.Visibility = Visibility.Visible;
            DeepRowSub.Text = deep is not null
                ? $"{deep.Processed:N0} of {deep.Total:N0} files"
                : engine.DeepAnalyzeStarting?.Message ?? "Starting";
            DeepRowState.Text = fraction is { } f ? Percent(f) : "STARTING";
            DeepRowState.Foreground = _ai;
            DeepRowBar.Foreground = _aiBar;
            SetBar(DeepRowBar, fraction);
            DeepRowAction.Visibility = Visibility.Visible;
            return;
        }

        if (engine.DeepAnalyzeComplete is { Processed: > 0UL } done)
        {
            DeepRow.Visibility = Visibility.Visible;
            DeepRowSub.Text = $"{done.Processed:N0} files";
            DeepRowState.Text = done.Cancelled ? "STOPPED" : "DONE";
            DeepRowState.Foreground = done.Cancelled ? _text3 : _delight;
            DeepRowBar.Foreground = done.Cancelled ? _pausedBar : _doneBar;
            SetBar(DeepRowBar, 1);
            DeepRowAction.Visibility = Visibility.Collapsed;
            return;
        }

        DeepRow.Visibility = Visibility.Collapsed;
    }

    private static string PhaseLabel(ScanPhase? phase) => phase switch
    {
        ScanPhase.Cancelled => "STOPPED",
        ScanPhase.Failed => "FAILED",
        _ => phase?.ToString().ToUpperInvariant() ?? string.Empty,
    };

    private static double? ScanFraction(ScanProgress? progress) =>
        progress is { Total: > 0UL } p ? Math.Clamp((double)p.Processed / p.Total, 0, 1) : null;

    private static double? DeepFraction(DeepAnalyzeProgress? progress) =>
        progress is { Total: > 0UL } p ? Math.Clamp((double)p.Processed / p.Total, 0, 1) : null;

    private static string Percent(double fraction) => $"{(int)Math.Round(fraction * 100)}%";

    private static void SetBar(ProgressBar bar, double? fraction)
    {
        if (fraction is { } f)
        {
            bar.IsIndeterminate = false;
            bar.Maximum = 1000;
            bar.Value = Math.Clamp(f, 0, 1) * 1000;
        }
        else
        {
            bar.IsIndeterminate = true;
        }
    }

    private async void OnScanRowActionClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            if (EngineClient.Instance.IsPaused)
            {
                await EngineClient.Instance.ResumeScanAsync();
            }
            else
            {
                await EngineClient.Instance.PauseScanAsync();
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn("TitleBar pause/resume failed: " + ex.Message);
        }
    }

    private async void OnDeepRowActionClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            await EngineClient.Instance.DeepAnalyzeCancelAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("TitleBar Deep Analyze cancel failed: " + ex.Message);
        }
    }

    private void OnOpenLibraryClicked(object sender, RoutedEventArgs e) => OpenFromActivity(SidebarTab.Library);

    private void OnOpenDeepAnalyzeClicked(object sender, RoutedEventArgs e) => OpenFromActivity(SidebarTab.DeepAnalyze);

    private void OnOpenPeopleClicked(object sender, RoutedEventArgs e) => OpenFromActivity(SidebarTab.People);

    private void OpenFromActivity(SidebarTab tab)
        => DebugLog.SafeRun(nameof(OpenFromActivity), () =>
        {
            ActivityFlyout.Hide();
            AppViewModel.Instance.ActiveTab = tab;
        });

    // ─── Search or ask ───────────────────────────────────────────────────

    private void OnSearchGotFocus(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnSearchGotFocus), () =>
        {
            RefreshSuggestions();
            SearchBox.IsSuggestionListOpen = _suggestions.Count > 0;
        });

    private void OnSearchTextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
        => DebugLog.SafeRun(nameof(OnSearchTextChanged), () =>
        {
            if (args.Reason != AutoSuggestionBoxTextChangeReason.UserInput) return;
            RefreshSuggestions();
            sender.IsSuggestionListOpen = _suggestions.Count > 0;
        });

    private void OnSearchQuerySubmitted(AutoSuggestBox sender, AutoSuggestBoxQuerySubmittedEventArgs args)
        => DebugLog.SafeRun(nameof(OnSearchQuerySubmitted), () =>
        {
            if (_suggestions.Count == 0) RefreshSuggestions();
            var item = (args.ChosenSuggestion as FrameworkElement)?.Tag as SearchItem
                ?? (_suggestions.Count > 0 ? _suggestions[0] : null);
            if (item is null) return;
            sender.IsSuggestionListOpen = false;
            sender.Text = string.Empty;
            _suggestions.Clear();
            item.Run();
        });

    private void RefreshSuggestions()
    {
        var query = (SearchBox.Text ?? string.Empty).Trim();
        _suggestions.Clear();
        if (query.Length > 0)
        {
            _suggestions.Add(new SearchItem($"“{query}”", "Ask Chat", _ai,
                () => AppViewModel.Instance.RequestChatDraft(query)));
            _suggestions.Add(new SearchItem($"Find “{query}” in the library", "Library", _text1,
                () => AppViewModel.Instance.RequestLibraryQuery(query)));
        }

        var limit = query.Length == 0 ? 10 : 4;
        foreach (var page in SidebarTab.All)
        {
            if (limit == 0) break;
            if (query.Length > 0
                && !page.Label.Contains(query, StringComparison.OrdinalIgnoreCase)
                && !page.Id.Contains(query, StringComparison.OrdinalIgnoreCase)
                && !page.Summary.Contains(query, StringComparison.OrdinalIgnoreCase))
            {
                continue;
            }
            var target = page;
            _suggestions.Add(new SearchItem(page.Label, PageHint(page), _text1,
                () => AppViewModel.Instance.ActiveTab = target));
            limit--;
        }

        var rows = new List<object>(_suggestions.Count);
        foreach (var item in _suggestions)
        {
            rows.Add(BuildSuggestionRow(item));
        }
        SearchBox.ItemsSource = rows;
    }

    private static string PageHint(SidebarTab page)
    {
        if (page.Id == SidebarTab.Settings.Id) return "FileID · Ctrl+,";
        var area = page.Area ?? "FileID";
        return page.ShortcutNumber > 0 ? $"{area} · Ctrl+{page.ShortcutNumber}" : area;
    }

    private Grid BuildSuggestionRow(SearchItem item)
    {
        var row = new Grid { Tag = item, MinHeight = 28, ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var label = new TextBlock
        {
            Text = item.Label,
            FontSize = 14,
            Foreground = item.Color,
            TextTrimming = TextTrimming.CharacterEllipsis,
            VerticalAlignment = VerticalAlignment.Center,
        };
        var hint = new TextBlock
        {
            Text = item.Hint,
            FontSize = 11.5,
            Foreground = _text3,
            VerticalAlignment = VerticalAlignment.Center,
        };
        if (_bodyFont is not null)
        {
            label.FontFamily = _bodyFont;
            hint.FontFamily = _bodyFont;
        }
        Grid.SetColumn(hint, 1);
        row.Children.Add(label);
        row.Children.Add(hint);
        AutomationProperties.SetName(row, $"{item.Label}, {item.Hint}");
        return row;
    }
}
