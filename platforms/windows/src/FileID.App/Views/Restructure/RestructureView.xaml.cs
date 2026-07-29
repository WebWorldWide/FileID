// RestructureView code-behind — recommendation-first + file-first reorg UI
// (port of macOS RestructureView.swift). Reads EngineClient.LastRestructurePlan,
// groups the moves by Tier into Keep / Tidy / Reorganize recommendation cards,
// and drives a per-file + per-group selection model whose count is, by
// construction, identical to the move set Apply sends to the engine.
//
// Crash-safety (platforms/windows/CLAUDE.md): the recommendation + file lists
// are ItemsRepeater + DataTemplate over observable VMs (never imperative
// children); the engine subscription is SafeRun-wrapped, posts XAML writes via
// DispatcherQueue, logs [ENGINE-SUB:RestructureView], and is _unloaded-guarded;
// tints resolve via VM brushes / {ThemeResource}, never a code-side theme lookup.

using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views.Restructure;

public sealed partial class RestructureView : UserControl
{
    private const int InlineFileCap = 30;

    private readonly ObservableCollection<RestructureRecommendationVm> _recommendations = new();
    private readonly ObservableCollection<RestructureLargePlanCategoryVm> _largePlanCategories = new();
    private readonly Dictionary<long, RestructureFileRowVm> _allFileRows = new();
    private readonly Dictionary<RestructureOutcome, List<RestructureFileRowVm>> _filesByOutcome = new();
    private readonly Dictionary<RestructureOutcome, RestructureRecommendationVm> _recByOutcome = new();

    // Selection intent persisted across navigation. The view is recreated on
    // every tab switch (ctor re-subscribes, Unloaded unsubscribes), so the
    // per-file IsSelected flags — which default to all-selected — would reset
    // each return, silently discarding which files the user chose to exclude.
    // Static so it survives the view's recreation for the app session; cleared
    // only when a genuinely new plan is computed (OnLoaded's recompute path).
    private static readonly HashSet<long> _deselectedFileIds = new();
    // Companion to _deselectedFileIds: files the user EXPLICITLY selected. "ask"
    // rows default to deselected, so opting one in is an intent that deselection
    // tracking alone can't represent — without this it is wiped by the
    // ask-default-deselect on the next SyncPlan after a tab switch.
    private static readonly HashSet<long> _selectedFileIds = new();

    private bool _unloaded;
    private bool _subscribed;
    private bool _suppressRecompute;
    private bool _planIntegrityBlocked;
    // R6-04: static so the in-flight-apply guard survives the view's per-tab-switch
    // recreation (mirrors the existing static _deselectedFileIds). _applyingPlan is
    // the plan currently being applied; SyncPlan compares against it to tell a
    // genuine post-apply re-plan (release the guard) from a returning instance
    // re-rendering the SAME cached pre-apply plan (keep it engaged).
    private static bool _applying;
    private static RestructurePlan? _applyingPlan;
    // True from the apply command send until its result/error arrives. A fresh
    // plan landing in that window (user re-plan, DeepAnalyzeComplete auto
    // re-plan) must NOT release _applying: a second concurrent apply truncates
    // the first run's undo journal (open_undo_journal_truncating).
    private static bool _applyInFlight;
    private static bool _applyingAsShortcuts;
    // R6-06: the EngineClient.SpawnGeneration captured when the in-flight apply
    // was sent. If the generation moves while the guard is engaged, the engine
    // process that owned the apply (or its post-apply re-plan) is gone — its
    // result/error event can never arrive — so SyncEngineLifecycle releases the
    // guard instead of leaving Apply disabled for the rest of the session
    // (e.g. the external drive was unplugged mid-apply and the engine died).
    private static int _applyingSpawnGen;
    private static bool _planning;
    private static string? _planningRoot;
    private static int _planningSpawnGen;
    private bool _deepAnalyzeHintDismissed;
    private RestructureOutcome? _hovered;
    // H2: the exact plan instance SyncPlan rendered the reviewed rows from.
    // EngineClient sets LastRestructurePlan synchronously but SyncPlan is only
    // ENQUEUED, so a background re-plan (user re-plan / DeepAnalyzeComplete auto
    // re-plan) can swap LastRestructurePlan out from under the on-screen rows.
    // ApplyAsync refuses to apply when the live plan is no longer the one on
    // screen and re-renders instead, so a destructive apply can never target
    // destinations the user never reviewed. Paired with _allFileRows (both are
    // repopulated together by SyncPlan), so it is instance-scoped like them.
    private RestructurePlan? _renderedPlan;
    // R6-05: static (like _applying / _applyingPlan) so "the completion we've
    // already surfaced" survives this view's per-tab-switch recreation, so a
    // reload-time replay can't re-alert a completion an earlier instance already
    // handled. See OnLoaded / SyncApplyResult / SyncEngineError.
    private static EngineError? _lastHandledError;
    private static RestructureApplyResult? _lastHandledApplyResult;

    // UI-thread brushes cached at ctor time (CLAUDE.md: never build brushes per
    // event). Tile tints match RestructureRecommendationVm's outcome colors.
    private readonly SolidColorBrush _keepBrush;
    private readonly SolidColorBrush _tidyBrush;
    private readonly SolidColorBrush _reorgBrush;
    private readonly SolidColorBrush _idleTileStroke;

    public RestructureView()
    {
        InitializeComponent();
        _keepBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0x6C, 0xC2, 0x4A));
        _tidyBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0xFF, 0x9F, 0x45));
        _reorgBrush = new SolidColorBrush(Windows.UI.Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00));
        _idleTileStroke = new SolidColorBrush(Windows.UI.Color.FromArgb(0x18, 0xFF, 0xFF, 0xFF));

        SubscribeEvents();
        WireApplyBarHoverSprings();
        Loaded += OnLoaded;
        Unloaded += OnUnloaded;
    }

    private void SubscribeEvents()
    {
        if (_subscribed) return;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        AppViewModel.Instance.PropertyChanged += OnAppChanged;
        Services.ChangeLog.Instance.Changed += OnChangeLogChanged;
        Sankey.RibbonInvoked += OnSankeyRibbonInvoked;
        _subscribed = true;
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        if (!_subscribed) return;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        AppViewModel.Instance.PropertyChanged -= OnAppChanged;
        Services.ChangeLog.Instance.Changed -= OnChangeLogChanged;
        Sankey.RibbonInvoked -= OnSankeyRibbonInvoked;
        _subscribed = false;
    }

    // Normalized library-root equality (trailing-separator- and case-
    // insensitive, matching Windows path semantics). A null/empty either side
    // never matches — no active folder means no plan may apply.
    private static bool RootsMatch(string? planRoot, string? currentRoot)
    {
        if (string.IsNullOrEmpty(planRoot) || string.IsNullOrEmpty(currentRoot)) return false;
        static string Norm(string p) => p.TrimEnd('\\', '/');
        return string.Equals(Norm(planRoot), Norm(currentRoot), StringComparison.OrdinalIgnoreCase);
    }

    // CRITICAL: a plan is computed for one library root and never invalidated
    // when the active folder changes/clears/wipes. Without this, switching the
    // library after planning left the stale plan on screen with a live Apply
    // that would move files in the OLD folder. Drop the cached plan the moment
    // the active folder changes so the tab clears and re-plans for the new one.
    private void OnAppChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("RestructureView.OnAppChanged", () =>
        {
            if (_unloaded) return;
            if (e.PropertyName != nameof(AppViewModel.FolderPath)) return;
            DispatcherQueue.TryEnqueue(() => DebugLog.SafeRun(
                "RestructureView.OnAppChanged.Dispatch",
                () =>
                {
                    if (_unloaded) return;
                    EngineClient.Instance.InvalidateRestructurePlan();
                    SyncPlan();
                    SyncUndoAffordance();
                    ApplyStatusText.Text = string.Empty;
                    var folder = AppViewModel.Instance.FolderPath;
                    if (!string.IsNullOrEmpty(folder))
                    {
                        _ = RequestPlanForFolderAsync(folder, "folder change");
                    }
                }));
        });

    private void OnChangeLogChanged(object? sender, EventArgs e)
        => DebugLog.SafeRun("RestructureView.OnChangeLogChanged", () =>
        {
            if (_unloaded) return;
            DispatcherQueue.TryEnqueue(() =>
            {
                if (!_unloaded) SyncUndoAffordance();
            });
        });

    // macOS parity (RestructureApplyBar.swift): gold primary + outline secondary
    // scale to 1.02x on hover with a response 0.28 / dampingFraction 0.7 spring.
    private void WireApplyBarHoverSprings()
    {
        const double SpringResponse = 0.28;
        const double SpringDamping = 0.7;
        ApplySymlinkButton.PointerEntered += (_, _) =>
        {
            if (ApplySymlinkButton.IsEnabled)
                FileID.Theme.Motion.SpringEasing.AnimateScale(ApplySymlinkButton, 1.02f, SpringResponse, SpringDamping);
        };
        ApplySymlinkButton.PointerExited += (_, _) =>
            FileID.Theme.Motion.SpringEasing.AnimateScale(ApplySymlinkButton, 1.0f, SpringResponse, SpringDamping);
        ApplyMovesButton.PointerEntered += (_, _) =>
        {
            if (ApplyMovesButton.IsEnabled)
                FileID.Theme.Motion.SpringEasing.AnimateScale(ApplyMovesButton, 1.02f, SpringResponse, SpringDamping);
        };
        ApplyMovesButton.PointerExited += (_, _) =>
            FileID.Theme.Motion.SpringEasing.AnimateScale(ApplyMovesButton, 1.0f, SpringResponse, SpringDamping);
    }

    // macOS parity (RestructureView.swift `.task`): auto-generate the plan on
    // open. Render a cached plan if the engine still has one; otherwise compute.
    private async void OnLoaded(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnLoaded), async () =>
        {
            _unloaded = false;
            SubscribeEvents();
            _ = RefreshDeepAnalyzeHintAsync();
            SyncUndoAffordance();   // R2: reflect any pending undoable run on open
            // R6-05: the apply result/error is delivered via live PropertyChanged,
            // but leaving the Restructure tab mid-apply unsubscribes this view
            // (Unloaded), dropping that completion; because _applying is static the
            // single-flight guard would stay engaged for the whole session (Apply
            // buttons stuck disabled) — a 0-applied/failed/errored apply emits no
            // re-plan, so SyncPlan's reference check can't release it. Replay both
            // handlers on reload: each de-dupes on its static marker, so an apply
            // still genuinely in flight (slot unchanged) is a no-op that keeps the
            // guard engaged until the freshly re-subscribed live handler fires.
            if (_applying)
            {
                SyncApplyResult();
                SyncEngineError();
                // R6-06: an engine death while the tab was closed emits NEITHER
                // a result nor an apply_restructure error, so the replays above
                // can't release the guard — the generation check can.
                SyncEngineLifecycle();
            }
            var folder = AppViewModel.Instance.FolderPath;
            var cachedPlan = EngineClient.Instance.LastRestructurePlan;
            if (cachedPlan is not null && RootsMatch(cachedPlan.LibraryRoot, folder))
            {
                SyncPlan();
                return;
            }
            if (cachedPlan is not null)
            {
                EngineClient.Instance.InvalidateRestructurePlan();
                SyncPlan();
            }
            if (string.IsNullOrEmpty(folder))
            {
                PlanStatusText.Text = "Pick a library folder in the sidebar to plan a reorganization.";
                return;
            }
            await RequestPlanForFolderAsync(folder, "open");
        });

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("RestructureView.OnEngineChanged", () =>
        {
            if (_unloaded) return;
            switch (e.PropertyName)
            {
                case nameof(EngineClient.LastRestructurePlan):
                    DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncPlan(); });
                    break;
                case nameof(EngineClient.RestructurePlanDiscardedSignal):
                    DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() =>
                    {
                        if (!_unloaded) SyncDiscardedPlan();
                    });
                    break;
                case nameof(EngineClient.LastRestructureApplyResult):
                    DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncApplyResult(); });
                    break;
                case nameof(EngineClient.CanUndoRestructure):
                    DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncUndoAffordance(); });
                    break;
                case nameof(EngineClient.LastError):
                    DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncEngineError(); });
                    break;
                case nameof(EngineClient.State):
                    // R6-06: an engine that dies mid-apply emits neither a
                    // RestructureApplyResult nor an apply_restructure error, so
                    // the static single-flight guard would stay engaged for the
                    // whole session. A lifecycle transition is the only signal.
                    DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncEngineLifecycle(); });
                    break;
                case nameof(EngineClient.DeepAnalyzeCommandInFlight):
                case nameof(EngineClient.DeepAnalyzeProgress):
                    DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) UpdateDeepAnalyzeBanner(); });
                    break;
                case nameof(EngineClient.DeepAnalyzeComplete):
                    {
                        if (EngineClient.Instance.DeepAnalyzeComplete is null) break;
                        DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                        // macOS parity: re-plan when Deep Analyze finishes so the
                        // People/<name> buckets reflect newly-captioned files.
                        var folder = AppViewModel.Instance.FolderPath;
                        DispatcherQueue.TryEnqueue(async () =>
                        {
                            if (_unloaded) return;
                            await RefreshDeepAnalyzeHintAsync();
                            if (_unloaded) return;
                            if (!string.IsNullOrEmpty(folder))
                            {
                                await RequestPlanForFolderAsync(
                                    folder!,
                                    "Deep Analyze refresh");
                            }
                        });
                    }
                    break;
            }
        });

    private async void OnSankeyRibbonInvoked(object? sender, (string Source, string Category) ribbon)
        => await DebugLog.SafeRunAsync(nameof(OnSankeyRibbonInvoked), async () =>
        {
            var plan = _renderedPlan;
            if (plan is null || !IsFrozenPlanCurrent(
                    plan,
                    EngineClient.Instance.LastRestructurePlan,
                    _renderedPlan))
            {
                SyncPlan();
                ApplyStatusText.Text =
                    "The plan changed before the flow details opened — review the updated plan.";
                await ShowAlertAsync(
                    "Plan updated",
                    "The reorganization plan changed while you were reviewing it. " +
                    "The updated flow is on screen now; open its details again.");
                return;
            }
            var sheet = new DrillDownSheet();
            sheet.SetSankeyFilter(plan, ribbon.Source, ribbon.Category);
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = "Files in this flow",
                Content = sheet,
                CloseButtonText = "Done",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        });

    // ---- Plan rendering -------------------------------------------------

    private void SyncPlan()
    {
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan is null)
        {
            ClearPlanPresentation();
            return;
        }
        if (!RootsMatch(plan.LibraryRoot, AppViewModel.Instance.FolderPath))
        {
            CompletePlanningState();
            EngineClient.Instance.InvalidateRestructurePlan();
            ClearPlanPresentation();
            var currentRoot = AppViewModel.Instance.FolderPath;
            if (string.IsNullOrEmpty(currentRoot))
            {
                PlanStatusText.Text = "This plan was superseded. Pick a library folder to plan again.";
            }
            else
            {
                _ = RequestPlanForFolderAsync(currentRoot, "folder switch");
            }
            return;
        }

        // H2: record the exact plan the rows below are built from. ApplyAsync
        // compares the live plan against this to detect a background re-plan
        // that landed between review and click (see _renderedPlan).
        _renderedPlan = plan;

        // R6-04: a GENUINELY fresh plan supersedes any in-flight apply — release the
        // single-flight guard (F-C5-003). But the view is recreated on every tab
        // switch, so a returning instance can re-enter here with the SAME cached
        // pre-apply plan while the apply is still mid-flight; releasing then would
        // re-enable Apply and let a duplicate apply fire against already-moved
        // sources (the false "some changes couldn't be applied" alarm this guard
        // exists to prevent). Every plan event deserializes a NEW record instance,
        // so the post-apply re-plan is a different reference and still releases.
        // A new instance releases only once the apply's result/error has arrived
        // (_applyInFlight false) — a fresh plan generated DURING the apply must
        // keep Apply disabled or the concurrent run corrupts the undo journal.
        if (ShouldReleaseApplyGuardOnPlanArrival(_applyInFlight, plan, _applyingPlan))
        {
            _applying = false;
            _applyingPlan = null;
        }

        CompletePlanningState();
        var integrity = RestructurePlanPresentation.InspectPreview(plan);
        _planIntegrityBlocked = !integrity.IsSafe;
        if (_planIntegrityBlocked)
        {
            DebugLog.Warn($"Restructure plan preview failed integrity checks: {integrity.Summary}");
        }
        if (plan.Truncated)
        {
            SyncLargePlan(plan);
            return;
        }

        LargePlanCard.Visibility = Visibility.Collapsed;
        _largePlanCategories.Clear();
        _allFileRows.Clear();
        _filesByOutcome.Clear();
        _recByOutcome.Clear();
        _recommendations.Clear();

        foreach (var m in plan.Moves)
        {
            var outcome = RestructureGrouping.OutcomeForTier(m.Tier);
            var row = new RestructureFileRowVm { Move = m, SelectionChanged = OnFileSelectionChanged };
            _allFileRows[m.FileId] = row;
            if (!_filesByOutcome.TryGetValue(outcome, out var list))
            {
                list = new List<RestructureFileRowVm>();
                _filesByOutcome[outcome] = list;
            }
            list.Add(row);
        }

        int moveCount = plan.Moves.Count;
        int keepFolders = (int)(plan.FolderClassifications?.AnchorFolders ?? 0);
        int tidyFiles = CountOf(RestructureOutcome.Tidy);
        int reorgFiles = CountOf(RestructureOutcome.Reorganize);
        int tidyFolders = DistinctSourceFolders(RestructureOutcome.Tidy);
        int reorgFolders = DistinctSourceFolders(RestructureOutcome.Reorganize);

        if (keepFolders > 0)
        {
            AddRec(RestructureOutcome.Keep,
                $"Keep {Count(keepFolders, "folder")} untouched",
                "These folders already have clear names and matching contents - nothing about them changes.",
                fileCount: 0, folderCount: keepFolders, informational: true);
        }
        if (tidyFiles > 0)
        {
            AddRec(RestructureOutcome.Tidy,
                $"Tidy {Count(tidyFolders, "folder")} - move {Count(tidyFiles, "misplaced file")}",
                "Mostly-organized folders with a few files that don't fit. The folder stays; the misplaced files move to where they belong.",
                tidyFiles, tidyFolders, informational: false);
        }
        if (reorgFiles > 0)
        {
            AddRec(RestructureOutcome.Reorganize,
                $"Reorganize {Count(reorgFolders, "folder")} - sort {Count(reorgFiles, "file")}",
                "Folders with generic names like \"Untitled\" or \"Camera Roll\" - files sort into clear categories: People, Places, Documents, or Photos by year.",
                reorgFiles, reorgFolders, informational: false);
        }

        KeepValue.Text = keepFolders.ToString("N0");
        KeepHint.Text = keepFolders == 1 ? "folder kept intact" : "folders kept intact";
        TidyValue.Text = tidyFiles.ToString("N0");
        TidyHint.Text = tidyFolders == 1 ? "from 1 mixed folder" : $"from {tidyFolders:N0} mixed folders";
        ReorgValue.Text = reorgFiles.ToString("N0");
        ReorgHint.Text = reorgFolders == 1 ? "from 1 generic folder" : $"from {reorgFolders:N0} generic folders";

        // Stat-tile accessible names combine the value + its label so a screen
        // reader announces the whole stat (e.g. "Staying put: 12 folders kept intact").
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(KeepTile, $"Staying put: {KeepValue.Text} {KeepHint.Text}");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(TidyTile, $"Tidying: {TidyValue.Text} files {TidyHint.Text}");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(ReorgTile, $"Reorganizing: {ReorgValue.Text} files {ReorgHint.Text}");

        Sankey.SetPlan(plan);
        TreeDiff.SetPlan(plan);
        int srcCount = DistinctAllSourceFolders(plan);
        int bucketCount = plan.Moves
            .Select(m => m.Category)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .Count();
        SankeyHeroStat.Text =
            $"{srcCount} source folder{(srcCount == 1 ? "" : "s")} → " +
            $"{bucketCount} organization bucket{(bucketCount == 1 ? "" : "s")}";

        bool hasContent = moveCount > 0 || keepFolders > 0;
        bool hasMoves = moveCount > 0;
        PlanStatusText.Text = moveCount == 0
            ? "Your library is already organized - nothing to move."
            : plan.Truncated
                ? $"{moveCount:N0} files to reorganize across {plan.CategoryCounts.Count} categories. This large plan is stored by the engine and will be applied as one undoable run."
                : $"{moveCount:N0} files to reorganize across {plan.CategoryCounts.Count} categories.";
        StatHero.Visibility = hasContent && !plan.Truncated ? Visibility.Visible : Visibility.Collapsed;
        ViewModeToggle.Visibility = hasMoves && !plan.Truncated ? Visibility.Visible : Visibility.Collapsed;
        UnifiedSurface.Visibility = hasMoves && !plan.Truncated ? Visibility.Visible : Visibility.Collapsed;
        NothingToMoveCard.Visibility = hasMoves ? Visibility.Collapsed : Visibility.Visible;
        UpdateStayingPut(keepFolders);

        // Suppress per-row recomputes; RecomputeSelection runs once below.
        _suppressRecompute = true;
        // Ask-confidence moves start deselected — the user must explicitly check
        // them. (RESTRUCTURE.md §6 — confidence-tier autonomy)
        foreach (var kv in _allFileRows)
        {
            if (string.Equals(kv.Value.Move.Confidence, "ask", StringComparison.OrdinalIgnoreCase))
            {
                kv.Value.IsSelected = false;
            }
        }
        // Re-apply the explicit selections the user made before navigating away
        // (see _deselectedFileIds/_selectedFileIds). Intent in either direction
        // overrides the tier default, so an "ask" file the user opted in survives
        // the view's per-tab-switch recreation instead of reverting to deselected.
        foreach (var kv in _allFileRows)
        {
            if (_deselectedFileIds.Contains(kv.Key)) { kv.Value.IsSelected = false; }
            else if (_selectedFileIds.Contains(kv.Key)) { kv.Value.IsSelected = true; }
        }
        _suppressRecompute = false;

        ApplyBarTotalCount.Text = moveCount.ToString("N0");
        RecomputeSelection();
    }

    private void SyncDiscardedPlan()
    {
        var folder = AppViewModel.Instance.FolderPath;
        if (string.IsNullOrWhiteSpace(folder))
        {
            CompletePlanningState();
            ClearPlanPresentation();
            return;
        }

        _planning = true;
        _planningRoot = folder;
        _planningSpawnGen = EngineClient.Instance.SpawnGeneration;
        PlanBusyIndicator.IsActive = true;
        PlanBusyIndicator.Visibility = Visibility.Visible;
        ReplanButton.IsEnabled = false;
        PlanStatusText.Text =
            "Library inputs changed while planning — updating to a fresh plan…";
    }

    private void SyncLargePlan(RestructurePlan plan)
    {
        _allFileRows.Clear();
        _filesByOutcome.Clear();
        _recByOutcome.Clear();
        _recommendations.Clear();
        _largePlanCategories.Clear();
        Sankey.SetPlan(null);
        TreeDiff.SetPlan(null);

        var totalMoves = RestructurePlanPresentation.TotalMoves(plan);
        var hasCompleteConfidenceCounts =
            RestructurePlanPresentation.TryGetCompleteConfidenceCounts(
                plan,
                out var confidenceCounts);
        var categoryCount =
            RestructurePlanPresentation.CategoryCount(plan.CategoryCounts);
        var topCategories =
            RestructurePlanPresentation.TopCategories(plan.CategoryCounts);
        foreach (var category in topCategories)
        {
            _largePlanCategories.Add(category);
        }

        LargePlanMoveCount.Text = totalMoves.ToString("N0");
        LargePlanSummaryText.Text =
            $"{totalMoves:N0} proposals across {categoryCount:N0} " +
            $"organization bucket{(categoryCount == 1 ? "" : "s")} are stored outside the UI.";
        LargePlanBucketCount.Text =
            $"{categoryCount:N0} bucket{(categoryCount == 1 ? "" : "s")}";
        LargePlanMoreBucketsText.Text = categoryCount > topCategories.Count
            ? $"Showing the top {topCategories.Count:N0} organization buckets. " +
              $"{categoryCount - topCategories.Count:N0} more stay bounded in the engine. " +
              "These are semantic group totals, not destination folders."
            : "All organization-bucket totals are shown. These are semantic groups, not destination folders.";

        if (hasCompleteConfidenceCounts)
        {
            LargePlanAutoCount.Text = confidenceCounts.Auto.ToString("N0");
            LargePlanReviewCount.Text = confidenceCounts.Review.ToString("N0");
            LargePlanAskCount.Text = confidenceCounts.Ask.ToString("N0");
            var unknownDetail = confidenceCounts.Unknown > 0
                ? $" {confidenceCounts.Unknown:N0} proposals with unknown confidence are also held back."
                : string.Empty;
            LargePlanConfidenceDetailText.Text =
                "Review and Needs approval proposals cannot be inspected or selected individually " +
                "in this bounded large-plan view; plan a smaller folder for per-file review." +
                unknownDetail;
        }
        else
        {
            LargePlanAutoCount.Text = "—";
            LargePlanReviewCount.Text = "—";
            LargePlanAskCount.Text = "—";
            LargePlanConfidenceDetailText.Text =
                "The engine did not provide authoritative full-plan confidence totals. " +
                "Apply is disabled; generate a fresh plan before making changes.";
        }

        var integrity = RestructurePlanPresentation.InspectPreview(plan);
        var driveRootWarning = RestructurePlanPresentation.IsDriveRoot(plan.LibraryRoot)
            ? $" The selected library is the drive root '{plan.LibraryRoot}', so FileID may create or " +
              "reorganize top-level folders across that drive. Choose a narrower folder for finer review."
            : string.Empty;
        LargePlanSafetyText.Text = !integrity.IsSafe
            ? $"Apply is blocked because the visible sample failed basic path checks: {integrity.Summary}. " +
              "That sample never proves the unseen stored proposals are safe. " +
              $"The selected library root is '{plan.LibraryRoot}'. Generate a fresh plan before making any changes."
            : !hasCompleteConfidenceCounts
                ? "The visible sample does not prove the unseen stored proposals are safe. " +
                  "Apply remains disabled until a fresh plan supplies complete confidence totals."
                : "The visible sample does not prove the unseen stored proposals are safe. " +
                  "Immediately before the first change, the engine preflights every stored proposal. " +
                  "Only after that full preflight succeeds does it run Auto moves as one crash-journaled batch; " +
                  "Review, Needs approval, and unknown-confidence proposals stay put. " +
                  "Real moves can be reversed with Undo last run. Shortcut runs leave originals in place and " +
                  "appear in Recent Changes, but their links must be removed manually." + driveRootWarning;

        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
            LargePlanCard,
            $"Large plan ready: {totalMoves:N0} proposals across {categoryCount:N0} organization buckets. " +
            (hasCompleteConfidenceCounts
                ? $"{confidenceCounts.Auto:N0} Auto, {confidenceCounts.Review:N0} Review, " +
                  $"{confidenceCounts.Ask:N0} Needs approval. "
                : "Full-plan confidence totals are unavailable. ") +
            (integrity.IsSafe && hasCompleteConfidenceCounts
                ? "Only Auto moves will apply after a full engine preflight."
                : "Apply is blocked."));

        KeepValue.Text = (plan.FolderClassifications?.AnchorFolders ?? 0).ToString("N0");
        KeepHint.Text = "folders kept intact";
        TidyValue.Text = "0";
        TidyHint.Text = string.Empty;
        ReorgValue.Text = "0";
        ReorgHint.Text = string.Empty;
        SankeyHeroStat.Text = string.Empty;
        StatHero.Visibility = Visibility.Collapsed;
        ViewModeToggle.Visibility = Visibility.Collapsed;
        UnifiedSurface.Visibility = Visibility.Collapsed;
        NothingToMoveCard.Visibility = Visibility.Collapsed;
        LargePlanCard.Visibility = Visibility.Visible;
        UpdateStayingPut((int)(plan.FolderClassifications?.AnchorFolders ?? 0));

        PlanStatusText.Text = !integrity.IsSafe
            ? "Plan blocked: duplicate, invalid, or outside-library paths were detected in its visible sample."
            : !hasCompleteConfidenceCounts
                ? "Plan blocked: authoritative full-plan confidence totals are unavailable. Generate it again."
                : confidenceCounts.Auto == 0
                    ? $"Large plan ready, but none of its {totalMoves:N0} proposals are Auto-confidence. " +
                      "Plan a smaller folder to inspect Review and Needs approval items."
                    : $"Large plan ready: {confidenceCounts.Auto:N0} Auto moves can apply; " +
                      $"{confidenceCounts.Review + confidenceCounts.Ask + confidenceCounts.Unknown:N0} proposals stay put.";
        RecomputeSelection();
    }

    private void ClearPlanPresentation()
    {
        _renderedPlan = null;
        _planIntegrityBlocked = false;
        _allFileRows.Clear();
        _filesByOutcome.Clear();
        _recByOutcome.Clear();
        _recommendations.Clear();
        _largePlanCategories.Clear();
        Sankey.SetPlan(null);
        TreeDiff.SetPlan(null);
        KeepValue.Text = "0";
        KeepHint.Text = string.Empty;
        TidyValue.Text = "0";
        TidyHint.Text = string.Empty;
        ReorgValue.Text = "0";
        ReorgHint.Text = string.Empty;
        SankeyHeroStat.Text = string.Empty;
        StatHero.Visibility = Visibility.Collapsed;
        ViewModeToggle.Visibility = Visibility.Collapsed;
        UnifiedSurface.Visibility = Visibility.Collapsed;
        LargePlanCard.Visibility = Visibility.Collapsed;
        LargePlanAutoCount.Text = "—";
        LargePlanReviewCount.Text = "—";
        LargePlanAskCount.Text = "—";
        LargePlanConfidenceDetailText.Text = string.Empty;
        NothingToMoveCard.Visibility = Visibility.Collapsed;
        StayingPutCard.Visibility = Visibility.Collapsed;
        ApplyBarSelectedCount.Text = "0";
        ApplyBarTotalCount.Text = "0";
        ApplyBarOfText.Visibility = Visibility.Visible;
        ApplyBarTotalCount.Visibility = Visibility.Visible;
        ApplyBarSelectionLabel.Text = "selected";
        ApplyBarHint.Text = "Generate a plan to enable Apply.";
        ApplySymlinkButtonText.Text = "Create shortcuts";
        ApplyMovesButtonText.Text = "Convert to real moves";
        ApplySymlinkButton.IsEnabled = false;
        ApplyMovesButton.IsEnabled = false;
        CancelApplyButton.Visibility = Visibility.Collapsed;
        PlanStatusText.Text = string.IsNullOrEmpty(AppViewModel.Instance.FolderPath)
            ? "Pick a library folder in the sidebar to plan a reorganization."
            : "No current plan for this library — generate a new plan.";
    }

    private void AddRec(RestructureOutcome outcome, string headline, string body,
                        int fileCount, int folderCount, bool informational)
    {
        var vm = new RestructureRecommendationVm
        {
            Outcome = outcome,
            Headline = headline,
            BodyText = body,
            FileCount = fileCount,
            FolderCount = folderCount,
            IsInformational = informational,
            MatchedCount = informational ? 0 : CountOf(outcome),
        };
        if (!informational && _filesByOutcome.TryGetValue(outcome, out var files))
        {
            foreach (var f in files.Take(InlineFileCap)) vm.Files.Add(f);
        }
        _recommendations.Add(vm);
        _recByOutcome[outcome] = vm;
    }

    private int CountOf(RestructureOutcome outcome)
        => _filesByOutcome.TryGetValue(outcome, out var list) ? list.Count : 0;

    private int DistinctSourceFolders(RestructureOutcome outcome)
    {
        if (!_filesByOutcome.TryGetValue(outcome, out var list)) return 0;
        return list.Select(f => System.IO.Path.GetDirectoryName(f.Move.Source) ?? "")
                   .Distinct(StringComparer.OrdinalIgnoreCase).Count();
    }

    private static int DistinctAllSourceFolders(RestructurePlan plan)
        => plan.Moves.Select(m => System.IO.Path.GetDirectoryName(m.Source) ?? "")
                     .Distinct(StringComparer.OrdinalIgnoreCase).Count();

    private void UpdateStayingPut(int keepFolders)
    {
        StayingPutCard.Visibility = keepFolders > 0 ? Visibility.Visible : Visibility.Collapsed;
        StayingPutSubtitle.Text = keepFolders == 1 ? "1 folder kept intact" : $"{keepFolders:N0} folders kept intact";
    }

    // ---- Selection ------------------------------------------------------

    private void OnFileSelectionChanged()
    {
        if (_suppressRecompute) return;
        RecomputeSelection();
    }

    /// <summary>Recompute the apply count + button state from the per-file
    /// IsSelected flags, and reconcile each card's approve state. The count and
    /// the move set ApplyAsync sends both read the same _allFileRows, so they
    /// can never diverge (the macOS toggleSkip invariant).</summary>
    internal static T? ResolveRepeaterItem<T>(object? itemsSource, int index)
        where T : class
    {
        if (index < 0) return null;
        if (itemsSource is IReadOnlyList<T> readOnly && index < readOnly.Count)
        {
            return readOnly[index];
        }
        if (itemsSource is IList<T> list && index < list.Count)
        {
            return list[index];
        }
        return null;
    }

    private void OnRecommendationElementPrepared(
        ItemsRepeater sender,
        ItemsRepeaterElementPreparedEventArgs args)
        => DebugLog.SafeRun(nameof(OnRecommendationElementPrepared), () =>
        {
            if (args.Element is not FrameworkElement element) return;
            element.DataContext =
                ResolveRepeaterItem<RestructureRecommendationVm>(sender.ItemsSource, args.Index);
        });

    private void OnFileElementPrepared(
        ItemsRepeater sender,
        ItemsRepeaterElementPreparedEventArgs args)
        => DebugLog.SafeRun(nameof(OnFileElementPrepared), () =>
        {
            if (args.Element is not FrameworkElement element) return;
            element.DataContext =
                ResolveRepeaterItem<RestructureFileRowVm>(sender.ItemsSource, args.Index);
        });

    private void RecomputeSelection()
    {
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan?.Truncated == true)
        {
            var storedTotal = RestructurePlanPresentation.TotalMoves(plan);
            var hasCompleteConfidenceCounts =
                RestructurePlanPresentation.TryGetCompleteConfidenceCounts(
                    plan,
                    out var confidenceCounts);
            bool storedHasWork = RestructurePlanPresentation.CanApplyStoredPlan(
                plan,
                !_planIntegrityBlocked);
            ApplySymlinkButton.IsEnabled = storedHasWork && !_applying;
            ApplyMovesButton.IsEnabled = storedHasWork && !_applying;
            ApplyBarSelectedCount.Text = hasCompleteConfidenceCounts
                ? confidenceCounts.Auto.ToString("N0")
                : "—";
            ApplyBarOfText.Visibility = Visibility.Collapsed;
            ApplyBarTotalCount.Visibility = Visibility.Collapsed;
            ApplyBarSelectionLabel.Text = "Auto eligible";
            ApplyBarTotalCount.Text = storedTotal.ToString("N0");
            ApplySymlinkButtonText.Text = storedHasWork
                ? "Create Auto shortcuts"
                : "Create shortcuts";
            ApplyMovesButtonText.Text = storedHasWork
                ? "Move Auto files"
                : "Convert to real moves";
            ApplyStatusText.Text = _planIntegrityBlocked
                ? "Apply blocked: the visible sample contains duplicate, invalid, or outside-library paths."
                : !hasCompleteConfidenceCounts
                    ? "Apply blocked: authoritative full-plan confidence totals are unavailable."
                    : string.IsNullOrWhiteSpace(plan.PlanId)
                        ? "Apply blocked: the stored plan handle is unavailable. Generate it again."
                        : confidenceCounts.Auto == 0
                            ? "Nothing can apply automatically. Review and Needs approval proposals stay put; " +
                              "plan a smaller folder to inspect them."
                            : $"Ready to apply {confidenceCounts.Auto:N0} Auto moves into '{plan.LibraryRoot}'. " +
                              $"{confidenceCounts.Review + confidenceCounts.Ask + confidenceCounts.Unknown:N0} " +
                              "Review, Needs approval, or unknown-confidence proposals stay put.";
            ApplyBarHint.Text = _planIntegrityBlocked
                ? "Generate a fresh plan before applying"
                : hasCompleteConfidenceCounts
                    ? "Full engine preflight immediately before first change · Auto only"
                    : "Generate a fresh plan before applying";
            return;
        }
        ApplyBarOfText.Visibility = Visibility.Visible;
        ApplyBarTotalCount.Visibility = Visibility.Visible;
        ApplyBarSelectionLabel.Text = "selected";
        ApplyMovesButtonText.Text = "Convert to real moves";
        int total = plan?.Moves.Count ?? 0;
        int selected = 0;
        foreach (var kv in _filesByOutcome)
        {
            int s = 0;
            foreach (var f in kv.Value) if (f.IsSelected) s++;
            selected += s;
            if (_recByOutcome.TryGetValue(kv.Key, out var rec) && !rec.IsInformational)
            {
                rec.IsApproved = s > 0;
            }
        }

        bool hasWork = selected > 0 && !_planIntegrityBlocked;
        // Single-flight: while an apply is in flight the buttons stay disabled
        // even if a checkbox toggle re-runs this, so a stale plan can't be
        // re-applied (F-C5-003).
        ApplySymlinkButton.IsEnabled = hasWork && !_applying;
        ApplyMovesButton.IsEnabled = hasWork && !_applying;
        ApplyBarSelectedCount.Text = selected.ToString("N0");
        ApplySymlinkButtonText.Text = hasWork ? $"Create shortcuts ({selected:N0})" : "Create shortcuts";
        ApplyStatusText.Text = _planIntegrityBlocked
            ? "Apply blocked: duplicate, invalid, or outside-library paths were detected."
            : hasWork
                ? $"Ready to apply {selected:N0} of {total:N0} into '{plan?.LibraryRoot}'."
                : "Select at least one file to apply.";
        ApplyBarHint.Text = _planIntegrityBlocked
            ? "Generate a fresh plan before applying"
            : total > 0
                ? "Shortcuts add links (manual removal) · Real moves use one-click Undo"
                : "Generate a plan to enable Apply.";
        StepChip1Bg.Background = hasWork
            ? FileID.Services.ThemeHelper.GetBrushSafe("GoldBrush")
            : new SolidColorBrush(Windows.UI.Color.FromArgb(0x44, 0xFF, 0xCC, 0x00));
    }

    private void OnFileCheckClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnFileCheckClicked), () =>
        {
            if (sender is CheckBox cb && cb.DataContext is RestructureFileRowVm f)
            {
                SetFileSelection(f, cb.IsChecked == true);
            }
        });

    private void SetFileSelection(RestructureFileRowVm file, bool isSelected)
    {
        if (isSelected)
        {
            _deselectedFileIds.Remove(file.FileId);
            _selectedFileIds.Add(file.FileId);
        }
        else
        {
            _selectedFileIds.Remove(file.FileId);
            _deselectedFileIds.Add(file.FileId);
        }
        file.IsSelected = isSelected;
    }

    private void OnRecReviewClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnRecReviewClicked), () =>
        {
            if ((sender as FrameworkElement)?.DataContext is RestructureRecommendationVm vm)
            {
                vm.IsExpanded = !vm.IsExpanded;
            }
        });

    private void OnRecApproveClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnRecApproveClicked), () =>
        {
            if ((sender as FrameworkElement)?.DataContext is not RestructureRecommendationVm vm) return;
            bool approve = !vm.IsApproved;
            if (_filesByOutcome.TryGetValue(vm.Outcome, out var files))
            {
                _suppressRecompute = true;
                foreach (var f in files)
                {
                    SetFileSelection(f, approve);
                }
                _suppressRecompute = false;
            }
            RecomputeSelection();
        });

    private async void OnSeeAllClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnSeeAllClicked), async () =>
        {
            if ((sender as FrameworkElement)?.DataContext is not RestructureRecommendationVm vm)
            {
                return;
            }
            var plan = _renderedPlan;
            var livePlan = EngineClient.Instance.LastRestructurePlan;
            var hasCurrentCard = _recByOutcome.TryGetValue(vm.Outcome, out var currentVm)
                                 && IsCurrentRecommendation(vm, currentVm);
            if (!IsFrozenPlanCurrent(plan, livePlan, _renderedPlan) || !hasCurrentCard)
            {
                SyncPlan();
                ApplyStatusText.Text =
                    "The plan changed before the file details opened — review the updated plan.";
                await ShowAlertAsync(
                    "Plan updated",
                    "The reorganization plan changed while you were reviewing it. " +
                    "The updated recommendations are on screen now; open See all again.");
                return;
            }
            if (!_filesByOutcome.TryGetValue(vm.Outcome, out var rows))
            {
                return;
            }
            var title = vm.Outcome switch
            {
                RestructureOutcome.Tidy => "Tidying — files moving out of mixed folders",
                RestructureOutcome.Reorganize => "Reorganizing — files leaving generic folders",
                _ => "Files staying put",
            };
            var sheet = new DrillDownSheet();
            var staleDetailSurfaced = false;
            sheet.SetOutcomeFilter(
                rows,
                title,
                (row, isSelected) =>
                {
                    var stillCurrent =
                        IsFrozenPlanCurrent(
                            plan,
                            EngineClient.Instance.LastRestructurePlan,
                            _renderedPlan)
                        && _recByOutcome.TryGetValue(vm.Outcome, out var renderedVm)
                        && IsCurrentRecommendation(vm, renderedVm);
                    if (stillCurrent)
                    {
                        SetFileSelection(row, isSelected);
                        return;
                    }
                    if (staleDetailSurfaced) return;
                    staleDetailSurfaced = true;
                    SyncPlan();
                    ApplyStatusText.Text =
                        "The plan changed while file details were open — review the updated plan.";
                    _ = ShowAlertAsync(
                        "Plan updated",
                        "That file list belonged to an older reorganization plan, so the selection was not changed. " +
                        "Close it and open See all from the updated recommendation.");
                });
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = "Files in this group",
                Content = sheet,
                CloseButtonText = "Done",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        });

    // ---- Hover cross-highlight ------------------------------------------

    private void OnKeepTileEntered(object sender, PointerRoutedEventArgs e) => SetHoveredOutcome(RestructureOutcome.Keep);
    private void OnTidyTileEntered(object sender, PointerRoutedEventArgs e) => SetHoveredOutcome(RestructureOutcome.Tidy);
    private void OnReorgTileEntered(object sender, PointerRoutedEventArgs e) => SetHoveredOutcome(RestructureOutcome.Reorganize);
    private void OnTileExited(object sender, PointerRoutedEventArgs e) => SetHoveredOutcome(null);

    private void OnRecPointerEntered(object sender, PointerRoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnRecPointerEntered), () =>
        {
            if ((sender as FrameworkElement)?.DataContext is RestructureRecommendationVm vm)
                SetHoveredOutcome(vm.Outcome);
        });

    private void OnRecPointerExited(object sender, PointerRoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnRecPointerExited), () => SetHoveredOutcome(null));

    private void SetHoveredOutcome(RestructureOutcome? outcome)
    {
        if (_hovered == outcome) return;
        _hovered = outcome;
        foreach (var rec in _recommendations)
        {
            rec.IsHighlighted = outcome != null && rec.Outcome == outcome.Value;
        }
        UpdateTileHighlight(KeepTile, RestructureOutcome.Keep, _keepBrush);
        UpdateTileHighlight(TidyTile, RestructureOutcome.Tidy, _tidyBrush);
        UpdateTileHighlight(ReorgTile, RestructureOutcome.Reorganize, _reorgBrush);
    }

    private void UpdateTileHighlight(Border tile, RestructureOutcome outcome, Brush tint)
    {
        bool active = _hovered == outcome;
        tile.BorderBrush = active ? tint : _idleTileStroke;
        FileID.Theme.Motion.SpringEasing.AnimateScale(tile, active ? 1.012f : 1.0f, 0.28, 0.7);
    }

    // ---- Flow / Tree toggle ---------------------------------------------

    private void OnViewModeClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnViewModeClicked), () =>
        {
            if (Sankey is null || TreeDiff is null || VisualizationHeader is null) return;
            bool tree = ReferenceEquals(sender, TreeModeToggle);
            FlowModeToggle.IsChecked = !tree;
            TreeModeToggle.IsChecked = tree;
            Sankey.Visibility = tree ? Visibility.Collapsed : Visibility.Visible;
            TreeDiff.Visibility = tree ? Visibility.Visible : Visibility.Collapsed;
            VisualizationHeader.Text = tree ? "Current vs proposed tree" : "Folder map";
        });

    // ---- Deep Analyze nudge ---------------------------------------------

    private async Task RefreshDeepAnalyzeHintAsync()
    {
        if (EngineClient.Instance.DeepAnalyzeCommandInFlight) return;
        var libraryRoot = AppViewModel.Instance.FolderPath;
        if (string.IsNullOrWhiteSpace(libraryRoot))
        {
            MissingContentModelsBanner.Visibility = Visibility.Collapsed;
            DeepAnalyzeHintBanner.Visibility = Visibility.Collapsed;
            return;
        }
        var stats = default(RestructureQualityStats);
        try
        {
            stats = await Task.Run(
                () => QueryRestructureQuality(libraryRoot)).ConfigureAwait(true);
        }
        catch { }

        if (_unloaded
            || !RootsMatch(libraryRoot, AppViewModel.Instance.FolderPath))
        {
            return;
        }
        bool missingContentSignals = stats.Available
            && RestructurePlanPresentation.HasMissingContentSignals(
                stats.ContentEligible,
                stats.ClipEmbeddings,
                stats.TextEmbeddings);
        bool show = !_deepAnalyzeHintDismissed
            && !missingContentSignals
            && stats.Total > 0
            && (double)stats.Captioned / stats.Total < 0.4
            && !EngineClient.Instance.DeepAnalyzeCommandInFlight;
        try
        {
            MissingContentModelsBanner.Visibility =
                missingContentSignals ? Visibility.Visible : Visibility.Collapsed;
            DeepAnalyzeHintBanner.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Deep Analyze hint update threw (view unloaded?): " + ex.Message);
        }
    }

    private void UpdateDeepAnalyzeBanner()
    {
        var engine = EngineClient.Instance;
        if (engine.DeepAnalyzeCommandInFlight)
        {
            DeepAnalyzeHintBanner.Visibility = Visibility.Visible;
            DeepAnalyzeHintTitle.Text = engine.DeepAnalyzeProgress is null
                ? "Deep Analyze preparing..."
                : "Deep Analyze running...";
            DeepAnalyzeHintBody.Text = "Analyzing your library - proposals will sharpen as it runs.";
            RunDeepAnalyzeButton.IsEnabled = false;
        }
        else
        {
            DeepAnalyzeHintTitle.Text = "Sharper proposals with Deep Analyze";
            DeepAnalyzeHintBody.Text = "Deep Analyze reads the contents of each file - captions, OCR text, scene tags - so receipts go to Documents, screenshots to Photos, and people are recognized by name.";
            RunDeepAnalyzeButton.IsEnabled = true;
            _ = RefreshDeepAnalyzeHintAsync();
        }
    }

    internal readonly record struct RestructureQualityStats(
        bool Available,
        int Captioned,
        int Total,
        int ContentEligible,
        int ClipEmbeddings,
        int TextEmbeddings);

    private static RestructureQualityStats QueryRestructureQuality(
        string libraryRoot)
    {
        try
        {
            if (!System.IO.File.Exists(AppPaths.DbPath)
                || string.IsNullOrWhiteSpace(libraryRoot))
            {
                return default;
            }
            var normalizedRoot = libraryRoot.TrimEnd('\\', '/');
            using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                {
                    DataSource = AppPaths.DbPath,
                    Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                }.ToString());
            conn.Open();
            return QueryRestructureQuality(conn, normalizedRoot);
        }
        catch { return default; }
    }

    internal static RestructureQualityStats QueryRestructureQuality(
        Microsoft.Data.Sqlite.SqliteConnection conn,
        string libraryRoot)
    {
        var normalizedRoot = libraryRoot.TrimEnd('\\', '/');
        using var cmd = conn.CreateCommand();
        const string deepEligible =
            "(kind IN ('image', 'video', 'pdf', 'audio')"
            + " OR (kind = 'model' AND lower(path_text) LIKE '%.obj'))";
        const string textEligible =
            "(kind IN ('doc', 'pdf') AND EXISTS ("
            + "SELECT 1 FROM doc_text dt WHERE dt.file_id = scoped.id))";
        const string embeddingEligible =
            "(kind IN ('image', 'video')"
            + " OR (kind = 'model' AND lower(path_text) LIKE '%.obj')"
            + " OR " + textEligible + ")";
        cmd.CommandText =
            "WITH scoped AS (" +
            " SELECT id, kind, path_text, vlm_full_model FROM files WHERE failed = 0 AND (" +
            "   path_text = $root COLLATE NOCASE OR (" +
            "     substr(path_text, 1, length($root)) = $root COLLATE NOCASE" +
            "     AND substr(path_text, length($root) + 1, 1) IN ('\\', '/')" +
            "   )" +
            " )" +
            ")" +
            " SELECT SUM(CASE WHEN " + deepEligible + " THEN 1 ELSE 0 END)," +
            " SUM(CASE WHEN " + deepEligible +
            "   AND vlm_full_model IS NOT NULL AND vlm_full_model <> '' THEN 1 ELSE 0 END)," +
            " SUM(CASE WHEN " + embeddingEligible + " THEN 1 ELSE 0 END)," +
            " SUM(CASE WHEN (kind IN ('image', 'video')" +
            "   OR (kind = 'model' AND lower(path_text) LIKE '%.obj')) AND EXISTS (" +
            "   SELECT 1 FROM clip_embeddings ce WHERE ce.file_id = scoped.id" +
            " ) THEN 1 ELSE 0 END)," +
            " SUM(CASE WHEN " + textEligible + " AND EXISTS (" +
            "   SELECT 1 FROM text_embeddings te WHERE te.file_id = scoped.id" +
            " ) THEN 1 ELSE 0 END)" +
            " FROM scoped";
        cmd.Parameters.AddWithValue("$root", normalizedRoot);
        using var reader = cmd.ExecuteReader();
        if (!reader.Read()) return default;

        int total = reader.IsDBNull(0) ? 0 : (int)Math.Min(Convert.ToInt64(reader.GetValue(0)), int.MaxValue);
        int captioned = reader.IsDBNull(1) ? 0 : (int)Math.Min(Convert.ToInt64(reader.GetValue(1)), int.MaxValue);
        int eligible = reader.IsDBNull(2) ? 0 : (int)Math.Min(Convert.ToInt64(reader.GetValue(2)), int.MaxValue);
        int clip = reader.IsDBNull(3) ? 0 : (int)Math.Min(Convert.ToInt64(reader.GetValue(3)), int.MaxValue);
        int text = reader.IsDBNull(4) ? 0 : (int)Math.Min(Convert.ToInt64(reader.GetValue(4)), int.MaxValue);
        return new RestructureQualityStats(true, captioned, total, eligible, clip, text);
    }

    private async void OnRunDeepAnalyzeClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnRunDeepAnalyzeClicked), async () =>
        {
            if (EngineClient.Instance.DeepAnalyzeCommandInFlight) return;
            var model = AppViewModel.Instance.Settings.SelectedVlmModelKind;
            try
            {
                await EngineClient.Instance.DeepAnalyzeAllAsync(model, skipExisting: true);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("Restructure Deep Analyze start failed: " + ex.Message);
                await ShowAlertAsync("Couldn't start Deep Analyze", ex.Message);
            }
        });

    private void OnDismissHintClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnDismissHintClicked), () =>
        {
            _deepAnalyzeHintDismissed = true;
            DeepAnalyzeHintBanner.Visibility = Visibility.Collapsed;
        });

    private void OnOpenModelsSettingsClicked(object sender, RoutedEventArgs e)
        => AppViewModel.Instance.ActiveTab = SidebarTab.Settings;

    private void BeginOrUpdatePlanningState(string folder)
    {
        if (_planning)
        {
            _planningRoot = folder;
            _deselectedFileIds.Clear();
            _selectedFileIds.Clear();
            PlanBusyIndicator.IsActive = true;
            PlanBusyIndicator.Visibility = Visibility.Visible;
            ReplanButton.IsEnabled = false;
            PlanStatusText.Text = "Updating the plan with the latest library inputs…";
            return;
        }

        _planning = true;
        _planningRoot = folder;
        _planningSpawnGen = EngineClient.Instance.SpawnGeneration;
        _deselectedFileIds.Clear();
        _selectedFileIds.Clear();
        PlanBusyIndicator.IsActive = true;
        PlanBusyIndicator.Visibility = Visibility.Visible;
        ReplanButton.IsEnabled = false;
        PlanStatusText.Text = "Computing plan…";
    }

    private void CompletePlanningState()
    {
        _planning = false;
        _planningRoot = null;
        PlanBusyIndicator.IsActive = false;
        PlanBusyIndicator.Visibility = Visibility.Collapsed;
        ReplanButton.IsEnabled = !_applying;
    }

    private async Task<bool> RequestPlanForFolderAsync(string folder, string origin)
    {
        BeginOrUpdatePlanningState(folder);

        try
        {
            await EngineClient.Instance.PlanRestructureAsync(folder);
            return true;
        }
        catch (Exception ex)
        {
            CompletePlanningState();
            DebugLog.Warn($"PlanRestructure ({origin}) send failed: {ex.Message}");
            PlanStatusText.Text =
                "Couldn't start planning — the engine isn't responding. Try restarting the app.";
            return false;
        }
    }

    private async Task ReplanAfterApplyAsync(string folder)
    {
        if (await RequestPlanForFolderAsync(folder, "post-apply")) return;

        _applying = false;
        if (!_unloaded) RecomputeSelection();
    }

    // ---- Plan / Apply ---------------------------------------------------

    private async void OnPlanClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnPlanClicked), async () =>
        {
            var folder = AppViewModel.Instance.FolderPath;
            if (string.IsNullOrEmpty(folder))
            {
                PlanStatusText.Text = "Pick a library folder in the sidebar first.";
                return;
            }
            await RequestPlanForFolderAsync(folder, "Re-plan");
        });

    private async void OnApplySymlinksClicked(object sender, RoutedEventArgs e) => await ApplyAsync(useSymlinks: true);

    private async void OnApplyMovesClicked(object sender, RoutedEventArgs e) => await ApplyAsync(useSymlinks: false);

    private async Task ApplyAsync(bool useSymlinks)
    {
        // Single-flight: re-clicking after an apply re-runs against a now-stale
        // plan (B4 reports every file failed -> a false "some changes couldn't be
        // applied" alarm). Guard + disable until the result (and re-plan) land.
        if (_applying) return;
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan is null) return;
        RestructureConfidenceCounts? fullConfidenceCounts = null;
        if (plan.Truncated)
        {
            if (!RestructurePlanPresentation.TryGetCompleteConfidenceCounts(
                    plan,
                    out fullConfidenceCounts))
            {
                ApplyStatusText.Text =
                    "Apply blocked: authoritative full-plan confidence totals are unavailable.";
                await ShowAlertAsync(
                    "Generate a fresh plan",
                    "This stored plan cannot prove how many Auto, Review, and Needs approval proposals it contains. " +
                    "Nothing was changed. Generate a fresh plan before applying.");
                return;
            }
            if (fullConfidenceCounts.Auto == 0)
            {
                ApplyStatusText.Text =
                    "There are no Auto-confidence proposals to apply. Plan a smaller folder for per-file review.";
                return;
            }
        }
        else if (plan.Moves.Count == 0)
        {
            return;
        }
        if (_planIntegrityBlocked)
        {
            ApplyStatusText.Text =
                "Apply blocked: generate a fresh plan that passes path and collision safety checks.";
            await ShowAlertAsync(
                "Plan failed safety checks",
                "FileID found duplicate, invalid, or outside-library paths in this plan. " +
                "Nothing was changed. Generate a fresh plan before applying.");
            return;
        }

        // CRITICAL: hard guard — the plan's own library root MUST equal the
        // currently active library. If the folder was switched/cleared/wiped
        // since planning (and the invalidation-on-change signal was somehow
        // missed), applying would move files in the OLD folder the user has
        // moved on from. Refuse, drop the stale plan, and require a re-plan.
        if (!RootsMatch(plan.LibraryRoot, AppViewModel.Instance.FolderPath))
        {
            EngineClient.Instance.InvalidateRestructurePlan();
            SyncPlan();
            ApplyStatusText.Text = "The active folder changed since this plan was made — re-plan for the current library.";
            _ = ShowAlertAsync("Folder changed",
                "This reorganization plan was made for a different folder than the one now open. Re-plan for the current library before applying.");
            return;
        }

        // H2: the on-screen rows were rendered from _renderedPlan. If a background
        // re-plan has since replaced LastRestructurePlan (EngineClient sets it
        // synchronously; SyncPlan only re-renders on a later dispatcher turn), the
        // reviewed rows no longer match the live plan — applying now would send
        // moves the user never saw. Refuse, re-render the current plan, and make
        // the user re-review + re-click. This also guards the truncated
        // apply-by-plan_id path: a plan_id whose rendered plan changed is stale.
        if (!ReferenceEquals(plan, _renderedPlan))
        {
            SyncPlan();
            ApplyStatusText.Text = "The plan changed since you reviewed it — review the updated moves, then apply.";
            _ = ShowAlertAsync("Plan updated",
                "The reorganization plan changed while you were reviewing it. The updated moves are on screen now — review them, then apply again.");
            return;
        }

        // Build the move set from the REVIEWED rows themselves — the exact Move
        // records rendered on screen — not by re-reading the live plan's Moves.
        // The two are the same set here (the reference check above proved plan ==
        // _renderedPlan), but sourcing from the rows makes the applied set, by
        // construction, precisely what the user approved.
        var sel = new List<RestructureMove>();
        if (!plan.Truncated)
        {
            foreach (var row in _allFileRows.Values)
            {
                if (row.IsSelected) sel.Add(row.Move);
            }
        }
        if (plan.Truncated && string.IsNullOrWhiteSpace(plan.PlanId))
        {
            ApplyStatusText.Text =
                "Apply blocked: the engine no longer has this stored plan. Generate it again.";
            await ShowAlertAsync(
                "Generate the plan again",
                "The bounded plan no longer has a valid engine handle. Nothing was changed.");
            return;
        }
        if (!plan.Truncated && sel.Count == 0) return;

        if (!useSymlinks)
        {
            var confirmed = await ConfirmRealMovesAsync(
                plan,
                plan.Truncated ? fullConfidenceCounts!.Auto : (ulong)sel.Count);
            if (!confirmed) return;
            if (_applying) return;
            if (!IsFrozenPlanCurrent(
                    plan,
                    EngineClient.Instance.LastRestructurePlan,
                    _renderedPlan) ||
                !RootsMatch(plan.LibraryRoot, AppViewModel.Instance.FolderPath))
            {
                SyncPlan();
                ApplyStatusText.Text =
                    "The plan changed while confirmation was open — review it and try again.";
                await ShowAlertAsync(
                    "Plan updated",
                    "The plan changed before FileID could apply the frozen reviewed set. " +
                    "Nothing was moved. Review the updated plan and confirm again.");
                return;
            }
        }

        _applying = true;
        _applyInFlight = true;
        _applyingAsShortcuts = useSymlinks;
        _applyingPlan = plan;   // R6-04: record the in-flight plan (see SyncPlan)
        _applyingSpawnGen = EngineClient.Instance.SpawnGeneration; // R6-06
        ApplySymlinkButton.IsEnabled = false;
        ApplyMovesButton.IsEnabled = false;
        ReplanButton.IsEnabled = false;
        CancelApplyButton.IsEnabled = true;
        CancelApplyButton.Visibility = Visibility.Visible;
        ApplyStatusText.Text = plan.Truncated
            ? useSymlinks
                ? $"Creating {fullConfidenceCounts!.Auto:N0} Auto shortcuts after full preflight…"
                : $"Moving {fullConfidenceCounts!.Auto:N0} Auto files after full preflight…"
            : useSymlinks
                ? $"Creating {sel.Count:N0} shortcuts…"
                : $"Moving {sel.Count:N0} files…";
        try
        {
            await EngineClient.Instance.ApplyRestructureAsync(
                plan.LibraryRoot, sel, useSymlinks, plan.Truncated ? plan.PlanId : null);
        }
        catch (Exception ex)
        {
            // SendCommandAsync can throw if the engine pipe is dead. Without this
            // the status freezes on "Moving N files..." (the apply-result event
            // never arrives). Surface it instead of a silent hang.
            DebugLog.Warn("ApplyRestructure send failed: " + ex.Message);
            _applyInFlight = false;
            _applying = false;
            _applyingAsShortcuts = false;
            CancelApplyButton.Visibility = Visibility.Collapsed;
            ReplanButton.IsEnabled = true;
            RecomputeSelection();
            ApplyStatusText.Text = "Couldn't apply - the engine isn't responding. Try restarting the app.";
            await ShowAlertAsync("Couldn't apply changes",
                "FileID couldn't tell the engine to apply your reorganization (it isn't responding). " +
                "Your files were not touched. Try restarting the app, then apply again.");
        }
    }

    // R2 reversibility: show/hide the "Undo last run" button from the engine's
    // CanUndoRestructure flag (set after an apply that moved files, cleared once
    // undone). Mirrors macOS RestructureView's canUndoRestructure affordance.
    private async Task<bool> ConfirmRealMovesAsync(
        RestructurePlan plan,
        ulong selectedCount)
    {
        if (XamlRoot is null)
        {
            DebugLog.Warn("Restructure confirmation skipped because XamlRoot is null.");
            return false;
        }

        var title =
            $"Move {selectedCount:N0} file{(selectedCount == 1 ? "" : "s")}?";
        var rootScope =
            $"The selected library root is '{plan.LibraryRoot}'. " +
            (RestructurePlanPresentation.IsDriveRoot(plan.LibraryRoot)
                ? "This is a drive root, so FileID may create or reorganize top-level folders across " +
                  "the drive. Cancel and choose a narrower folder if you want finer review. "
                : string.Empty);
        var message = plan.Truncated
            ? rootScope +
              $"FileID will move exactly {selectedCount:N0} Auto-confidence " +
              $"file{(selectedCount == 1 ? "" : "s")} from the frozen stored plan. " +
              "The visible sample does not validate the unseen plan. Immediately before the first move, " +
              "the engine preflights every stored proposal; if that full preflight fails, nothing moves. " +
              "Review, Needs approval, and unknown-confidence proposals stay put. " +
              "Every completed move is journaled and Undo last run can reverse the batch."
            : rootScope +
              $"FileID will move the exact {selectedCount:N0} reviewed " +
              $"file{(selectedCount == 1 ? "" : "s")} now. " +
              "Every completed move is journaled and Undo last run can reverse the batch.";

        try
        {
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = title,
                Content = message,
                PrimaryButtonText = "Move files",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Close,
            };
            return await dialog.ShowAsync() == ContentDialogResult.Primary;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Restructure confirmation failed: " + ex.Message);
            return false;
        }
    }

    private async void OnCancelApplyClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnCancelApplyClicked), async () =>
        {
            if (!_applyInFlight) return;

            CancelApplyButton.IsEnabled = false;
            ApplyStatusText.Text =
                "Stopping safely after the current file… completed moves remain undoable.";
            try
            {
                await EngineClient.Instance.CancelRestructureApplyAsync();
            }
            catch (Exception ex)
            {
                CancelApplyButton.IsEnabled = true;
                ApplyStatusText.Text = "Couldn't send the stop request — the engine isn't responding.";
                DebugLog.Warn("Cancel restructure apply send failed: " + ex.Message);
                await ShowAlertAsync(
                    "Couldn't stop applying",
                    "FileID couldn't reach the engine. Completed moves are still journaled; " +
                    "use Undo last run after the operation finishes or the engine restarts.");
            }
        });

    private void SyncUndoAffordance()
    {
        var canUndo = EngineClient.Instance.CanUndoRestructure
            && RootsMatch(EngineClient.Instance.UndoRestructureRoot, AppViewModel.Instance.FolderPath);
        UndoButton.Visibility = canUndo ? Visibility.Visible : Visibility.Collapsed;
        UndoButton.IsEnabled = CanStartRestructureUndo(
            canUndo,
            Services.ChangeLog.Instance.IsUndoInFlight,
            EngineClient.Instance.UndoRestructureInFlight);
    }

    private async void OnUndoClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnUndoClicked), async () =>
        {
            var canUndoForRoot = EngineClient.Instance.CanUndoRestructure
                && RootsMatch(
                    EngineClient.Instance.UndoRestructureRoot,
                    AppViewModel.Instance.FolderPath);
            if (!CanStartRestructureUndo(
                    canUndoForRoot,
                    Services.ChangeLog.Instance.IsUndoInFlight,
                    EngineClient.Instance.UndoRestructureInFlight))
            {
                SyncUndoAffordance();
                return;
            }

            var root = EngineClient.Instance.UndoRestructureRoot
                       ?? EngineClient.Instance.LastRestructurePlan?.LibraryRoot
                       ?? AppViewModel.Instance.FolderPath;
            if (string.IsNullOrEmpty(root)) return;
            UndoButton.IsEnabled = false;
            ApplyStatusText.Text = "Undoing the last restructure…";
            try
            {
                // Route through the session change log entry when one exists so
                // the changes sheet shows this apply as Undone (the engine
                // journal persists across launches, so the log may be empty —
                // fall back to the direct command then).
                var logEntry = FindLatestRestructureUndoEntry(
                    Services.ChangeLog.Instance.Snapshot());
                if (logEntry is not null
                    && string.IsNullOrWhiteSpace(
                        EngineClient.Instance.UndoRestructureShortcutToken))
                {
                    bool undone;
                    if (logEntry.Status == Services.ChangeStatus.UndoFailed)
                    {
                        undone = await Services.ChangeLog.Instance.RetryAsync(logEntry);
                    }
                    else
                    {
                        undone = await Services.ChangeLog.Instance.UndoAsync(logEntry);
                    }
                    if (!undone)
                    {
                        SyncUndoAffordance();
                        if (Services.ChangeLog.Instance.IsUndoInFlight)
                        {
                            ApplyStatusText.Text =
                                "Another undo is still running — try again when it finishes.";
                        }
                        else
                        {
                            ApplyStatusText.Text =
                                "Undo didn't complete — fix the reported issue and try Undo again.";
                        }
                    }
                }
                else
                {
                    await EngineClient.Instance.UndoRestructureAsync(
                        root!,
                        EngineClient.Instance.UndoRestructureShortcutToken);
                }
            }
            catch (Exception ex)
            {
                // A faulted send (engine respawning) — re-enable so the button
                // isn't stuck, mirroring the apply path's fault handling.
                DebugLog.Warn("Undo restructure send failed: " + ex.Message);
                ApplyStatusText.Text = "Engine is unavailable — try again in a moment.";
                UndoButton.IsEnabled = true;
            }
        });

    internal static bool CanStartRestructureUndo(
        bool canUndoForRoot,
        bool changeLogUndoInFlight,
        bool engineUndoInFlight)
        => canUndoForRoot && !changeLogUndoInFlight && !engineUndoInFlight;

    internal static Services.ChangeLogEntry? FindLatestRestructureUndoEntry(
        IEnumerable<Services.ChangeLogEntry> entries)
        => entries.FirstOrDefault(entry =>
            entry.Kind == Services.ChangeKind.Restructure
            && entry.Status is Services.ChangeStatus.Undoable
                or Services.ChangeStatus.UndoFailed);

    private void SyncApplyResult()
    {
        var r = EngineClient.Instance.LastRestructureApplyResult;
        if (!IsUnhandledCompletion(r, _lastHandledApplyResult)) return;
        _lastHandledApplyResult = r;

        // M4: an Undo reply lands on the SAME LastRestructureApplyResult slot as an
        // Apply reply. EngineClient captured whether this terminal was an undo (from
        // UndoRestructureInFlight, before clearing it) and paired it with the result,
        // so present "undone" — not "applied" — and don't push a fresh undo-stack
        // entry for an undo. Undo never engages the apply single-flight guard, so
        // this branch leaves _applying / _applyInFlight untouched.
        if (EngineClient.Instance.LastRestructureApplyResultWasUndo)
        {
            var wasShortcutUndo =
                EngineClient.Instance.LastRestructureApplyResultWasShortcutUndo;
            SyncUndoAffordance();
            // The undo moved files back, so the on-screen plan is stale — refresh
            // it, mirroring the post-apply regenerate. Fire-and-forget; log a
            // faulted send instead of stranding (PlanRestructureAsync faults its
            // Task rather than throwing synchronously).
            var undoFolder = AppViewModel.Instance.FolderPath;
            if (!wasShortcutUndo
                && r.Applied > 0
                && !string.IsNullOrEmpty(undoFolder))
            {
                _ = RequestPlanForFolderAsync(undoFolder!, "post-undo");
            }
            if (!string.IsNullOrEmpty(r.PrivilegeError))
            {
                ApplyStatusText.Text = r.PrivilegeError;
                _ = ShowAlertAsync("Couldn't undo changes", r.PrivilegeError!);
                return;
            }
            ApplyStatusText.Text = FormatUndoCompletion(r, wasShortcutUndo);
            if (r.Cancelled)
            {
                _ = ShowAlertAsync(
                    "Undo stopped",
                    FormatUndoCompletion(r, wasShortcutUndo));
            }
            else if (r.Failed > 0)
            {
                _ = ShowAlertAsync(
                    wasShortcutUndo
                        ? "Some shortcuts couldn't be removed"
                        : "Some items couldn't be restored",
                    wasShortcutUndo
                        ? $"Removed {r.Applied:N0}, but {r.Failed:N0} shortcuts couldn't be safely removed. " +
                          "Check that the links still point to the original files, then retry from Recent Changes. " +
                          "Check the engine log at %LOCALAPPDATA%\\FileID\\logs\\engine.jsonl."
                        : $"Undo moved back {r.Applied:N0}, but {r.Failed:N0} couldn't be returned to their original location. " +
                          "Close any app using those files or restore the missing folder, then click Undo again. " +
                          "Check the engine log at %LOCALAPPDATA%\\FileID\\logs\\engine.jsonl.");
            }
            return;
        }

        var appliedAsShortcuts = _applyingAsShortcuts;
        _applyingAsShortcuts = false;
        _applyInFlight = false;
        CancelApplyButton.Visibility = Visibility.Collapsed;

        // Anything actually moved -> the current plan is stale (real moves
        // updated the DB; applied rows must leave the view). Re-plan, exactly as
        // macOS applySelected() does via regenerate(), and KEEP the single-flight
        // guard engaged across the re-plan so the stale plan can't be re-applied
        // in the gap before the fresh one lands (the F-C5-003 false-alarm window);
        // SyncPlan releases the guard when the new plan arrives. When nothing
        // moved, release it now so the user can retry.
        if (r.Applied > 0)
        {
            EngineClient.Instance.InvalidateRestructurePlan();

            var undoRoot = _applyingPlan?.LibraryRoot
                           ?? EngineClient.Instance.LastRestructurePlan?.LibraryRoot
                           ?? AppViewModel.Instance.FolderPath;
            if (appliedAsShortcuts)
            {
                var shortcutUndoToken = r.ShortcutUndoToken;
                if (!string.IsNullOrWhiteSpace(shortcutUndoToken)
                    && !string.IsNullOrWhiteSpace(undoRoot))
                {
                    Services.UndoStack.Instance.Push(
                        $"create {r.Applied:N0} restructure shortcut{(r.Applied == 1 ? "" : "s")}",
                        Services.ChangeKind.RestructureShortcuts,
                        async () =>
                        {
                            try
                            {
                                return await EngineClient.Instance
                                    .UndoRestructureAndWaitAsync(
                                        undoRoot!,
                                        shortcutUndoToken);
                            }
                            catch (Exception ex)
                            {
                                DebugLog.Warn(
                                    "Undo restructure shortcuts send failed: " +
                                    ex.Message);
                                return false;
                            }
                        });
                }
                else
                {
                    Services.ChangeLog.Instance.RecordNotUndoable(
                        $"create {r.Applied:N0} restructure shortcut{(r.Applied == 1 ? "" : "s")}",
                        Services.ChangeKind.RestructureShortcuts,
                        "The engine did not return a shortcut undo token; remove the links manually.");
                }
            }

            // Session change log: a confirmed apply is undoable via the
            // engine's inverse-move journal. Pushing a Restructure entry
            // auto-marks any older restructure entry "superseded" (the
            // journal is truncate-per-batch — only the latest replays).
            if (!string.IsNullOrEmpty(undoRoot)
                && ShouldRecordUndoableRestructureChange(
                    appliedAsShortcuts,
                    r.Applied,
                    EngineClient.Instance.CanUndoRestructure))
            {
                Services.UndoStack.Instance.Push(
                    $"reorganize {r.Applied:N0} file{(r.Applied == 1 ? "" : "s")}",
                    Services.ChangeKind.Restructure,
                    async () =>
                    {
                        try
                        {
                            return await EngineClient.Instance
                                .UndoRestructureAndWaitAsync(undoRoot!);
                        }
                        catch (Exception ex)
                        {
                            DebugLog.Warn("Undo restructure send failed: " + ex.Message);
                            return false;
                        }
                    });
            }

            var folder = AppViewModel.Instance.FolderPath;
            if (!string.IsNullOrEmpty(folder))
            {
                _ = ReplanAfterApplyAsync(folder!);
            }
            else
            {
                _applying = false;
                RecomputeSelection();
            }
        }
        else
        {
            _applying = false;
            RecomputeSelection();
        }

        if (!string.IsNullOrEmpty(r.PrivilegeError))
        {
            ApplyStatusText.Text = r.PrivilegeError;
            _ = ShowAlertAsync("Couldn't apply changes", r.PrivilegeError!);
            return;
        }
        ApplyStatusText.Text = FormatApplyCompletion(r, appliedAsShortcuts);
        if (r.Cancelled)
        {
            _ = ShowAlertAsync(
                "Restructure stopped",
                FormatApplyCompletion(r, appliedAsShortcuts) +
                "\n\nGenerate a fresh plan before applying again so the proposals match what is now on disk.");
        }
        else if (!appliedAsShortcuts && r.Failed == 0 && r.Applied > 0)
        {
            StepChip2Bg.Background = FileID.Services.ThemeHelper.GetBrushSafe("GoldBrush");
            StepChip2Bg.BorderThickness = new Thickness(0);
        }
        else if (r.Failed > 0)
        {
            // Partial/total failure must be a dismissible, actionable surface -
            // not a status line the user can scroll past thinking it worked.
            // L3: do NOT claim the originals are unchanged — the engine can count a
            // file as failed that WAS physically moved (move succeeded, DB path
            // update failed). Point the user at rescan/Undo to reconcile instead.
            if (appliedAsShortcuts)
            {
                _ = ShowAlertAsync(
                    "Some shortcuts couldn't be created",
                    $"Created {r.Applied:N0}, but {r.Failed:N0} shortcuts failed. Original files stayed in place. " +
                    "This usually means a link already exists or FileID cannot write to that folder. " +
                    "Check %LOCALAPPDATA%\\FileID\\logs\\engine.jsonl, then generate a fresh plan.");
            }
            else
            {
                _ = ShowAlertAsync("Some changes couldn't be applied",
                    $"Applied {r.Applied:N0}, but {r.Failed:N0} couldn't be fully applied. A few of those may have been moved but not fully recorded - " +
                    "run a rescan to reconcile, or Undo to restore.\n\n" +
                    "This usually means a file was open, already moved, or you don't have permission to write the destination. " +
                    "Check the engine log at %LOCALAPPDATA%\\FileID\\logs\\engine.jsonl, then try again.");
            }
        }
    }

    // A plan/apply/undo that dies engine-side surfaces as EngineClient.LastError
    // with a command-specific restructure kind, never as a Plan/ApplyResult event.
    // Without handling it the tab can leave its command affordance disabled forever.
    // Only react to restructure kinds (LastError is a shared slot) and de-dupe.
    private void SyncEngineError()
    {
        var err = EngineClient.Instance.LastError;
        if (err is null || ReferenceEquals(err, _lastHandledError)) return;
        bool planError = IsPlanRestructureErrorKind(err.Kind);
        bool undoError = err.Kind == "undo_restructure";
        if (!planError && !undoError && err.Kind != "apply_restructure") return;
        _lastHandledError = err;

        if (undoError)
        {
            EngineClient.Instance.UndoRestructureInFlight = false;
            EngineClient.Instance.UndoRestructureInFlightWasShortcut = false;
            UndoButton.IsEnabled = EngineClient.Instance.CanUndoRestructure;
            ApplyStatusText.Text = "Undo didn't complete - try again.";
            _ = ShowAlertAsync("Couldn't undo changes",
                string.IsNullOrWhiteSpace(err.Message)
                    ? "FileID couldn't finish undoing the last reorganization."
                    : err.Message);
            return;
        }

        if (!planError)
        {
            _applyInFlight = false;
            _applyingAsShortcuts = false;
            CancelApplyButton.Visibility = Visibility.Collapsed;
        }
        // The apply itself, or the post-apply re-plan, failed - release the
        // single-flight guard so the buttons aren't stuck disabled (F-C5-003).
        // Except a plan failure while an apply is STILL in flight: releasing
        // then would re-enable Apply for a concurrent run (see _applyInFlight).
        if (!_applyInFlight)
        {
            _applying = false;
            ReplanButton.IsEnabled = true;
            RecomputeSelection();
        }

        if (planError)
        {
            CompletePlanningState();
            PlanStatusText.Text = "Planning didn't complete — try again, or run a fresh scan.";
            _ = ShowAlertAsync("Couldn't plan the reorganization",
                string.IsNullOrWhiteSpace(err.Message)
                    ? "FileID couldn't compute a reorganization plan. Try again, or run a fresh scan first."
                    : err.Message);
        }
        else
        {
            // No "your files are unchanged" claim: an apply that dies mid-run
            // (task panic) may already have moved files; every completed move
            // is in the engine's undo journal.
            ApplyStatusText.Text = "Apply didn't complete - try again.";
            _ = ShowAlertAsync("Couldn't apply changes",
                (string.IsNullOrWhiteSpace(err.Message)
                    ? "FileID couldn't finish applying your reorganization."
                    : err.Message) +
                "\n\nIf the run stopped partway, every move that completed was journaled and can be undone. Try again; if it keeps failing, check the engine log at %LOCALAPPDATA%\\FileID\\logs\\engine.jsonl.");
        }
    }

    // R6-06: release the apply single-flight guard when the engine process
    // that owned the in-flight apply (or its post-apply re-plan) is gone. That
    // engine's RestructureApplyResult / apply_restructure error can never
    // arrive, so without this the static guard stayed engaged and the Apply
    // buttons were disabled for the rest of the session (an unplugged external
    // drive killing the engine mid-apply is the daily-user repro).
    private void SyncEngineLifecycle()
    {
        var currentGeneration = EngineClient.Instance.SpawnGeneration;
        if (_planning && _planningSpawnGen != currentGeneration)
        {
            CompletePlanningState();
            PlanStatusText.Text =
                "The engine restarted before planning finished. Generate the plan again.";
        }

        if (!ShouldReleaseApplyGuardOnEngineChange(
                _applying || _applyInFlight, _applyingSpawnGen, currentGeneration))
        {
            return;
        }
        DebugLog.Warn("RestructureView: engine restarted while an apply/re-plan was in flight; releasing the single-flight guard.");
        bool applyWasInFlight = _applyInFlight;
        _applyInFlight = false;
        _applying = false;
        _applyingAsShortcuts = false;
        _applyingPlan = null;
        CancelApplyButton.Visibility = Visibility.Collapsed;
        RecomputeSelection();
        if (applyWasInFlight)
        {
            // No "your files are unchanged" claim — moves that completed
            // before the engine died are real and journaled (undoable).
            ApplyStatusText.Text = "The engine stopped while applying. Completed moves were journaled and can be undone — generate a fresh plan before applying again.";
            _ = ShowAlertAsync("Apply interrupted",
                "The engine stopped while your reorganization was being applied. Every move that completed was journaled and can be undone.\n\n" +
                "Generate a fresh plan before applying again — the old plan no longer matches what's on disk.");
        }
        else
        {
            // Guard was held across the post-apply re-plan when the engine
            // died — the fresh plan will never arrive on its own.
            ApplyStatusText.Text = "The engine restarted before the updated plan arrived. Generate the plan again.";
        }
    }

    // ---- Helpers --------------------------------------------------------

    private static string Count(int n, string noun)
        => $"{n:N0} {noun}{(n == 1 ? "" : "s")}";

    internal static bool IsFrozenPlanCurrent(
        object? frozenPlan,
        object? livePlan,
        object? renderedPlan)
        => frozenPlan is not null
           && ReferenceEquals(frozenPlan, livePlan)
           && ReferenceEquals(frozenPlan, renderedPlan);

    internal static bool IsCurrentRecommendation(
        RestructureRecommendationVm? invoked,
        RestructureRecommendationVm? rendered)
        => invoked is not null && ReferenceEquals(invoked, rendered);

    internal static bool ShouldRecordUndoableRestructureChange(
        bool appliedAsShortcuts,
        uint applied,
        bool canUndoThisRun)
        => !appliedAsShortcuts && applied > 0 && canUndoThisRun;

    internal static string FormatApplyCompletion(
        RestructureApplyResult result,
        bool appliedAsShortcuts)
    {
        if (result.Cancelled)
        {
            var remaining = result.Remaining is { } count
                ? $"{count:N0} eligible proposal{(count == 1 ? "" : "s")} stayed unchanged."
                : "Unprocessed proposals stayed unchanged.";
            return appliedAsShortcuts
                ? $"Stopped safely after creating {result.Applied:N0} shortcut" +
                  $"{(result.Applied == 1 ? "" : "s")}. Originals stayed put; {remaining}"
                : $"Stopped safely after moving {result.Applied:N0} file" +
                  $"{(result.Applied == 1 ? "" : "s")}. Completed moves remain undoable; {remaining}";
        }

        if (result.Failed == 0)
        {
            return appliedAsShortcuts
                ? $"Created {result.Applied:N0} shortcut{(result.Applied == 1 ? "" : "s")}; originals stayed put."
                : $"Moved {result.Applied:N0} file{(result.Applied == 1 ? "" : "s")} successfully.";
        }

        return appliedAsShortcuts
            ? $"Created {result.Applied:N0} shortcuts; {result.Failed:N0} failed. " +
              "Check %LOCALAPPDATA%\\FileID\\logs\\."
            : $"Moved {result.Applied:N0}; {result.Failed:N0} failed. " +
              "Check %LOCALAPPDATA%\\FileID\\logs\\.";
    }

    internal static string FormatUndoCompletion(
        RestructureApplyResult result,
        bool wasShortcutUndo = false)
    {
        var verb = wasShortcutUndo ? "removing" : "restoring";
        var item = wasShortcutUndo ? "shortcut" : "file";
        if (result.Cancelled)
        {
            if (result.Remaining == 0)
            {
                return $"Undo stopped after {verb} {result.Applied:N0} {item}" +
                       $"{(result.Applied == 1 ? "" : "s")}. No moves remain.";
            }
            var remaining = result.Remaining is { } count
                ? wasShortcutUndo
                    ? $"{count:N0} shortcut{(count == 1 ? "" : "s")} still need to be undone."
                    : $"{count:N0} move{(count == 1 ? "" : "s")} still need to be undone."
                : wasShortcutUndo
                    ? "Some shortcuts still need to be undone."
                    : "Some moves still need to be undone.";
            return $"Undo stopped after {verb} {result.Applied:N0} {item}" +
                   $"{(result.Applied == 1 ? "" : "s")}. {remaining} Click Undo again to continue.";
        }

        if (result.Failed > 0)
        {
            return wasShortcutUndo
                ? $"Undo finished with problems — {result.Applied:N0} shortcuts removed, " +
                  $"{result.Failed:N0} couldn't be safely removed. Check %LOCALAPPDATA%\\FileID\\logs\\."
                : $"Undo finished with problems — {result.Applied:N0} moved back, " +
                  $"{result.Failed:N0} couldn't be restored. Check %LOCALAPPDATA%\\FileID\\logs\\.";
        }

        if (result.Applied > 0)
        {
            return wasShortcutUndo
                ? $"Removed {result.Applied:N0} restructure shortcut" +
                  $"{(result.Applied == 1 ? "" : "s")}."
                : $"Undid the last restructure — {result.Applied:N0} file" +
                  $"{(result.Applied == 1 ? "" : "s")} moved back to where they were.";
        }

        return wasShortcutUndo
            ? "No shortcuts were removed. Undo remains available; check the engine log and try again."
            : "Nothing was restored. Undo remains available; check the engine log and try again.";
    }

    // R6-05: a completion (apply-result or engine-error) is "unhandled" when it's
    // present and not the reference we last surfaced. EngineClient.Set swaps in a
    // new instance only when the payload changes, so reference identity is the
    // de-dupe key (mirrors SyncEngineError's ReferenceEquals guard). Gates the
    // OnLoaded reload replay so a still-in-flight apply (slot unchanged) is a
    // no-op and an already-surfaced completion can't re-alert.
    internal static bool IsUnhandledCompletion<T>([System.Diagnostics.CodeAnalysis.NotNullWhen(true)] T? completion, T? lastSurfaced) where T : class
        => completion is not null && !ReferenceEquals(completion, lastSurfaced);

    // Every plan-path failure kind the engine emits (restructure.rs:
    // plan_restructure_failed / plan_restructure_db / plan_restructure_store).
    // Prefix match so a new plan_restructure_* kind can't silently re-freeze
    // the tab on "Computing plan...".
    internal static bool IsPlanRestructureErrorKind(string kind)
        => kind.StartsWith("plan_restructure", StringComparison.Ordinal);

    // SyncPlan's guard-release rule: a new plan instance releases the apply
    // single-flight guard only once the in-flight apply has completed
    // (result or error arrived). See _applyInFlight.
    internal static bool ShouldReleaseApplyGuardOnPlanArrival(
        bool applyInFlight, object? incomingPlan, object? applyingPlan)
        => !applyInFlight && !ReferenceEquals(incomingPlan, applyingPlan);

    // R6-06: SyncEngineLifecycle's release rule — the guard is released only
    // when it is actually engaged AND the engine process generation moved
    // since the apply was sent (the owning process is gone, so its terminal
    // result/error event can never arrive).
    internal static bool ShouldReleaseApplyGuardOnEngineChange(
        bool guardEngaged, int applyingGeneration, int currentGeneration)
        => guardEngaged && applyingGeneration != currentGeneration;

    // Mirrors SidebarProcessingControl.ShowAlertAsync: a dismissible ContentDialog
    // that never escalates to App.UnhandledException on a broken XamlRoot.
    private async Task ShowAlertAsync(string title, string body)
    {
        try
        {
            if (XamlRoot is null)
            {
                DebugLog.Warn($"ShowAlertAsync: XamlRoot is null ({title}); skipping dialog.");
                return;
            }
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = title,
                Content = body,
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ShowAlertAsync({title}) threw: " + ex.Message);
        }
    }
}
