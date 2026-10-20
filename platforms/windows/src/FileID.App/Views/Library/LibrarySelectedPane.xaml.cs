using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Views.Library;

public sealed partial class LibrarySelectedPane : UserControl
{
    private const int PreviewDecodeDip = 268;

    private readonly Brush _text1;
    private readonly Brush _text2;
    private readonly Brush _text3;
    private readonly Brush _hairlineSoft;
    private readonly Brush _chipFill;
    private readonly FontFamily? _bodyFont;
    private FileTile? _tile;
    private int _revision;

    public LibrarySelectedPane()
    {
        InitializeComponent();
        _text1 = ThemeHelper.GetBrushSafe("Text1Brush");
        _text2 = ThemeHelper.GetBrushSafe("Text2Brush");
        _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
        _hairlineSoft = ThemeHelper.GetBrushSafe("HairlineSoftBrush");
        _chipFill = ThemeHelper.GetBrushSafe("SecondaryButtonFillBrush");
        try { _bodyFont = Application.Current.Resources["BodyFontFamily"] as FontFamily; }
        catch (Exception ex) { DebugLog.Warn("LibrarySelectedPane: BodyFontFamily unavailable: " + ex.Message); }
        Unloaded += (_, _) => _revision++;
    }

    internal ReadStore? Store { get; set; }

    internal FileTile? Tile => _tile;

    public event EventHandler? CloseRequested;
    public event EventHandler? OpenPreviewRequested;
    public event EventHandler? FindSimilarRequested;

    internal void Show(FileTile tile)
    {
        _tile = tile;
        var revision = ++_revision;
        NameText.Text = tile.FileName;
        var folder = System.IO.Path.GetDirectoryName(tile.Path) ?? string.Empty;
        FolderText.Text = folder;
        ToolTipService.SetToolTip(FolderText, folder);
        PreviewImage.Source = null;
        PreviewGlyph.Glyph = tile.Kind switch
        {
            "video" => "",
            "audio" => "",
            "pdf" => "",
            _ => "",
        };
        bool visual = tile.Kind is "image" or "video";
        SimilarButton.Visibility = visual ? Visibility.Visible : Visibility.Collapsed;
        SimilarColumn.Width = visual ? new GridLength(1, GridUnitType.Star) : new GridLength(0);

        RenderDetails(tile, null);
        RenderTags(tile.Tags);
        CaptionText.Visibility = Visibility.Collapsed;

        _ = DebugLog.SafeRunAsync("LibrarySelectedPane.Load", () => LoadAsync(tile, revision));
    }

    private async Task LoadAsync(FileTile tile, int revision)
    {
        var ui = DispatcherQueue;
        var store = Store;
        if (store is not null)
        {
            var details = await Task.Run(() => store.FileDetailsAsync(tile.Id, CancellationToken.None)).ConfigureAwait(false);
            ui.TryEnqueue(() =>
            {
                if (revision != _revision) return;
                RenderDetails(tile, details);
                var caption = details?.Caption;
                CaptionText.Text = caption ?? string.Empty;
                CaptionText.Visibility = string.IsNullOrWhiteSpace(caption) ? Visibility.Collapsed : Visibility.Visible;
            });
        }

        if (tile.Kind is not ("image" or "video" or "pdf" or "doc")) return;
        Windows.Storage.FileProperties.StorageItemThumbnail? thumb = null;
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(tile.Path);
            thumb = await file.GetThumbnailAsync(
                Windows.Storage.FileProperties.ThumbnailMode.SingleItem,
                PreviewDecodeDip,
                Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale);
            if (thumb is not { Size: > 0 }) return;
            var captured = thumb;
            thumb = null;
            if (!ui.TryEnqueue(async () =>
            {
                try
                {
                    if (revision != _revision) return;
                    var scale = XamlRoot?.RasterizationScale ?? 1;
                    var bitmap = new BitmapImage { DecodePixelWidth = (int)Math.Ceiling(PreviewDecodeDip * Math.Max(1, scale)) };
                    await bitmap.SetSourceAsync(captured);
                    if (revision != _revision) return;
                    PreviewImage.Source = bitmap;
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("LibrarySelectedPane preview failed: " + ex.Message);
                }
                finally
                {
                    try { captured.Dispose(); } catch { }
                }
            }))
            {
                captured.Dispose();
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn("LibrarySelectedPane thumbnail failed: " + ex.Message);
        }
        finally
        {
            try { thumb?.Dispose(); } catch { }
        }
    }

    private void RenderDetails(FileTile tile, FileDetails? details)
    {
        DetailRows.Children.Clear();
        var extension = System.IO.Path.GetExtension(tile.Path).TrimStart('.').ToUpperInvariant();
        var kind = tile.Kind.Length > 0 ? char.ToUpperInvariant(tile.Kind[0]) + tile.Kind[1..] : tile.Kind;
        AddRow("Kind", extension.Length > 0 ? $"{kind} · {extension}" : kind);
        double? captured = details?.CapturedAt ?? tile.ModifiedAt;
        if (captured is { } seconds)
        {
            var local = DateTimeOffset.FromUnixTimeMilliseconds((long)(seconds * 1000)).ToLocalTime();
            AddRow("Captured", $"{local:MMM d, yyyy} at {local:t}");
        }
        AddRow("Size", ByteFormat.File(tile.SizeBytes));
        if (!string.IsNullOrWhiteSpace(details?.CameraModel)) AddRow("Camera", details!.CameraModel!);
        if (details is { People.Count: > 0 }) AddRow("People", string.Join(", ", details.People));
    }

    private void AddRow(string key, string value)
    {
        var row = new Grid
        {
            ColumnSpacing = 10,
            Padding = new Thickness(0, 6, 0, 6),
            BorderBrush = _hairlineSoft,
            BorderThickness = new Thickness(0, 0, 0, 1),
        };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(64) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var keyText = new TextBlock { Text = key, FontSize = 12, Foreground = _text3 };
        var valueText = new TextBlock
        {
            Text = value,
            FontSize = 12.5,
            Foreground = _text1,
            TextWrapping = TextWrapping.Wrap,
            MaxLines = 2,
            TextTrimming = TextTrimming.CharacterEllipsis,
            IsTextSelectionEnabled = true,
        };
        if (_bodyFont is not null)
        {
            keyText.FontFamily = _bodyFont;
            valueText.FontFamily = _bodyFont;
        }
        Grid.SetColumn(valueText, 1);
        row.Children.Add(keyText);
        row.Children.Add(valueText);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(row, $"{key}: {value}");
        DetailRows.Children.Add(row);
    }

    private void RenderTags(IReadOnlyList<string> tags)
    {
        TagsHost.Children.Clear();
        foreach (var tag in tags.Take(12))
        {
            var text = new TextBlock
            {
                Text = tag,
                FontSize = 11.5,
                FontWeight = Microsoft.UI.Text.FontWeights.Medium,
                Foreground = _text2,
                TextTrimming = TextTrimming.CharacterEllipsis,
                MaxWidth = 244,
                VerticalAlignment = VerticalAlignment.Center,
            };
            if (_bodyFont is not null) text.FontFamily = _bodyFont;
            var chip = new Border
            {
                Height = 22,
                Padding = new Thickness(8, 0, 8, 0),
                CornerRadius = new CornerRadius(6),
                Background = _chipFill,
                Child = text,
            };
            ToolTipService.SetToolTip(chip, tag);
            TagsHost.Children.Add(chip);
        }
        TagsHost.Visibility = TagsHost.Children.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void OnCloseClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnCloseClicked), () => CloseRequested?.Invoke(this, EventArgs.Empty));

    private void OnOpenPreviewClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnOpenPreviewClicked), () => OpenPreviewRequested?.Invoke(this, EventArgs.Empty));

    private void OnPreviewDoubleTapped(object sender, DoubleTappedRoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnPreviewDoubleTapped), () => OpenPreviewRequested?.Invoke(this, EventArgs.Empty));

    private void OnSimilarClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnSimilarClicked), () => FindSimilarRequested?.Invoke(this, EventArgs.Empty));

    private void OnRevealClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnRevealClicked), () =>
        {
            if (_tile is { } tile) SafeOpen.Reveal(tile.Path);
        });

    private void OnOpenClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnOpenClicked), () =>
        {
            if (_tile is { } tile && !SafeOpen.TryOpenFile(tile.Path)) SafeOpen.Reveal(tile.Path);
        });
}
