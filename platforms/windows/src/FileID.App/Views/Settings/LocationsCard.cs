using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Settings;

/// <summary>Settings ▸ Locations (macOS LocationsCard): every folder or drive the catalog has
/// scanned, read with the engine's read-only listRoots. An unplugged drive reads OFFLINE and
/// keeps its catalog rows.</summary>
public sealed class LocationsCard : UserControl
{
    private readonly EngineClient _engine = EngineClient.Instance;
    private readonly StackPanel _rows = new() { Spacing = 10 };
    private readonly TextBlock _empty;
    private readonly TextBlock _footer;
    private readonly TextBlock _status;
    private readonly Button _refresh;
    private readonly Brush _text1 = ThemeHelper.GetBrushSafe("Text1Brush");
    private readonly Brush _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
    private readonly Brush _warning = ThemeHelper.GetBrushSafe("WarningBrush");
    private readonly Brush _warningStroke = new SolidColorBrush(Color.FromArgb(0x99, 0xE0, 0xA3, 0x40));
    private readonly Brush _divider = new SolidColorBrush(Color.FromArgb(0x1A, 0xFF, 0xFF, 0xFF));
    private CancellationTokenSource? _lifetime;
    private int _refreshing;

    public LocationsCard()
    {
        var title = new TextBlock { Text = "Locations", VerticalAlignment = VerticalAlignment.Center };
        if (ThemeHelper.GetStyleSafe("SubtitleTextBlockStyle") is { } subtitle) title.Style = subtitle;
        AutomationProperties.SetHeadingLevel(title, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level2);
        _refresh = new Button { Content = "Refresh", VerticalAlignment = VerticalAlignment.Center };
        AutomationProperties.SetName(_refresh, "Refresh locations");
        _refresh.Click += (_, _) => DebugLog.SafeRun("LocationsCard.Refresh", Refresh);
        var header = new Grid { ColumnSpacing = 12 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(_refresh, 1);
        header.Children.Add(title);
        header.Children.Add(_refresh);

        _empty = Caption(LocationPresentation.Empty);
        _footer = Caption(LocationPresentation.Footer);
        _footer.Visibility = Visibility.Collapsed;
        _status = Caption("");
        _status.Visibility = Visibility.Collapsed;

        var stack = new StackPanel { Spacing = 10 };
        stack.Children.Add(header);
        stack.Children.Add(_empty);
        stack.Children.Add(_rows);
        stack.Children.Add(_footer);
        stack.Children.Add(_status);
        Content = new Border
        {
            CornerRadius = new CornerRadius(12),
            Background = ThemeHelper.GetBrushSafe("CardBackgroundFillColorDefaultBrush"),
            BorderBrush = ThemeHelper.GetBrushSafe("CardStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(1),
            Padding = new Thickness(20),
            Child = stack,
        };
        Loaded += (_, _) => DebugLog.SafeRun("LocationsCard.Loaded", () =>
        {
            _lifetime?.Dispose();
            _lifetime = new CancellationTokenSource();
            Refresh();
        });
        Unloaded += (_, _) => DebugLog.SafeRun("LocationsCard.Unloaded", () =>
        {
            _lifetime?.Cancel();
            _lifetime?.Dispose();
            _lifetime = null;
        });
    }

    private void Refresh()
    {
        if (_lifetime is not { } lifetime || Interlocked.Exchange(ref _refreshing, 1) != 0) return;
        _refresh.IsEnabled = false;
        var ct = lifetime.Token;
        _ = DebugLog.SafeRunAsync("LocationsCard.RefreshAsync", async () =>
        {
            try
            {
                var list = await _engine.ListRootsAsync(ct);
                if (!ct.IsCancellationRequested) Render(list.Roots);
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested) { }
            catch (Exception ex)
            {
                DebugLog.Warn("LocationsCard: listRoots failed: " + ex.Message);
                if (!ct.IsCancellationRequested) ShowStatus("Locations couldn't be read right now. FileID may still be starting; press Refresh in a moment.");
            }
            finally
            {
                Interlocked.Exchange(ref _refreshing, 0);
                if (!ct.IsCancellationRequested) _refresh.IsEnabled = true;
            }
        });
    }

    private void Render(IReadOnlyList<LibraryRoot> roots)
    {
        _status.Visibility = Visibility.Collapsed;
        _rows.Children.Clear();
        var now = DateTimeOffset.Now;
        for (var i = 0; i < roots.Count; i++)
        {
            if (i > 0) _rows.Children.Add(new Border { Height = 1, Background = _divider });
            _rows.Children.Add(Row(roots[i], now));
        }
        _empty.Visibility = roots.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        _footer.Visibility = roots.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    private Grid Row(LibraryRoot root, DateTimeOffset now)
    {
        var icon = new FontIcon { Glyph = LocationPresentation.Glyph(root.Kind), FontSize = 16, Foreground = _text3, Width = 22, VerticalAlignment = VerticalAlignment.Center };
        AutomationProperties.SetAccessibilityView(icon, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
        var name = new TextBlock { Text = root.Display, FontSize = 13.5, FontWeight = Microsoft.UI.Text.FontWeights.Medium, Foreground = _text1, VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis };
        var nameRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        nameRow.Children.Add(name);
        if (!root.Online) nameRow.Children.Add(OfflinePill());
        var path = Caption(root.Path);
        path.TextWrapping = TextWrapping.NoWrap;
        path.TextTrimming = TextTrimming.CharacterEllipsis;
        ToolTipService.SetToolTip(path, root.Path);
        var text = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        text.Children.Add(nameRow);
        text.Children.Add(Caption(LocationPresentation.Summary(root, now)));
        text.Children.Add(path);
        var rescan = new Button { Content = "Rescan", IsEnabled = root.Online, VerticalAlignment = VerticalAlignment.Center };
        ToolTipService.SetToolTip(rescan, LocationPresentation.RescanTooltip(root));
        AutomationProperties.SetName(rescan, $"Rescan {root.Display}");
        rescan.Click += (_, _) => DebugLog.SafeRun("LocationsCard.Rescan", () => Rescan(root, rescan));
        var row = new Grid { ColumnSpacing = 12 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(text, 1);
        Grid.SetColumn(rescan, 2);
        row.Children.Add(icon);
        row.Children.Add(text);
        row.Children.Add(rescan);
        AutomationProperties.SetName(row, LocationPresentation.Spoken(root, now));
        return row;
    }

    private void Rescan(LibraryRoot root, Button button)
    {
        button.IsEnabled = false;
        _ = DebugLog.SafeRunAsync("LocationsCard.RescanAsync", async () =>
        {
            try
            {
                await _engine.StartScanAsync(root.Path, root.Display);
                ShowStatus($"Scanning {root.Display}. Progress shows in the sidebar.");
            }
            catch (Exception ex)
            {
                ShowStatus(ex.Message);
                button.IsEnabled = root.Online;
            }
        });
    }

    private void ShowStatus(string text)
    {
        _status.Text = text;
        _status.Visibility = Visibility.Visible;
    }

    private Border OfflinePill()
    {
        var label = new TextBlock { Text = "OFFLINE", FontSize = 10.5, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Foreground = _warning };
        return new Border
        {
            Padding = new Thickness(6, 2, 6, 2),
            CornerRadius = new CornerRadius(4),
            BorderBrush = _warningStroke,
            BorderThickness = new Thickness(1),
            VerticalAlignment = VerticalAlignment.Center,
            Child = label,
        };
    }

    private TextBlock Caption(string text)
    {
        var caption = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap, Foreground = _text3 };
        if (ThemeHelper.GetStyleSafe("CaptionTextBlockStyle") is { } style) caption.Style = style;
        caption.Foreground = _text3;
        return caption;
    }
}
