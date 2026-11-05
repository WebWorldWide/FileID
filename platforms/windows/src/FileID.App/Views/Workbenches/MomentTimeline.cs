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

/// <summary>Search &amp; Moments timeline (DESIGN_V3_PORT "Timeline", macOS MomentTimeline): Examined,
/// Chapters and Speech lanes over one ruler, with a 1.5 px gold playhead in every lane. Clicking a
/// lane or pressing the arrow keys on it seeks.</summary>
internal sealed class MomentTimeline : StackPanel
{
    private const double LabelWidth = 70;
    private const double LaneHeight = 16;
    private const double TickLabelWidth = 64;

    private readonly Action<double> _seek;
    private readonly Lane _examinedLane;
    private readonly Lane _chaptersLane;
    private readonly Lane _speechLane;
    private readonly Lane[] _lanes;
    private readonly Canvas _ruler = new() { Height = 20 };
    private readonly Brush _hairlineStrong = ThemeHelper.GetBrushSafe("HairlineStrongBrush");
    private readonly Brush _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
    private readonly Brush _laneTrack = Solid(0x0B, 0xFF, 0xFF, 0xFF);
    private readonly Brush _playheadFill = ThemeHelper.GetBrushSafe("GoldBrush");
    private readonly Brush _examinedTick = Solid(0xE6, 0xB1, 0x9B, 0xCE);
    private readonly Brush _examinedSpan = Solid(0x73, 0xB1, 0x9B, 0xCE);
    private readonly Brush _speechSpan = Solid(0x99, 0xA0, 0xE2, 0xEA);
    private readonly Brush _acceptedFill = Solid(0x8C, 0xFF, 0xCC, 0x00);
    private readonly Brush _acceptedStroke = Solid(0xD9, 0xFF, 0xCC, 0x00);
    private readonly Brush _draftFill = Solid(0x52, 0xB1, 0x9B, 0xCE);
    private readonly Brush _draftStroke = Solid(0xD9, 0xB1, 0x9B, 0xCE);
    private readonly Brush _staleFill = Solid(0x52, 0xB9, 0xBE, 0xC4);
    private readonly Brush _staleStroke = Solid(0xD9, 0xB9, 0xBE, 0xC4);
    private IReadOnlyList<CatalogChapter> _chapters = Array.Empty<CatalogChapter>();
    private IReadOnlyList<(double Start, double End)> _examined = Array.Empty<(double, double)>();
    private IReadOnlyList<(double Start, double End)> _speech = Array.Empty<(double, double)>();
    private double _duration = 1;
    private double _playhead;
    private double _laidOutWidth = -1;

    public MomentTimeline(Action<double> seek)
    {
        _seek = seek;
        Spacing = 6;
        _examinedLane = AddLane("Examined");
        _chaptersLane = AddLane("Chapters");
        _speechLane = AddLane("Speech");
        _lanes = new[] { _examinedLane, _chaptersLane, _speechLane };
        var rulerRow = new Grid { ColumnSpacing = 10 };
        rulerRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(LabelWidth) });
        rulerRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(_ruler, 1);
        rulerRow.Children.Add(_ruler);
        AutomationProperties.SetAccessibilityView(rulerRow, AccessibilityView.Raw);
        Children.Add(rulerRow);
        Layout(force: true);
    }

    public void SetChapters(IReadOnlyList<CatalogChapter> chapters, double duration)
    {
        _chapters = chapters;
        _duration = duration;
        Layout(force: true);
    }

    public void SetTracks(IReadOnlyList<(double Start, double End)> examined, IReadOnlyList<(double Start, double End)> speech, double duration)
    {
        _examined = examined;
        _speech = speech;
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
        MovePlayheads();
    }

    public void Reset()
    {
        _chapters = Array.Empty<CatalogChapter>();
        _examined = Array.Empty<(double, double)>();
        _speech = Array.Empty<(double, double)>();
        _duration = 1;
        _playhead = 0;
        Layout(force: true);
    }

    private Lane AddLane(string name)
    {
        var marks = new Canvas { Height = LaneHeight };
        var offset = new TranslateTransform();
        var playhead = new Rectangle
        {
            Width = 1.5,
            Height = LaneHeight,
            Fill = _playheadFill,
            HorizontalAlignment = HorizontalAlignment.Left,
            RenderTransform = offset,
            IsHitTestVisible = false,
        };
        var track = new Grid { Height = LaneHeight, CornerRadius = new CornerRadius(4), Background = _laneTrack };
        track.Children.Add(marks);
        track.Children.Add(playhead);
        var host = new ContentControl
        {
            Content = track,
            IsTabStop = true,
            UseSystemFocusVisuals = true,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
            VerticalContentAlignment = VerticalAlignment.Stretch,
        };
        AutomationProperties.SetName(host, name);
        ToolTipService.SetToolTip(host, "Click to play from that point. Left and right arrows move 5 seconds.");
        var caption = new TextBlock
        {
            Text = name,
            FontSize = 11.5,
            FontWeight = Microsoft.UI.Text.FontWeights.Medium,
            Foreground = _text3,
            VerticalAlignment = VerticalAlignment.Center,
        };
        SetFont(caption, "BodyFontFamily");
        AutomationProperties.SetAccessibilityView(caption, AccessibilityView.Raw);
        var row = new Grid { ColumnSpacing = 10 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(LabelWidth) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(host, 1);
        row.Children.Add(caption);
        row.Children.Add(host);
        Children.Add(row);

        track.SizeChanged += (_, _) => DebugLog.SafeRun("MomentTimeline.Sized", () => Layout(force: false));
        track.PointerPressed += (_, e) => DebugLog.SafeRun("MomentTimeline.Seek", () =>
        {
            var x = e.GetCurrentPoint(track).Position.X;
            host.Focus(FocusState.Pointer);
            _seek(MomentTimelinePolicy.SeekSeconds(x, track.ActualWidth, _duration));
            e.Handled = true;
        });
        host.KeyDown += (_, e) => DebugLog.SafeRun("MomentTimeline.Key", () =>
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
        return new Lane { Track = track, Marks = marks, Playhead = playhead, Offset = offset, Host = host };
    }

    private double LaneWidth => _chaptersLane.Track.ActualWidth;

    private void Layout(bool force)
    {
        var width = LaneWidth;
        if (!force && Math.Abs(width - _laidOutWidth) < 0.5) return;
        _laidOutWidth = width;
        foreach (var lane in _lanes) lane.Marks.Children.Clear();
        _ruler.Children.Clear();
        if (width > 0)
        {
            var gap = MomentTimelinePolicy.PixelSeconds(_duration, width);
            LayoutExamined(width, gap);
            LayoutChapters(width);
            LayoutSpeech(width, gap);
            LayoutRuler(width);
        }
        foreach (var lane in _lanes)
        {
            lane.Track.Clip = new RectangleGeometry { Rect = new Windows.Foundation.Rect(0, 0, Math.Max(0, width), LaneHeight) };
        }
        _examinedLane.Value = MomentTimelinePolicy.ExaminedValue(_examined.Count);
        _chaptersLane.Value = MomentTimelinePolicy.LaneValue(_chapters.Count);
        _speechLane.Value = MomentTimelinePolicy.SpeechValue(_speech.Count);
        MovePlayheads();
    }

    private void LayoutExamined(double width, double gap)
    {
        foreach (var span in MomentTimelinePolicy.MergeSpans(_examined, _duration, gap))
        {
            var (x, markWidth, point) = MomentTimelinePolicy.ExaminedMark(span, _duration, width);
            FrameworkElement mark = point
                ? new Rectangle { Width = markWidth, Height = LaneHeight - 4, Fill = _examinedTick }
                : new Border { Width = markWidth, Height = LaneHeight - 6, CornerRadius = new CornerRadius(2), Background = _examinedSpan };
            Canvas.SetLeft(mark, x);
            Canvas.SetTop(mark, point ? 2 : 3);
            _examinedLane.Marks.Children.Add(mark);
        }
    }

    private void LayoutChapters(double width)
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
            _chaptersLane.Marks.Children.Add(span);
        }
    }

    private void LayoutSpeech(double width, double gap)
    {
        foreach (var span in MomentTimelinePolicy.MergeSpans(_speech, _duration, gap))
        {
            var (x, markWidth) = MomentTimelinePolicy.SpeechMark(span, _duration, width);
            var mark = new Border { Width = markWidth, Height = LaneHeight - 8, CornerRadius = new CornerRadius(2), Background = _speechSpan };
            Canvas.SetLeft(mark, x);
            Canvas.SetTop(mark, 4);
            _speechLane.Marks.Children.Add(mark);
        }
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

    private void MovePlayheads()
    {
        var width = LaneWidth;
        var x = MomentTimelinePolicy.X(_playhead, _duration, width) - 0.75;
        foreach (var lane in _lanes)
        {
            lane.Playhead.Visibility = width > 0 ? Visibility.Visible : Visibility.Collapsed;
            lane.Offset.X = x;
            var spoken = MomentTimelinePolicy.LaneSpoken(lane.Value, _playhead);
            if (spoken == lane.Spoken) continue;
            lane.Spoken = spoken;
            AutomationProperties.SetHelpText(lane.Host, spoken);
        }
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

    private sealed class Lane
    {
        public required Grid Track { get; init; }
        public required Canvas Marks { get; init; }
        public required Rectangle Playhead { get; init; }
        public required TranslateTransform Offset { get; init; }
        public required ContentControl Host { get; init; }
        public string Value { get; set; } = "";
        public string Spoken { get; set; } = "";
    }
}
