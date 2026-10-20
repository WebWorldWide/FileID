// SidebarPipelineProgress code-behind. Five capsule segments plus a
// "Step N of 5 · Stage" line inside the v3 scan card (macOS PipelineBlock).
// The stage derivation (live engine signals over a DB-derived launch floor)
// is unchanged from the dot strip it replaces.

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
        ("Scan",     0),
        ("Tag",      1),
        ("Faces",    2),
        ("Captions", 3),
        ("Done",     4),
    };

    private readonly Border[] _segments = new Border[Stages.Length];

    // Cached once in the ctor (UI thread): SyncStage fires ~10 Hz during a
    // scan and must never allocate DispatcherObjects.
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

        // Seed from the persisted (DB-derived) scan state so the strip reflects
        // a prior session's work instead of blanking to grey on every launch of
        // an existing library. Live engine signals always win (see SyncStage).
        SeedFromPersistedState();
    }

    // DB-derived launch floor. -1 = no persisted state (fresh library or DB not
    // yet created); otherwise the post-scan stage to fall back to when no live
    // engine signal has arrived. Mirrors macOS PipelineProgress, which reads
    // cheap DB counters so the strip stays accurate across launches.
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
                    // A non-empty library was scanned in a prior session. Hold the
                    // strip at People (post-scan) — the same floor SyncStage applies
                    // right after ScanComplete — rather than blanking to grey.
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
                DebugLog.Debug($"[ENGINE-SUB:SidebarPipelineProgress] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(SyncStage);
            }
        });

    // HasFolder goes false on BOTH a wipe (FinishWipeAsync nulls FolderPath after
    // the engine-side truncate / fallback DB-delete+restart has settled the DB)
    // and a plain "Clear folder" (which leaves the DB fully populated). Don't
    // hard-blank the floor on the signal — that wrongly greyed the strip after a
    // clear-then-repick of an intact library (the DB floor is seeded only once in
    // the ctor, so it never re-derives). Instead re-derive the floor from the DB:
    // a real wipe leaves it empty/gone -> floor -1 -> SyncStage blanks to grey;
    // a clear-folder leaves it populated -> floor 2 -> the strip stays lit.
    private void OnAppChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("SidebarPipelineProgress.OnAppChanged", () =>
        {
            if (e.PropertyName is nameof(AppViewModel.HasFolder)
                && !AppViewModel.Instance.HasFolder)
            {
                ReseedFloorFromDb();
            }
        });

    // Re-derive the persisted DB floor and APPLY it (raise OR lower — unlike
    // SeedFromPersistedState, which only raises on launch) after a folder
    // transition. The DB is in its settled post-op state when HasFolder flips
    // (wipe truncates/recreates before nulling FolderPath; clear-folder doesn't
    // touch it), so KindCounts is authoritative.
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
            // No live engine signal at all (fresh launch of an existing library):
            // fall back to the persisted DB-derived stage so the strip reflects a
            // prior session's scan instead of blanking to grey.
            else if (_dbDerivedIndex >= 0) activeIndex = _dbDerivedIndex;
        }

        // The rendered strip is a pure function of activeIndex, but LastProgress
        // fires ~10 Hz throughout a scan while activeIndex changes only a handful
        // of times. Skip the redundant 5-cell rewrite (Fill/Width/Height + a
        // per-cell AutomationProperties string allocation) when nothing changed.
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
