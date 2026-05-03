// SidebarPipelineProgress code-behind. Builds five dots + four connectors
// and tints them based on the engine's reported phase + completion of
// face-clustering / deep-analyze stages.

using System.ComponentModel;
using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;
using Windows.UI;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarPipelineProgress : UserControl
{
    private const int StageCount = 5; // Scan, Tag, People, Captions, Done
    private readonly Ellipse[] _dots = new Ellipse[StageCount];
    private readonly Rectangle[] _connectors = new Rectangle[StageCount - 1];

    public SidebarPipelineProgress()
    {
        InitializeComponent();
        BuildDots();
        Loaded += (_, _) => SyncStage();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
    }

    private void BuildDots()
    {
        // Layout: dot, connector, dot, connector, ... dot.
        // Use a Grid with 9 alternating star columns (dot=auto, connector=*).
        for (int i = 0; i < StageCount; i++)
        {
            DotsRow.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            if (i < StageCount - 1)
            {
                DotsRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            }
        }

        for (int i = 0; i < StageCount; i++)
        {
            var dot = new Ellipse
            {
                Width = 12,
                Height = 12,
                VerticalAlignment = VerticalAlignment.Center,
                HorizontalAlignment = HorizontalAlignment.Center,
                Fill = new SolidColorBrush(Color.FromArgb(0x55, 0xFF, 0xFF, 0xFF)),
            };
            Grid.SetColumn(dot, i * 2);
            DotsRow.Children.Add(dot);
            _dots[i] = dot;

            if (i < StageCount - 1)
            {
                var line = new Rectangle
                {
                    Height = 2,
                    Fill = new SolidColorBrush(Color.FromArgb(0x33, 0xFF, 0xFF, 0xFF)),
                    VerticalAlignment = VerticalAlignment.Center,
                    HorizontalAlignment = HorizontalAlignment.Stretch,
                    Margin = new Thickness(2, 0, 2, 0),
                };
                Grid.SetColumn(line, i * 2 + 1);
                DotsRow.Children.Add(line);
                _connectors[i] = line;
            }
        }
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(EngineClient.Phase)
                          or nameof(EngineClient.LastFaceClustering)
                          or nameof(EngineClient.DeepAnalyzeComplete)
                          or nameof(EngineClient.LastProgress))
        {
            DispatcherQueue.TryEnqueue(SyncStage);
        }
    }

    private void SyncStage()
    {
        // Stage 0 = Scan (Discovering)
        // Stage 1 = Tag (Tagging)
        // Stage 2 = People (face clustering, indicated by LastFaceClustering present)
        // Stage 3 = Captions (Deep Analyze in flight or complete)
        // Stage 4 = Done

        var phase = EngineClient.Instance.Phase ?? EngineClient.Instance.LastProgress?.Phase;
        bool peopleDone = EngineClient.Instance.LastFaceClustering is not null;
        bool captionsRunning = EngineClient.Instance.DeepAnalyzeProgress is not null
                            || EngineClient.Instance.DeepAnalyzeStarting is not null;
        bool captionsDone = EngineClient.Instance.DeepAnalyzeComplete is { Cancelled: false };

        // Activity stage: where we are RIGHT NOW. "Filled" stages are those
        // before activity. "Inactive" are after.
        int activeIndex = phase switch
        {
            ScanPhase.Discovering => 0,
            ScanPhase.Tagging     => 1,
            ScanPhase.PostScan    => 2,
            _                      => -1,
        };
        if (activeIndex < 0)
        {
            // Not scanning; pick the latest completed stage.
            if (captionsDone) activeIndex = 4;
            else if (captionsRunning) activeIndex = 3;
            else if (peopleDone) activeIndex = 2;
            else activeIndex = -1;
        }

        var goldBrush = (SolidColorBrush)Application.Current.Resources["GoldBrush"];
        var fadedGold = new SolidColorBrush(Color.FromArgb(0x66, 0xFF, 0xCC, 0x00));
        var inactive = new SolidColorBrush(Color.FromArgb(0x55, 0xFF, 0xFF, 0xFF));

        for (int i = 0; i < StageCount; i++)
        {
            if (i < activeIndex)
            {
                _dots[i].Fill = fadedGold;
            }
            else if (i == activeIndex)
            {
                _dots[i].Fill = goldBrush;
                // Subtle drop shadow for the active dot.
            }
            else
            {
                _dots[i].Fill = inactive;
            }
        }
        for (int i = 0; i < _connectors.Length; i++)
        {
            _connectors[i].Fill = i < activeIndex ? fadedGold : new SolidColorBrush(Color.FromArgb(0x33, 0xFF, 0xFF, 0xFF));
        }
    }
}
