// SidebarTabList code-behind. Builds the area switcher and every page row
// once (stable containers), then only toggles visibility, text and brushes
// as AppViewModel / EngineClient change.

using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Hosting;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarTabList : UserControl
{
    private sealed record PageRow(SidebarTab Tab, Button Button, FontIcon Icon, TextBlock Label, TextBlock Badge);

    private readonly Dictionary<string, PageRow> _rows = new();
    private readonly Dictionary<string, (Button Button, TextBlock Label)> _areaButtons = new();
    private string _shownArea = "Browse";

    // Cached at ctor (UI thread): selection and badge syncs fire on every tab
    // switch and on the ~10 Hz scan progress stream.
    private readonly Brush _gold;
    private readonly Brush _ai;
    private readonly Brush _warning;
    private readonly Brush _onGold;
    private readonly Brush _text1;
    private readonly Brush _text2;
    private readonly Brush _text3;
    private readonly Brush _navSelected;
    private readonly Brush _transparent;
    private readonly FontFamily? _bodyFont;

    public SidebarTabList()
    {
        InitializeComponent();
        _gold = ThemeHelper.GetBrushSafe("GoldBrush");
        _ai = ThemeHelper.GetBrushSafe("AiBrush");
        _warning = ThemeHelper.GetBrushSafe("WarningBrush");
        _onGold = ThemeHelper.GetBrushSafe("OnGoldBrush");
        _text1 = ThemeHelper.GetBrushSafe("Text1Brush");
        _text2 = ThemeHelper.GetBrushSafe("Text2Brush");
        _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
        _navSelected = ThemeHelper.GetBrushSafe("NavSelectedBrush");
        _transparent = new SolidColorBrush(Microsoft.UI.Colors.Transparent);
        try { _bodyFont = Application.Current.Resources["BodyFontFamily"] as FontFamily; }
        catch (Exception ex) { DebugLog.Warn("SidebarTabList: BodyFontFamily unavailable: " + ex.Message); }

        _shownArea = AppViewModel.Instance.ActiveTab.Area ?? "Browse";
        BuildAreaButtons();
        BuildRows();
        ElementCompositionPreview.SetIsTranslationEnabled(AreaPill, true);
        AreaTrack.SizeChanged += (_, _) => PlacePill(animate: false);

        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
    }

    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.PropertyChanged -= OnAppViewModelChanged;
        AppViewModel.Instance.PropertyChanged += OnAppViewModelChanged;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        SyncSelection();
        SyncBadges();
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.PropertyChanged -= OnAppViewModelChanged;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void BuildAreaButtons()
    {
        for (int i = 0; i < SidebarTab.Areas.Count; i++)
        {
            var area = SidebarTab.Areas[i];
            var pages = SidebarTab.InArea(area);
            var first = pages.Count > 0 ? pages[0].ShortcutNumber : 0;
            var last = pages.Count > 0 ? pages[^1].ShortcutNumber : 0;
            var label = new TextBlock
            {
                Text = area,
                FontSize = 12.5,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
            };
            if (_bodyFont is not null) label.FontFamily = _bodyFont;
            var button = new Button
            {
                Style = ThemeHelper.GetStyleSafe("ShellButtonStyle")!,
                Content = label,
                Height = 26,
                MinHeight = 26,
                Padding = new Thickness(0),
                HorizontalAlignment = HorizontalAlignment.Stretch,
                Background = _transparent,
            };
            ToolTipService.SetToolTip(button, $"{area}: {string.Join(", ", pages.Select(p => p.Label))} (Ctrl+{first}–{last})");
            AutomationProperties.SetName(button, $"{area} pages");
            button.Click += (_, _) => DebugLog.SafeRun("SidebarTabList.AreaClick", () => ShowArea(area, navigate: true));
            Grid.SetColumn(button, i);
            AreaTrack.Children.Add(button);
            _areaButtons[area] = (button, label);
        }
    }

    private void BuildRows()
    {
        foreach (var tab in SidebarTab.Order)
        {
            var icon = new FontIcon
            {
                FontFamily = new FontFamily("Segoe Fluent Icons"),
                Glyph = tab.IconGlyph,
                FontSize = 13,
                Width = 18,
                VerticalAlignment = VerticalAlignment.Center,
            };
            var label = new TextBlock
            {
                Text = tab.Label,
                FontSize = 13.5,
                TextTrimming = TextTrimming.CharacterEllipsis,
                VerticalAlignment = VerticalAlignment.Center,
            };
            var badge = new TextBlock
            {
                FontSize = 11.5,
                FontWeight = Microsoft.UI.Text.FontWeights.Medium,
                VerticalAlignment = VerticalAlignment.Center,
                Visibility = Visibility.Collapsed,
            };
            if (_bodyFont is not null)
            {
                label.FontFamily = _bodyFont;
                badge.FontFamily = _bodyFont;
            }

            var content = new Grid { ColumnSpacing = 10 };
            content.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            content.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            content.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            Grid.SetColumn(label, 1);
            Grid.SetColumn(badge, 2);
            content.Children.Add(icon);
            content.Children.Add(label);
            content.Children.Add(badge);

            var button = new Button
            {
                Style = ThemeHelper.GetStyleSafe("ShellNavRowStyle")!,
                Content = content,
                Background = _transparent,
                Tag = tab.Id,
            };
            ToolTipService.SetToolTip(button, $"{tab.Summary} (Ctrl+{tab.ShortcutNumber})");
            AutomationProperties.SetName(button, $"{tab.Label} page");
            AutomationProperties.SetHelpText(button, tab.Summary);
            var target = tab;
            button.Click += (_, _) => DebugLog.SafeRun("SidebarTabList.RowClick", () => AppViewModel.Instance.ActiveTab = target);

            _rows[tab.Id] = new PageRow(tab, button, icon, label, badge);
            ButtonsHost.Children.Add(button);
        }
    }

    private void OnAppViewModelChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnAppViewModelChanged), () => OnAppViewModelChangedCore(sender, e));

    private void OnAppViewModelChangedCore(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(AppViewModel.ActiveTab) or nameof(AppViewModel.HasFolder))
        {
            DispatcherQueue.TryEnqueue(SyncSelection);
        }
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("SidebarTabList.OnEngineChanged", () =>
        {
            if (e.PropertyName is nameof(EngineClient.LastProgress)
                              or nameof(EngineClient.Phase)
                              or nameof(EngineClient.LastFaceClustering)
                              or nameof(EngineClient.DeepAnalyzeProgress)
                              or nameof(EngineClient.DeepAnalyzeComplete)
                              or nameof(EngineClient.QueueState)
                              or nameof(EngineClient.LastRestructurePlan))
            {
                DebugLog.Debug($"[ENGINE-SUB:SidebarTabList] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(SyncBadges);
            }
        });

    private void ShowArea(string area, bool navigate)
    {
        var changed = _shownArea != area;
        _shownArea = area;
        if (navigate)
        {
            AppViewModel.Instance.ActiveTab = AppViewModel.Instance.RememberedTab(area);
        }
        if (changed)
        {
            SyncRowsForArea(animate: true);
        }
    }

    private void SyncSelection()
    {
        var vm = AppViewModel.Instance;
        if (vm.ActiveTab.Area is { } area && area != _shownArea)
        {
            _shownArea = area;
            SyncRowsForArea(animate: true);
        }
        else
        {
            SyncRowsForArea(animate: false);
        }

        foreach (var row in _rows.Values)
        {
            bool selected = vm.ActiveTab.Id == row.Tab.Id;
            row.Button.Background = selected ? _navSelected : _transparent;
            row.Icon.Foreground = selected ? _gold : _text2;
            row.Label.Foreground = selected ? _gold : _text1;
            row.Label.FontWeight = selected ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal;
            AutomationProperties.SetItemStatus(row.Button, selected ? "Selected" : string.Empty);

            // Pages that read the library stay disabled until a folder is picked
            // (macOS "Pick a folder to start").
            bool enabled = vm.HasFolder || !row.Tab.RequiresFolder;
            row.Button.IsEnabled = enabled;
        }
    }

    private void SyncRowsForArea(bool animate)
    {
        foreach (var row in _rows.Values)
        {
            row.Button.Visibility = row.Tab.Area == _shownArea ? Visibility.Visible : Visibility.Collapsed;
        }
        foreach (var (area, entry) in _areaButtons)
        {
            bool on = area == _shownArea;
            entry.Label.Foreground = on ? _onGold : _text2;
            entry.Label.FontWeight = on ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Medium;
            AutomationProperties.SetItemStatus(entry.Button, on ? "Selected" : string.Empty);
        }
        PlacePill(animate);
        if (animate && !ReducedMotion.Instance.IsReduced)
        {
            ElementCompositionPreview.GetElementVisual(ButtonsHost).Opacity = 0.35f;
            SpringEasing.AnimateOpacity(ButtonsHost, 1f, 0.38, 0.8);
        }
    }

    private void PlacePill(bool animate)
    {
        var index = 0;
        for (int i = 0; i < SidebarTab.Areas.Count; i++)
        {
            if (SidebarTab.Areas[i] == _shownArea) index = i;
        }
        var width = AreaTrack.ActualWidth / SidebarTab.Areas.Count;
        if (width <= 0) return;
        var x = (float)(index * width);
        if (animate && !ReducedMotion.Instance.IsReduced)
        {
            SpringEasing.AnimateScalar(AreaPill, "Translation.X", x, 0.38, 0.8);
        }
        else
        {
            var visual = ElementCompositionPreview.GetElementVisual(AreaPill);
            visual.StopAnimation("Translation.X");
            visual.Properties.InsertVector3("Translation", new System.Numerics.Vector3(x, 0, 0));
        }
    }

    private void SyncBadges()
    {
        var engine = EngineClient.Instance;
        var progress = engine.LastProgress;
        var phase = engine.Phase ?? progress?.Phase;

        string? library = null;
        Brush libraryColor = _text3;
        if (phase is ScanPhase.Discovering or ScanPhase.Tagging)
        {
            var total = progress is null ? 0UL : Math.Max(progress.Total, progress.Discovered);
            library = progress is not null && total > 0
                ? $"{(int)Math.Round(100.0 * progress.Processed / total)}%"
                : "…";
            libraryColor = _gold;
        }
        else if (engine.LastScanProcessedFiles > 0)
        {
            library = Compact(engine.LastScanProcessedFiles);
        }
        SetBadge("library", library, libraryColor);

        if (engine.QueueState?.Running?.Category == JobCategory.FaceCluster)
        {
            SetBadge("people", "…", _ai);
        }
        else if (engine.LastFaceClustering is { PersonCount: > 0u } faces)
        {
            SetBadge("people", Compact(faces.PersonCount), _text3);
        }
        else if (engine.LastScanProcessedFiles > 0 && ModelInstallerService.Instance.Arcface.Status != ModelInstallStatus.Installed)
        {
            SetBadge("people", "Setup", _warning);
        }
        else
        {
            SetBadge("people", null, _text3);
        }

        var deep = engine.DeepAnalyzeProgress;
        SetBadge("deepanalyze",
            deep is { Total: > 0UL } ? $"{(int)Math.Round(100.0 * deep.Processed / deep.Total)}%" : null,
            _ai);

        SetBadge("restructure", engine.LastRestructurePlan is not null ? "Ready" : null, _gold);
    }

    private void SetBadge(string id, string? text, Brush color)
    {
        if (!_rows.TryGetValue(id, out var row)) return;
        if (string.IsNullOrEmpty(text))
        {
            row.Badge.Visibility = Visibility.Collapsed;
            return;
        }
        if (row.Badge.Text != text) row.Badge.Text = text;
        row.Badge.Foreground = color;
        row.Badge.Visibility = Visibility.Visible;
    }

    private static string Compact(ulong n) =>
        n >= 10_000 ? $"{n / 1000.0:F0}k" : n.ToString("N0");
}
