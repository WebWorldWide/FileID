// SidebarPipelineProgress code-behind. Five capsule segments plus a "Step N of 5 · Stage" line in the scan card; the stage comes from live engine signals over a DB-derived launch floor.

using System.ComponentModel;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarPipelineProgress : UserControl
{
    private static readonly (string Label, int Index)[] Stages =
    {
        ("Find files", 0),
        ("Tag", 1),
        ("Faces", 2),
        ("Describe (optional)", 3),
        ("Done", 4),
    };

    private readonly Border[] _segments = new Border[Stages.Length];

    // Cached once in the ctor (UI thread): SyncStage fires ~10 Hz during a scan and must never allocate DispatcherObjects.
    private readonly Brush _goldBrush;
    private readonly Brush _aiBrush;
    private readonly Brush _delightBrush;
    private readonly Brush _doneBrush;
    private readonly Brush _pendingBrush;

    public SidebarPipelineProgress()
    {
        InitializeComponent();
        _goldBrush = FileID.Services.ThemeHelper.GetBrushSafe("GoldBrush");
        _aiBrush = FileID.Services.ThemeHelper.GetBrushSafe("AiBrush");
        _delightBrush = FileID.Services.ThemeHelper.GetBrushSafe("DelightBrush");
        _doneBrush = FileID.Services.ThemeHelper.GetBrushSafe("StepDoneBrush");
        _pendingBrush = FileID.Services.ThemeHelper.GetBrushSafe("StepPendingBrush");
        BuildStages();

        Loaded += (_, _) => SyncStage();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        AppViewModel.Instance.PropertyChanged += OnAppChanged;
        Unloaded += (_, _) =>
        {
            EngineClient.Instance.PropertyChanged -= OnEngineChanged;
            AppViewModel.Instance.PropertyChanged -= OnAppChanged;
        };

        SeedFromPersistedState();
    }

    // DB-derived launch floor. -1 = no persisted state (fresh library or DB not yet created); otherwise the post-scan stage to fall back to when no live engine signal has arrived.
    private int _dbDerivedIndex = -1;

    private void SeedFromPersistedState()
    {
        var ui = DispatcherQueue;
        _ = DebugLog.SafeRunAsync(nameof(SeedFromPersistedState), async () =>
        {
            int derived = await Task.Run(async () =>
            {
                try
                {
                    await using var store = new ReadStore(AppPaths.DbPath);
                    await store.OpenAsync().ConfigureAwait(false);
                    if (!store.IsOpen) return -1; // first launch: engine hasn't created the DB
                    var counts = await store.KindCountsAsync(default).ConfigureAwait(false);
                    int total = 0;
                    foreach (var n in counts.Values) total += n;
                    return total > 0 ? 2 : -1;
                }
                catch
                {
                    return -1; // locked/unreadable DB: leave the strip on live state
                }
            }).ConfigureAwait(false);

            if (derived < 0) return;
            ui.TryEnqueue(() =>
            {
                if (derived <= _dbDerivedIndex) return;
                _dbDerivedIndex = derived;
                SyncStage();
            });
        });
    }

    private void BuildStages()
    {
        for (int i = 0; i < Stages.Length; i++)
        {
            StagesRow.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            var segment = new Border
            {
                Height = 3,
                CornerRadius = new CornerRadius(1.5),
                Background = _pendingBrush,
            };
            Grid.SetColumn(segment, i);
            StagesRow.Children.Add(segment);
            _segments[i] = segment;
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
                DebugLog.Info($"[ENGINE-SUB:SidebarPipelineProgress] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(SyncStage);
            }
        });

    // The floor is seeded once in the ctor and never re-derives, so blanking it here would grey the strip after a clear-then-repick of an intact library.
    private void OnAppChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("SidebarPipelineProgress.OnAppChanged", () =>
        {
            if (e.PropertyName is nameof(AppViewModel.HasFolder)
                && !AppViewModel.Instance.HasFolder)
            {
                ReseedFloorFromDb();
            }
        });

    // Re-derive the persisted DB floor and APPLY it (raise OR lower — unlike SeedFromPersistedState, which only raises on launch) after a folder transition.
    private void ReseedFloorFromDb()
    {
        var ui = DispatcherQueue;
        _ = DebugLog.SafeRunAsync(nameof(ReseedFloorFromDb), async () =>
        {
            int derived = await Task.Run(async () =>
            {
                try
                {
                    await using var store = new ReadStore(AppPaths.DbPath);
                    await store.OpenAsync().ConfigureAwait(false);
                    if (!store.IsOpen) return -1; // wiped/absent DB
                    var counts = await store.KindCountsAsync(default).ConfigureAwait(false);
                    int total = 0;
                    foreach (var n in counts.Values) total += n;
                    return total > 0 ? 2 : -1;
                }
                catch
                {
                    return -1;
                }
            }).ConfigureAwait(false);

            ui.TryEnqueue(() =>
            {
                _dbDerivedIndex = derived;
                _lastRenderedIndex = int.MinValue;
                SyncStage();
            });
        });
    }

    private int _lastRenderedIndex = int.MinValue;

    private void SyncStage()
    {
        var phase = EngineClient.Instance.Phase ?? EngineClient.Instance.LastProgress?.Phase;
        bool peopleDone = EngineClient.Instance.LastFaceClustering is not null;
        bool captionsRunning = EngineClient.Instance.DeepAnalyzeProgress is not null
                            || EngineClient.Instance.DeepAnalyzeStarting is not null;
        // any DeepAnalyzeComplete event (cancelled or finished) is a terminal state — flip the strip to "Done" rather than freezing at Captions when the user cancels.
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
            else if (phase == ScanPhase.Completed) activeIndex = 2;
            else if (_dbDerivedIndex >= 0) activeIndex = _dbDerivedIndex;
        }

        // The rendered strip is a pure function of activeIndex, but LastProgress fires ~10 Hz throughout a scan while activeIndex changes only a handful of times.
        if (activeIndex == _lastRenderedIndex) return;
        _lastRenderedIndex = activeIndex;

        var current = Math.Max(activeIndex, 0);
        for (int i = 0; i < Stages.Length; i++)
        {
            bool done = activeIndex >= 0 && i < activeIndex;
            bool active = i == activeIndex;
            _segments[i].Background = active
                ? activeIndex switch
                {
                    3 => _aiBrush,
                    4 => _delightBrush,
                    _ => _goldBrush,
                }
                : done ? _doneBrush : _pendingBrush;
        }

        var stepLabel = $"Step {current + 1} of {Stages.Length} · {Stages[current].Label}";
        StepText.Text = stepLabel;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(this, $"Pipeline, {stepLabel}");
    }
}
