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

    private bool _unloaded;
    private bool _suppressRecompute;
    private bool _deepAnalyzeHintDismissed;
    private RestructureOutcome? _hovered;
    private EngineError? _lastHandledError;

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
                case nameof(EngineClient.LastError):
                    DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncEngineError(); });
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
                            if (!string.IsNullOrEmpty(folder))
                            {
                                // This recompute supersedes any prior plan, so the
                                // user's selection intent from the old plan must not
                                // leak forward (see _deselectedFileIds).
                                _deselectedFileIds.Clear();
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
        int dstCount = plan.Moves.Select(m => m.Category).Distinct(StringComparer.OrdinalIgnoreCase).Count();
        SankeyHeroStat.Text = $"{srcCount} source{(srcCount == 1 ? "" : "s")} -> {dstCount} destination{(dstCount == 1 ? "" : "s")}";

        bool hasContent = moveCount > 0 || keepFolders > 0;
        bool hasMoves = moveCount > 0;
        PlanStatusText.Text = moveCount == 0
            ? "Your library is already organized - nothing to move."
            : $"{moveCount:N0} files to reorganize across {plan.CategoryCounts.Count} categories.";
        StatHero.Visibility = hasContent ? Visibility.Visible : Visibility.Collapsed;
        ViewModeToggle.Visibility = hasMoves ? Visibility.Visible : Visibility.Collapsed;
        UnifiedSurface.Visibility = hasMoves ? Visibility.Visible : Visibility.Collapsed;
        NothingToMoveCard.Visibility = hasMoves ? Visibility.Collapsed : Visibility.Visible;
        UpdateStayingPut(keepFolders);

        // Re-apply the selections the user made before navigating away (see
        // _deselectedFileIds). Suppressed so RecomputeSelection runs once below,
        // not once per row.
        if (_deselectedFileIds.Count > 0)
        {
            _suppressRecompute = true;
            foreach (var kv in _allFileRows)
                if (_deselectedFileIds.Contains(kv.Key)) kv.Value.IsSelected = false;
            _suppressRecompute = false;
        }

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
        ApplySymlinkButton.IsEnabled = hasWork;
        ApplyMovesButton.IsEnabled = hasWork;
        ApplyBarSelectedCount.Text = selected.ToString("N0");
        ApplySymlinkButtonText.Text = hasWork ? $"Apply as shortcuts ({selected:N0})" : "Apply as shortcuts";
        ApplyStatusText.Text = hasWork
            ? $"Ready to apply {selected:N0} of {total:N0} into '{plan?.LibraryRoot}'."
            : "Select at least one file to apply.";
        ApplyBarHint.Text = total > 0
            ? "Originals stay put - applying creates shortcuts you can review."
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
                if (f.IsSelected) _deselectedFileIds.Remove(f.FileId);
                else _deselectedFileIds.Add(f.FileId);
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
                    if (approve) _deselectedFileIds.Remove(f.FileId);
                    else _deselectedFileIds.Add(f.FileId);
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
                int total = reader.IsDBNull(0) ? 0 : Convert.ToInt32(reader.GetValue(0));
                int captioned = reader.IsDBNull(1) ? 0 : Convert.ToInt32(reader.GetValue(1));
                return (captioned, total);
            }
            return (0, 0);
        }
        catch { return (0, 0); }
    }

    private async void OnRunDeepAnalyzeClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnRunDeepAnalyzeClicked), async () =>
        {
            var model = AppSettings.Load().SelectedVlmModelKind;
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
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan is null || plan.Moves.Count == 0) return;
        var sel = new List<RestructureMove>();
        foreach (var m in plan.Moves)
        {
            if (_allFileRows.TryGetValue(m.FileId, out var row) && row.IsSelected) sel.Add(m);
        }
        if (sel.Count == 0) return;
        ApplyStatusText.Text = useSymlinks
            ? $"Creating {sel.Count:N0} symlinks..."
            : $"Moving {sel.Count:N0} files...";
        try
        {
            await EngineClient.Instance.ApplyRestructureAsync(plan.LibraryRoot, sel, useSymlinks);
        }
        catch (Exception ex)
        {
            // SendCommandAsync can throw if the engine pipe is dead. Without this
            // the status freezes on "Moving N files..." (the apply-result event
            // never arrives). Surface it instead of a silent hang.
            DebugLog.Warn("ApplyRestructure send failed: " + ex.Message);
            ApplyStatusText.Text = "Couldn't apply - the engine isn't responding. Try restarting the app.";
            await ShowAlertAsync("Couldn't apply changes",
                "FileID couldn't tell the engine to apply your reorganization (it isn't responding). " +
                "Your files were not touched. Try restarting the app, then apply again.");
        }
    }

    private void SyncApplyResult()
    {
        var r = EngineClient.Instance.LastRestructureApplyResult;
        if (r is null) return;
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
    // "apply_restructure") - never as a Plan/ApplyResult event. Without handling
    // it the tab freezes on "Computing plan..." / "Moving N files..." forever.
    // Only react to restructure kinds (LastError is a shared slot) and de-dupe.
    private void SyncEngineError()
    {
        var err = EngineClient.Instance.LastError;
        if (err is null || ReferenceEquals(err, _lastHandledError)) return;
        if (err.Kind != "plan_restructure_failed" && err.Kind != "apply_restructure") return;
        _lastHandledError = err;

        if (err.Kind == "plan_restructure_failed")
        {
            PlanStatusText.Text = "Planning didn't complete - try again, or run a fresh scan.";
            _ = ShowAlertAsync("Couldn't plan the reorganization",
                string.IsNullOrWhiteSpace(err.Message)
                    ? "FileID couldn't compute a reorganization plan. Try again, or run a fresh scan first."
                    : err.Message);
        }
        else
        {
            ApplyStatusText.Text = "Apply didn't complete - your files are unchanged. Try again.";
            _ = ShowAlertAsync("Couldn't apply changes",
                (string.IsNullOrWhiteSpace(err.Message)
                    ? "FileID couldn't finish applying your reorganization."
                    : err.Message) +
                "\n\nYour originals are unchanged. Try again; if it keeps failing, check the engine log at %LOCALAPPDATA%\\FileID\\logs\\engine.jsonl.");
        }
    }

    // ---- Helpers --------------------------------------------------------

    private static string Count(int n, string noun)
        => $"{n:N0} {noun}{(n == 1 ? "" : "s")}";

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
