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
    private bool _suppressRecompute;
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
    // R6-06: the EngineClient.SpawnGeneration captured when the in-flight apply
    // was sent. If the generation moves while the guard is engaged, the engine
    // process that owned the apply (or its post-apply re-plan) is gone — its
    // result/error event can never arrive — so SyncEngineLifecycle releases the
    // guard instead of leaving Apply disabled for the rest of the session
    // (e.g. the external drive was unplugged mid-apply and the engine died).
    private static int _applyingSpawnGen;
    private bool _deepAnalyzeHintDismissed;
    private RestructureOutcome? _hovered;
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

        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Sankey.RibbonInvoked += OnSankeyRibbonInvoked;
        WireApplyBarHoverSprings();
        Loaded += OnLoaded;
        Unloaded += (_, _) =>
        {
            _unloaded = true;
            EngineClient.Instance.PropertyChanged -= OnEngineChanged;
            Sankey.RibbonInvoked -= OnSankeyRibbonInvoked;
        };
    }

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
            _ = RefreshDeepAnalyzeHintAsync();
            if (_unloaded) return;
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
            if (EngineClient.Instance.LastRestructurePlan is not null)
            {
                SyncPlan();
                return;
            }
            var folder = AppViewModel.Instance.FolderPath;
            if (string.IsNullOrEmpty(folder))
            {
                PlanStatusText.Text = "Pick a library folder in the sidebar to plan a reorganization.";
                return;
            }
            PlanStatusText.Text = "Computing plan...";
            // A freshly computed plan supersedes any prior selection intent.
            _deselectedFileIds.Clear();
            _selectedFileIds.Clear();
            try
            {
                await EngineClient.Instance.PlanRestructureAsync(folder);
            }
            catch (Exception ex)
            {
                // SendCommandAsync can throw if the engine pipe is dead. Without
                // this the status freezes on "Computing plan..." forever (the
                // plan event never arrives). Recover to a clear message.
                DebugLog.Warn("PlanRestructure (OnLoaded) send failed: " + ex.Message);
                PlanStatusText.Text = "Couldn't start planning - the engine isn't responding. Try restarting the app.";
            }
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
                case nameof(EngineClient.DeepAnalyzeProgress):
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) UpdateDeepAnalyzeBanner(); });
                    break;
                case nameof(EngineClient.DeepAnalyzeComplete):
                    {
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
                                // This recompute supersedes any prior plan, so the
                                // user's selection intent from the old plan must not
                                // leak forward (see _deselectedFileIds).
                                _deselectedFileIds.Clear();
                                _selectedFileIds.Clear();
                                try { await EngineClient.Instance.PlanRestructureAsync(folder!); }
                                catch (Exception ex) { DebugLog.Warn("Restructure auto-regen failed: " + ex.Message); }
                            }
                        });
                    }
                    break;
            }
        });

    private async void OnSankeyRibbonInvoked(object? sender, (string Source, string Category) ribbon)
    {
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan is null) return;
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
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
    }

    // ---- Plan rendering -------------------------------------------------

    private void SyncPlan()
    {
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan is null) return;

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

        int moveCount = plan.Truncated
            ? (int)Math.Min(plan.TotalMoves ?? (ulong)plan.Moves.Count, int.MaxValue)
            : plan.Moves.Count;
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
        int dstCount = plan.Moves.Select(m => m.Category).Distinct(StringComparer.OrdinalIgnoreCase).Count();
        SankeyHeroStat.Text = $"{srcCount} source{(srcCount == 1 ? "" : "s")} -> {dstCount} destination{(dstCount == 1 ? "" : "s")}";

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
    private void RecomputeSelection()
    {
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan?.Truncated == true)
        {
            int storedTotal = (int)Math.Min(plan.TotalMoves ?? (ulong)plan.Moves.Count, int.MaxValue);
            bool storedHasWork = storedTotal > 0 && !string.IsNullOrWhiteSpace(plan.PlanId);
            ApplySymlinkButton.IsEnabled = storedHasWork && !_applying;
            ApplyMovesButton.IsEnabled = storedHasWork && !_applying;
            ApplyBarSelectedCount.Text = storedTotal.ToString("N0");
            ApplyBarTotalCount.Text = storedTotal.ToString("N0");
            ApplySymlinkButtonText.Text = storedHasWork ? $"Apply as shortcuts ({storedTotal:N0})" : "Apply as shortcuts";
            ApplyStatusText.Text = storedHasWork
                ? $"Ready to apply all {storedTotal:N0} moves from the engine-stored plan into '{plan.LibraryRoot}'."
                : "The stored plan is unavailable. Generate it again.";
            ApplyBarHint.Text = "Large plans apply as one complete, crash-journaled run · Moves are undoable.";
            return;
        }
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

        bool hasWork = selected > 0;
        // Single-flight: while an apply is in flight the buttons stay disabled
        // even if a checkbox toggle re-runs this, so a stale plan can't be
        // re-applied (F-C5-003).
        ApplySymlinkButton.IsEnabled = hasWork && !_applying;
        ApplyMovesButton.IsEnabled = hasWork && !_applying;
        ApplyBarSelectedCount.Text = selected.ToString("N0");
        ApplySymlinkButtonText.Text = hasWork ? $"Apply as shortcuts ({selected:N0})" : "Apply as shortcuts";
        ApplyStatusText.Text = hasWork
            ? $"Ready to apply {selected:N0} of {total:N0} into '{plan?.LibraryRoot}'."
            : "Select at least one file to apply.";
        ApplyBarHint.Text = total > 0
            ? "Shortcuts leave originals in place · Moves are permanent but undoable."
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
                f.IsSelected = cb.IsChecked == true;
                if (f.IsSelected) { _deselectedFileIds.Remove(f.FileId); _selectedFileIds.Add(f.FileId); }
                else { _selectedFileIds.Remove(f.FileId); _deselectedFileIds.Add(f.FileId); }
            }
        });

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
                    f.IsSelected = approve;
                    if (approve) { _deselectedFileIds.Remove(f.FileId); _selectedFileIds.Add(f.FileId); }
                    else { _selectedFileIds.Remove(f.FileId); _deselectedFileIds.Add(f.FileId); }
                }
                _suppressRecompute = false;
            }
            RecomputeSelection();
        });

    private async void OnSeeAllClicked(object sender, RoutedEventArgs e)
    {
        if ((sender as FrameworkElement)?.DataContext is not RestructureRecommendationVm vm) return;
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan is null) return;
        var title = vm.Outcome switch
        {
            RestructureOutcome.Tidy => "Tidying - files moving out of mixed folders",
            RestructureOutcome.Reorganize => "Reorganizing - files leaving generic folders",
            _ => "Files staying put",
        };
        var sheet = new DrillDownSheet();
        sheet.SetOutcomeFilter(plan, vm.Outcome, title);
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Files in this group",
            Content = sheet,
            CloseButtonText = "Done",
            DefaultButton = ContentDialogButton.Close,
        };
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
    }

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
        if (EngineClient.Instance.DeepAnalyzeProgress != null) return; // running: handled by UpdateDeepAnalyzeBanner
        int captioned = 0, total = 0;
        try
        {
            (captioned, total) = await Task.Run(QueryCaptionedFraction).ConfigureAwait(true);
        }
        catch { /* keep zeros -> banner hidden */ }

        if (_unloaded) return;
        bool show = !_deepAnalyzeHintDismissed
            && total > 0
            && (double)captioned / total < 0.4
            && EngineClient.Instance.DeepAnalyzeProgress == null;
        try
        {
            DeepAnalyzeHintBanner.Visibility = show ? Visibility.Visible : Visibility.Collapsed;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Deep Analyze hint update threw (view unloaded?): " + ex.Message);
        }
    }

    private void UpdateDeepAnalyzeBanner()
    {
        if (EngineClient.Instance.DeepAnalyzeProgress != null)
        {
            DeepAnalyzeHintBanner.Visibility = Visibility.Visible;
            DeepAnalyzeHintTitle.Text = "Deep Analyze running...";
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

    private static (int captioned, int total) QueryCaptionedFraction()
    {
        try
        {
            if (!System.IO.File.Exists(AppPaths.DbPath)) return (0, 0);
            using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                {
                    DataSource = AppPaths.DbPath,
                    Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                }.ToString());
            conn.Open();
            using var cmd = conn.CreateCommand();
            cmd.CommandText =
                "SELECT COUNT(*), " +
                "SUM(CASE WHEN vlm_description IS NOT NULL AND vlm_description <> '' THEN 1 ELSE 0 END) " +
                "FROM files";
            using var reader = cmd.ExecuteReader();
            if (reader.Read())
            {
                int total = reader.IsDBNull(0) ? 0 : (int)Math.Min(Convert.ToInt64(reader.GetValue(0)), int.MaxValue);
                int captioned = reader.IsDBNull(1) ? 0 : (int)Math.Min(Convert.ToInt64(reader.GetValue(1)), int.MaxValue);
                return (captioned, total);
            }
            return (0, 0);
        }
        catch { return (0, 0); }
    }

    private async void OnRunDeepAnalyzeClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnRunDeepAnalyzeClicked), async () =>
        {
            var model = AppViewModel.Instance.Settings.SelectedVlmModelKind;
            DeepAnalyzeHintTitle.Text = "Deep Analyze running...";
            DeepAnalyzeHintBody.Text = "Analyzing your library - proposals will sharpen as it runs.";
            RunDeepAnalyzeButton.IsEnabled = false;
            await EngineClient.Instance.DeepAnalyzeAllAsync(model, skipExisting: true);
        });

    private void OnDismissHintClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnDismissHintClicked), () =>
        {
            _deepAnalyzeHintDismissed = true;
            DeepAnalyzeHintBanner.Visibility = Visibility.Collapsed;
        });

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
            PlanStatusText.Text = "Computing plan...";
            // A freshly computed plan supersedes any prior selection intent.
            _deselectedFileIds.Clear();
            _selectedFileIds.Clear();
            try
            {
                await EngineClient.Instance.PlanRestructureAsync(folder);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("PlanRestructure (OnPlanClicked) send failed: " + ex.Message);
                PlanStatusText.Text = "Couldn't start planning - the engine isn't responding. Try restarting the app.";
            }
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
        if (plan is null || plan.Moves.Count == 0) return;
        var sel = new List<RestructureMove>();
        if (!plan.Truncated)
        {
            foreach (var m in plan.Moves)
            {
                if (_allFileRows.TryGetValue(m.FileId, out var row) && row.IsSelected) sel.Add(m);
            }
        }
        if (plan.Truncated && string.IsNullOrWhiteSpace(plan.PlanId)) return;
        if (!plan.Truncated && sel.Count == 0) return;
        int applyCount = plan.Truncated
            ? (int)Math.Min(plan.TotalMoves ?? (ulong)plan.Moves.Count, int.MaxValue)
            : sel.Count;
        _applying = true;
        _applyInFlight = true;
        _applyingPlan = plan;   // R6-04: record the in-flight plan (see SyncPlan)
        _applyingSpawnGen = EngineClient.Instance.SpawnGeneration; // R6-06
        ApplySymlinkButton.IsEnabled = false;
        ApplyMovesButton.IsEnabled = false;
        ApplyStatusText.Text = useSymlinks
            ? $"Creating {applyCount:N0} symlinks..."
            : $"Moving {applyCount:N0} files...";
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
    private void SyncUndoAffordance()
    {
        var canUndo = EngineClient.Instance.CanUndoRestructure;
        UndoButton.Visibility = canUndo ? Visibility.Visible : Visibility.Collapsed;
        UndoButton.IsEnabled = canUndo;
    }

    private async void OnUndoClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnUndoClicked), async () =>
        {
            var root = EngineClient.Instance.LastRestructurePlan?.LibraryRoot
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
                Services.ChangeLogEntry? logEntry = null;
                foreach (var candidate in Services.ChangeLog.Instance.Snapshot())
                {
                    if (candidate.Kind == Services.ChangeKind.Restructure
                        && candidate.Status == Services.ChangeStatus.Undoable)
                    {
                        logEntry = candidate;
                        break;
                    }
                }
                if (logEntry is not null)
                {
                    await Services.ChangeLog.Instance.UndoAsync(logEntry);
                }
                else
                {
                    await EngineClient.Instance.UndoRestructureAsync(root!);
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

    private void SyncApplyResult()
    {
        var r = EngineClient.Instance.LastRestructureApplyResult;
        if (!IsUnhandledCompletion(r, _lastHandledApplyResult)) return;
        _lastHandledApplyResult = r;
        _applyInFlight = false;

        // Anything actually moved -> the current plan is stale (real moves
        // updated the DB; applied rows must leave the view). Re-plan, exactly as
        // macOS applySelected() does via regenerate(), and KEEP the single-flight
        // guard engaged across the re-plan so the stale plan can't be re-applied
        // in the gap before the fresh one lands (the F-C5-003 false-alarm window);
        // SyncPlan releases the guard when the new plan arrives. When nothing
        // moved, release it now so the user can retry.
        if (r.Applied > 0)
        {
            // Session change log: a confirmed apply is undoable via the
            // engine's inverse-move journal. Pushing a Restructure entry
            // auto-marks any older restructure entry "superseded" (the
            // journal is truncate-per-batch — only the latest replays).
            var undoRoot = EngineClient.Instance.LastRestructurePlan?.LibraryRoot
                           ?? AppViewModel.Instance.FolderPath;
            if (!string.IsNullOrEmpty(undoRoot))
            {
                Services.UndoStack.Instance.Push(
                    $"reorganize {r.Applied:N0} file{(r.Applied == 1 ? "" : "s")}",
                    Services.ChangeKind.Restructure,
                    async () =>
                    {
                        try
                        {
                            await EngineClient.Instance.UndoRestructureAsync(undoRoot!);
                            return true;
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
                // OBSERVE the re-plan Task rather than discarding it: PlanRestructureAsync
                // faults its returned Task (frame-too-large/not-Ready/pipe IO all run
                // async) and never throws synchronously, so a sync try/catch is dead
                // code -- a faulted send would leave the single-flight guard stuck and
                // the Apply buttons disabled forever. Release + log on the UI thread.
                _ = EngineClient.Instance.PlanRestructureAsync(folder!).ContinueWith(t =>
                    DispatcherQueue.TryEnqueue(() =>
                    {
                        if (_unloaded) return;
                        DebugLog.Warn("Restructure post-apply re-plan failed: "
                            + t.Exception?.GetBaseException().Message);
                        _applying = false;
                        RecomputeSelection();
                    }), TaskContinuationOptions.OnlyOnFaulted);
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
        ApplyStatusText.Text = r.Failed == 0
            ? $"Applied {r.Applied:N0} moves successfully."
            : $"Applied {r.Applied:N0}, failed {r.Failed:N0}. Check %LOCALAPPDATA%\\FileID\\logs\\.";
        if (r.Failed == 0 && r.Applied > 0)
        {
            StepChip2Bg.Background = FileID.Services.ThemeHelper.GetBrushSafe("GoldBrush");
            StepChip2Bg.BorderThickness = new Thickness(0);
        }
        else if (r.Failed > 0)
        {
            // Partial/total failure must be a dismissible, actionable surface -
            // not a status line the user can scroll past thinking it worked.
            _ = ShowAlertAsync("Some changes couldn't be applied",
                $"Applied {r.Applied:N0}, but {r.Failed:N0} failed. The originals for the failed items are unchanged.\n\n" +
                "This usually means a file was open, moved, or you don't have permission to write the destination. " +
                "Check the engine log at %LOCALAPPDATA%\\FileID\\logs\\engine.jsonl, then try again.");
        }
    }

    // A plan/apply that dies engine-side surfaces as EngineClient.LastError with
    // a restructure kind (restructure.rs: "plan_restructure_failed" /
    // "plan_restructure_db" / "plan_restructure_store" / "apply_restructure") -
    // never as a Plan/ApplyResult event. Without handling it the tab freezes on
    // "Computing plan..." / "Moving N files..." forever.
    // Only react to restructure kinds (LastError is a shared slot) and de-dupe.
    private void SyncEngineError()
    {
        var err = EngineClient.Instance.LastError;
        if (err is null || ReferenceEquals(err, _lastHandledError)) return;
        bool planError = IsPlanRestructureErrorKind(err.Kind);
        if (!planError && err.Kind != "apply_restructure") return;
        _lastHandledError = err;

        if (!planError) _applyInFlight = false;
        // The apply itself, or the post-apply re-plan, failed - release the
        // single-flight guard so the buttons aren't stuck disabled (F-C5-003).
        // Except a plan failure while an apply is STILL in flight: releasing
        // then would re-enable Apply for a concurrent run (see _applyInFlight).
        if (!_applyInFlight)
        {
            _applying = false;
            RecomputeSelection();
        }

        if (planError)
        {
            PlanStatusText.Text = "Planning didn't complete - try again, or run a fresh scan.";
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
        if (!ShouldReleaseApplyGuardOnEngineChange(
                _applying || _applyInFlight, _applyingSpawnGen, EngineClient.Instance.SpawnGeneration))
        {
            return;
        }
        DebugLog.Warn("RestructureView: engine restarted while an apply/re-plan was in flight; releasing the single-flight guard.");
        bool applyWasInFlight = _applyInFlight;
        _applyInFlight = false;
        _applying = false;
        _applyingPlan = null;
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
