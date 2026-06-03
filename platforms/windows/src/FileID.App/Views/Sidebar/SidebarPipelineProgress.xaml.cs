// SidebarPipelineProgress code-behind. Builds five stage cells, each
// containing a dot + label + left/right connector halves. 1:1 port of
// macOS SidebarPipelineProgress.swift: dots and labels share the same
// 5-equal-column layout so they always align vertically; connectors
// live in the same column as the dot they belong to.

using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.UI;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarPipelineProgress : UserControl
{
    private static readonly (string Label, int Index)[] Stages =
    {
        ("Scan",     0),
        ("Tag",      1),
        ("People",   2),
        ("Captions", 3),
        ("Done",     4),
    };

    private readonly Ellipse[] _dots = new Ellipse[Stages.Length];
    private readonly Rectangle?[] _leftConnectors = new Rectangle?[Stages.Length];
    private readonly Rectangle?[] _rightConnectors = new Rectangle?[Stages.Length];
    private readonly TextBlock[] _labels = new TextBlock[Stages.Length];
    private readonly StackPanel[] _cells = new StackPanel[Stages.Length];

    // Cache the SyncStage brushes once: it fires ~10 Hz during a scan, and
    // allocating four SolidColorBrushes (DispatcherObjects) per call churned the
    // UI thread. Built in the ctor (UI thread) since SolidColorBrush is UI-affined.
    private Brush? _goldBrush;
    private SolidColorBrush? _fadedGold;
    private SolidColorBrush? _goldStroke;
    private Brush? _primaryText;
    private Brush? _secondaryText;
    private Brush? _tertiaryText;
    private SolidColorBrush? _inactiveDot;
    private SolidColorBrush? _inactiveDotStroke;
    private SolidColorBrush? _inactiveConnector;

    public SidebarPipelineProgress()
    {
        InitializeComponent();
        BuildStages();
        // Ctor runs on UI thread; cache UI-thread-affined brushes here
        // so SyncStage never allocates during the scan-event burst.
        _goldBrush = FileID.Services.ThemeHelper.GetBrushSafe("GoldBrush");
        _fadedGold = new SolidColorBrush(Color.FromArgb(0x99, 0xFF, 0xCC, 0x00));
        _goldStroke = new SolidColorBrush(Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00));
        _primaryText = FileID.Services.ThemeHelper.GetBrushSafe("TextFillColorPrimaryBrush");
        _secondaryText = FileID.Services.ThemeHelper.GetBrushSafe("TextFillColorSecondaryBrush");
        _tertiaryText = FileID.Services.ThemeHelper.GetBrushSafe("TextFillColorTertiaryBrush");
        _inactiveDot = InactiveDotBrush();
        _inactiveDotStroke = InactiveDotStrokeBrush();
        _inactiveConnector = InactiveConnectorBrush();

        Loaded += (_, _) => SyncStage();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += (_, _) => EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void BuildStages()
    {
        // 5 equal columns — each owns one stage. Dot + label stacked.
        for (int i = 0; i < Stages.Length; i++)
        {
            StagesRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        }

        for (int i = 0; i < Stages.Length; i++)
        {
            var (label, _) = Stages[i];

            // Stack inside this column: dot row (with connector halves) + label.
            var cellStack = new StackPanel
            {
                Spacing = 4,
                HorizontalAlignment = HorizontalAlignment.Stretch,
            };

            // Dot row — Grid with 3 columns (left half | dot | right half).
            // The connector lines fill their halves; the dot is centered.
            var dotRow = new Grid { Height = 14 };
            dotRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            dotRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            dotRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

            // WinUI 3 has no Visibility.Hidden — for the first stage we
            // simply don't add the left connector (the column slot stays
            // so the dot still centers correctly).
            Rectangle? leftConn = null;
            if (i > 0)
            {
                leftConn = new Rectangle
                {
                    Height = 1,
                    VerticalAlignment = VerticalAlignment.Center,
                    HorizontalAlignment = HorizontalAlignment.Stretch,
                    Fill = InactiveConnectorBrush(),
                };
                Grid.SetColumn(leftConn, 0);
                dotRow.Children.Add(leftConn);
            }
            _leftConnectors[i] = leftConn;

            var dot = new Ellipse
            {
                Width = 10,
                Height = 10,
                VerticalAlignment = VerticalAlignment.Center,
                HorizontalAlignment = HorizontalAlignment.Center,
                Fill = InactiveDotBrush(),
                Stroke = InactiveDotStrokeBrush(),
                StrokeThickness = 1,
            };
            Grid.SetColumn(dot, 1);
            dotRow.Children.Add(dot);
            _dots[i] = dot;

            Rectangle? rightConn = null;
            if (i < Stages.Length - 1)
            {
                rightConn = new Rectangle
                {
                    Height = 1,
                    VerticalAlignment = VerticalAlignment.Center,
                    HorizontalAlignment = HorizontalAlignment.Stretch,
                    Fill = InactiveConnectorBrush(),
                };
                Grid.SetColumn(rightConn, 2);
                dotRow.Children.Add(rightConn);
            }
            _rightConnectors[i] = rightConn;

            cellStack.Children.Add(dotRow);

            // Label — single-line, centered, small font (matches macOS 8 pt).
            var labelText = new TextBlock
            {
                Text = label,
                FontSize = 10,
                FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
                HorizontalAlignment = HorizontalAlignment.Center,
                TextWrapping = TextWrapping.NoWrap,
                TextTrimming = TextTrimming.None,
                Foreground = FileID.Services.ThemeHelper.GetBrushSafe("TextFillColorTertiaryBrush"),
            };
            cellStack.Children.Add(labelText);
            _labels[i] = labelText;

            // Each stage cell announces its name + state to a screen reader;
            // SyncStage refreshes the state suffix as the pipeline advances.
            _cells[i] = cellStack;
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(cellStack, $"{label} stage");

            Grid.SetColumn(cellStack, i);
            StagesRow.Children.Add(cellStack);
        }
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("SidebarPipelineProgress.OnEngineChanged", () =>
        {
            if (e.PropertyName is nameof(EngineClient.Phase)
                              or nameof(EngineClient.LastFaceClustering)
                              or nameof(EngineClient.DeepAnalyzeComplete)
                              or nameof(EngineClient.DeepAnalyzeProgress)
                              or nameof(EngineClient.DeepAnalyzeStarting)
                              or nameof(EngineClient.LastProgress))
            {
                DebugLog.Debug($"[ENGINE-SUB:SidebarPipelineProgress] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(SyncStage);
            }
        });

    // Last activeIndex actually rendered; lets SyncStage early-return on the
    // ~10 Hz LastProgress storm when the pipeline stage hasn't changed.
    private int _lastRenderedIndex = int.MinValue;

    private void SyncStage()
    {
        // Stage 0 = Scan (Discovering)
        // Stage 1 = Tag (Tagging)
        // Stage 2 = People (PostScan or face clustering)
        // Stage 3 = Captions (Deep Analyze in flight or complete)
        // Stage 4 = Done

        var phase = EngineClient.Instance.Phase ?? EngineClient.Instance.LastProgress?.Phase;
        bool peopleDone = EngineClient.Instance.LastFaceClustering is not null;
        bool captionsRunning = EngineClient.Instance.DeepAnalyzeProgress is not null
                            || EngineClient.Instance.DeepAnalyzeStarting is not null;
        // any DeepAnalyzeComplete event (cancelled or finished) is
        // a terminal state — flip the strip to "Done" rather than freezing
        // at Captions when the user cancels.
        bool captionsDone = EngineClient.Instance.DeepAnalyzeComplete is not null;

        int activeIndex = phase switch
        {
            ScanPhase.Discovering => 0,
            ScanPhase.Tagging => 1,
            ScanPhase.PostScan => 2,
            _ => -1,
        };
        if (activeIndex < 0)
        {
            if (captionsDone) activeIndex = 4;
            else if (captionsRunning) activeIndex = 3;
            else if (peopleDone) activeIndex = 2;
            // A finished scan never regresses below the People stage: face
            // clustering auto-runs right after ScanComplete, so before its event
            // lands all the latches above are still unset and activeIndex would
            // stay -1 — blanking the whole strip to grey for a beat on EVERY scan
            // completion. Hold at People (2) instead of going dark.
            else if (phase == ScanPhase.Completed) activeIndex = 2;
        }

        // The rendered strip is a pure function of activeIndex, but LastProgress
        // fires ~10 Hz throughout a scan while activeIndex changes only a handful
        // of times. Skip the redundant 5-cell rewrite (Fill/Width/Height + a
        // per-cell AutomationProperties string allocation) when nothing changed.
        if (activeIndex == _lastRenderedIndex) return;
        _lastRenderedIndex = activeIndex;

        // brushes cached at ctor time — see field comments.
        for (int i = 0; i < Stages.Length; i++)
        {
            bool filled = i < activeIndex || activeIndex == 4;
            bool active = i == activeIndex;

            // Dot fill + stroke.
            if (filled)
            {
                _dots[i].Fill = _goldBrush;
                _dots[i].Stroke = _goldStroke;
                _dots[i].StrokeThickness = 1; // reset the 1.5 left over from the active state
                _dots[i].Width = 10; _dots[i].Height = 10;
            }
            else if (active)
            {
                _dots[i].Fill = _fadedGold;
                _dots[i].Stroke = _goldStroke;
                _dots[i].StrokeThickness = 1.5;
                _dots[i].Width = 12; _dots[i].Height = 12;
            }
            else
            {
                _dots[i].Fill = _inactiveDot;
                _dots[i].Stroke = _inactiveDotStroke;
                _dots[i].StrokeThickness = 1;
                _dots[i].Width = 10; _dots[i].Height = 10;
            }

            // Label color: active → gold, filled → primary, else → tertiary.
            _labels[i].Foreground = active ? _goldBrush : (filled ? _primaryText : _tertiaryText);

            // Refresh the screen-reader state suffix for this stage cell.
            string state = active ? "in progress" : (filled ? "complete" : "pending");
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
                _cells[i], $"{Stages[i].Label} stage, {state}");

            // Connectors: a half is "filled" iff the dot it connects to AND
            // the dot it leads from are filled (or the half belongs to the
            // active dot extending toward a filled side).
            bool leftFilled = i > 0 && (i - 1 < activeIndex || activeIndex == 4);
            bool rightFilled = i < Stages.Length - 1 && filled;

            if (_leftConnectors[i] is { } lc)
                lc.Fill = leftFilled ? _goldBrush : _inactiveConnector;
            if (_rightConnectors[i] is { } rc)
                rc.Fill = rightFilled ? _goldBrush : _inactiveConnector;
        }
    }

    private static SolidColorBrush InactiveDotBrush() =>
        new(Color.FromArgb(0x1F, 0xFF, 0xFF, 0xFF));

    private static SolidColorBrush InactiveDotStrokeBrush() =>
        new(Color.FromArgb(0x2E, 0xFF, 0xFF, 0xFF));

    private static SolidColorBrush InactiveConnectorBrush() =>
        new(Color.FromArgb(0x1A, 0xFF, 0xFF, 0xFF));
}
