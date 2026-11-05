using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.System;
using Windows.UI;

namespace FileID.Views.Workbenches;

/// <summary>Chapters lane of the Search &amp; Moments timeline (DESIGN_V3_PORT "Timeline"): chapter
/// spans in the accepted / draft / stale colours, a 1.5 px gold playhead and a ruler. Clicking the lane
/// or pressing the arrow keys seeks. The Examined and Speech lanes are left out because the catalog
/// reply for a file carries chapters only.</summary>
internal sealed class MomentTimeline : StackPanel
{
    private const double LabelWidth = 70;
    private const double LaneHeight = 16;
    private const double TickLabelWidth = 64;

    private readonly Action<double> _seek;
    private readonly Grid _lane;
    private readonly Canvas _spans = new() { Height = LaneHeight };
    private readonly Canvas _ruler = new() { Height = 20 };
    private readonly Rectangle _playheadLine;
    private readonly TranslateTransform _playheadOffset = new();
    private readonly ContentControl _laneHost;
    private readonly Brush _hairlineStrong = ThemeHelper.GetBrushSafe("HairlineStrongBrush");
    private readonly Brush _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
    private readonly Brush _acceptedFill = Solid(0x8C, 0xFF, 0xCC, 0x00);
    private readonly Brush _acceptedStroke = Solid(0xD9, 0xFF, 0xCC, 0x00);
    private readonly Brush _draftFill = Solid(0x52, 0xB1, 0x9B, 0xCE);
    private readonly Brush _draftStroke = Solid(0xD9, 0xB1, 0x9B, 0xCE);
    private readonly Brush _staleFill = Solid(0x52, 0xB9, 0xBE, 0xC4);
    private readonly Brush _staleStroke = Solid(0xD9, 0xB9, 0xBE, 0xC4);
    private IReadOnlyList<CatalogChapter> _chapters = Array.Empty<CatalogChapter>();
    private double _duration = 1;
    private double _playhead;
    private double _laidOutWidth = -1;
    private string _spoken = "";

    public MomentTimeline(Action<double> seek)
    {
        _seek = seek;
        Spacing = 6;
        _playheadLine = new Rectangle
        {
            Width = 1.5,
            Height = LaneHeight,
            Fill = ThemeHelper.GetBrushSafe("GoldBrush"),
            HorizontalAlignment = HorizontalAlignment.Left,
            RenderTransform = _playheadOffset,
            IsHitTestVisible = false,
        };
        _lane = new Grid { Height = LaneHeight, CornerRadius = new CornerRadius(4), Background = Solid(0x0B, 0xFF, 0xFF, 0xFF) };
        _lane.Children.Add(_spans);
        _lane.Children.Add(_playheadLine);
        _laneHost = new ContentControl
        {
            Content = _lane,
            IsTabStop = true,
            UseSystemFocusVisuals = true,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            VerticalContentAlignment = VerticalAlignment.Stretch,
        };
        AutomationProperties.SetName(_laneHost, "Chapters");
        ToolTipService.SetToolTip(_laneHost, "Click to play from that point. Left and right arrows move 5 seconds.");

        var caption = new TextBlock
        {
            Text = "Chapters",
            FontSize = 11.5,
            FontWeight = Microsoft.UI.Text.FontWeights.Medium,
            Foreground = _text3,
            VerticalAlignment = VerticalAlignment.Center,
        };
        SetFont(caption, "BodyFontFamily");
        AutomationProperties.SetAccessibilityView(caption, AccessibilityView.Raw);
        var laneRow = new Grid { ColumnSpacing = 10 };
        laneRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(LabelWidth) });
        laneRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(_laneHost, 1);
        laneRow.Children.Add(caption);
        laneRow.Children.Add(_laneHost);
        var rulerRow = new Grid { ColumnSpacing = 10 };
        rulerRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(LabelWidth) });
        rulerRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(_ruler, 1);
        rulerRow.Children.Add(_ruler);
        AutomationProperties.SetAccessibilityView(rulerRow, AccessibilityView.Raw);
        Children.Add(laneRow);
        Children.Add(rulerRow);

        _lane.SizeChanged += (_, _) => DebugLog.SafeRun("MomentTimeline.Sized", () => Layout(force: false));
        _lane.PointerPressed += (_, e) => DebugLog.SafeRun("MomentTimeline.Seek", () =>
        {
            var x = e.GetCurrentPoint(_lane).Position.X;
            _laneHost.Focus(FocusState.Pointer);
            _seek(MomentTimelinePolicy.SeekSeconds(x, _lane.ActualWidth, _duration));
            e.Handled = true;
        });
        _laneHost.KeyDown += (_, e) => DebugLog.SafeRun("MomentTimeline.Key", () =>
        {
            switch (e.Key)
            {
                case VirtualKey.Left:
                    _seek(Math.Max(0, _playhead - MomentTimelinePolicy.KeyboardStep));
                    break;
                case VirtualKey.Right:
                    _seek(Math.Min(_duration, _playhead + MomentTimelinePolicy.KeyboardStep));
                    break;
                default:
                    return;
            }
            e.Handled = true;
        });
        Layout(force: true);
    }

    public void SetChapters(IReadOnlyList<CatalogChapter> chapters, double duration)
    {
        _chapters = chapters;
        _duration = duration;
        Layout(force: true);
    }

    public void Update(double duration, double playhead)
    {
        _playhead = playhead;
        if (Math.Abs(duration - _duration) > 0.01)
        {
            _duration = duration;
            Layout(force: true);
            return;
        }
        MovePlayhead();
    }

    public void Reset()
    {
        _chapters = Array.Empty<CatalogChapter>();
        _duration = 1;
        _playhead = 0;
        Layout(force: true);
    }

    private void Layout(bool force)
    {
        var width = _lane.ActualWidth;
        if (!force && Math.Abs(width - _laidOutWidth) < 0.5) return;
        _laidOutWidth = width;
        _spans.Children.Clear();
        _ruler.Children.Clear();
        if (width > 0)
        {
            foreach (var chapter in _chapters)
            {
                var (x, spanWidth) = MomentTimelinePolicy.ChapterSpan(chapter, _duration, width);
                var state = MomentTimelinePolicy.State(chapter);
                var span = new Border
                {
                    Width = spanWidth,
                    Height = LaneHeight - 4,
                    CornerRadius = new CornerRadius(3),
                    BorderThickness = new Thickness(1),
                    Background = state switch
                    {
                        MomentTimelinePolicy.ChapterState.Accepted => _acceptedFill,
                        MomentTimelinePolicy.ChapterState.Stale => _staleFill,
                        _ => _draftFill,
                    },
                    BorderBrush = state switch
                    {
                        MomentTimelinePolicy.ChapterState.Accepted => _acceptedStroke,
                        MomentTimelinePolicy.ChapterState.Stale => _staleStroke,
                        _ => _draftStroke,
                    },
                };
                ToolTipService.SetToolTip(span, $"{HitPresentation.Clock(chapter.StartSeconds)} · {chapter.Title}");
                Canvas.SetLeft(span, x);
                Canvas.SetTop(span, 2);
                _spans.Children.Add(span);
            }
            LayoutRuler(width);
        }
        _lane.Clip = new RectangleGeometry { Rect = new Windows.Foundation.Rect(0, 0, Math.Max(0, width), LaneHeight) };
        MovePlayhead();
    }

    private void LayoutRuler(double width)
    {
        foreach (var (seconds, anchor) in MomentTimelinePolicy.Ticks(_duration))
        {
            var x = MomentTimelinePolicy.X(seconds, _duration, width);
            var tick = new Rectangle { Width = 1, Height = 5, Fill = _hairlineStrong };
            Canvas.SetLeft(tick, Math.Min(x, width - 1));
            _ruler.Children.Add(tick);
            var label = new TextBlock
            {
                Text = HitPresentation.Clock(seconds),
                FontSize = 10.5,
                Foreground = _text3,
                Width = TickLabelWidth,
                TextAlignment = anchor switch
                {
                    MomentTimelinePolicy.TickAnchor.Leading => TextAlignment.Left,
                    MomentTimelinePolicy.TickAnchor.Trailing => TextAlignment.Right,
                    _ => TextAlignment.Center,
                },
            };
            SetFont(label, "MonoFontFamily");
            Canvas.SetLeft(label, anchor switch
            {
                MomentTimelinePolicy.TickAnchor.Leading => x,
                MomentTimelinePolicy.TickAnchor.Trailing => x - TickLabelWidth,
                _ => x - TickLabelWidth / 2,
            });
            Canvas.SetTop(label, 7);
            _ruler.Children.Add(label);
        }
    }

    private void MovePlayhead()
    {
        var width = _lane.ActualWidth;
        _playheadLine.Visibility = width > 0 ? Visibility.Visible : Visibility.Collapsed;
        _playheadOffset.X = MomentTimelinePolicy.X(_playhead, _duration, width) - 0.75;
        var spoken = MomentTimelinePolicy.LaneSpoken(_chapters.Count, _playhead);
        if (spoken == _spoken) return;
        _spoken = spoken;
        AutomationProperties.SetHelpText(_laneHost, spoken);
    }

    private static SolidColorBrush Solid(byte a, byte r, byte g, byte b) => new(Color.FromArgb(a, r, g, b));

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
}
