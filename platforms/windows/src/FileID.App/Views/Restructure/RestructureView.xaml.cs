// RestructureView code-behind. Wires Generate plan / Preview as symlinks /
// Apply (move) buttons to the engine's planRestructure + applyRestructure
// IPC. Subscribes to the EngineClient's LastRestructurePlan +
// LastRestructureApplyResult observables to refresh the UI.

using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Linq;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Restructure;

public sealed partial class RestructureView : UserControl
{
    private readonly ObservableCollection<RestructureCategoryRow> _categoryRows = new();
    private bool _unloaded;

    public RestructureView()
    {
        InitializeComponent();
        CategoryRepeater.ItemsSource = _categoryRows;
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

    // macOS parity (RestructureApplyBar.swift:114-117): the gold primary and
    // outlined secondary apply buttons scale up to 1.02× on hover with a
    // response: 0.28 / dampingFraction: 0.7 spring, gated by canApply.
    // SpringEasing.AnimateScale wraps Composition SpringScalarNaturalMotionAnimation
    // (Period=response, DampingRatio=dampingFraction) — the XAML comment at
    // RestructureView.xaml:280-281 promises this; this wires it.
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

    // macOS parity (RestructureView.swift `.task`): the plan auto-generates on
    // open — no manual "Generate plan" click. Render an already-computed plan if
    // one exists (cached on the engine across tab switches); otherwise compute it
    // now, provided a library folder has been scanned.
    private async void OnLoaded(object sender, RoutedEventArgs e)
        => await Services.DebugLog.SafeRunAsync(nameof(OnLoaded), async () =>
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
            PlanStatusText.Text = "Computing plan…";
            await EngineClient.Instance.PlanRestructureAsync(folder);
        });

    // Shows the Deep Analyze hint banner when there are unnamed person
    // clusters in the DB. Mirrors macOS RestructureView's "Name people
    // first" affordance — restructure puts photos into People/<name>/
    // folders, and unnamed clusters become "Person N", which the user
    // usually wants to fix before applying.
    private async System.Threading.Tasks.Task RefreshDeepAnalyzeHintAsync()
    {
        int unnamed = 0;
        try
        {
            unnamed = await System.Threading.Tasks.Task.Run(() =>
            {
                try
                {
                    if (!System.IO.File.Exists(AppPaths.DbPath)) return 0;
                    var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                        new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                        {
                            DataSource = AppPaths.DbPath,
                            Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                        }.ToString());
                    conn.Open();
                    using var cmd = conn.CreateCommand();
                    cmd.CommandText = "SELECT COUNT(*) FROM persons WHERE name IS NULL AND first_name IS NULL AND COALESCE(is_unknown, 0) = 0";
                    var v = cmd.ExecuteScalar();
                    return v is null ? 0 : System.Convert.ToInt32(v);
                }
                catch { return 0; }
            }).ConfigureAwait(true);
        }
        catch { unnamed = 0; }

        if (_unloaded) return;
        // Defensive: the view may have unloaded between the await and this
        // continuation, in which case the XAML element is already disposed.
        // Wrap in try/catch so the async-void plumbing doesn't propagate the
        // exception into the dispatcher loop (which would be a native fast-fail).
        try
        {
            DeepAnalyzeHintBanner.Visibility = unnamed > 0 ? Visibility.Visible : Visibility.Collapsed;
        }
        catch (System.Exception ex)
        {
            DebugLog.Warn("RefreshDeepAnalyzeHint UI update threw (view unloaded?): " + ex.Message);
        }
    }

    private void OnOpenPeopleHintClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnOpenPeopleHintClicked), () =>
        {
            try { AppViewModel.Instance.ActiveTab = SidebarTab.People; }
            catch (System.Exception ex) { DebugLog.Warn("Open People (hint) failed: " + ex.Message); }
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

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => Services.DebugLog.SafeRun("RestructureView.OnEngineChanged", () =>
        {
            if (_unloaded) return;
            if (e.PropertyName == nameof(EngineClient.LastRestructurePlan))
            {
                Services.DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncPlan(); });
            }
            else if (e.PropertyName == nameof(EngineClient.LastRestructureApplyResult))
            {
                Services.DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncApplyResult(); });
            }
            else if (e.PropertyName == nameof(EngineClient.DeepAnalyzeComplete))
            {
                // macOS parity: re-generate when Deep Analyze finishes so the
                // People/<name> buckets reflect newly-named clusters. Terminal
                // event (fires once per batch) and only while this view is alive.
                var folder = AppViewModel.Instance.FolderPath;
                if (!string.IsNullOrEmpty(folder))
                {
                    DispatcherQueue.TryEnqueue(async () =>
                    {
                        if (_unloaded) return;
                        try { await EngineClient.Instance.PlanRestructureAsync(folder!); }
                        catch (System.Exception ex) { Services.DebugLog.Warn("Restructure auto-regen failed: " + ex.Message); }
                    });
                }
            }
        });

    private void SyncPlan()
    {
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan is null)
        {
            return;
        }
        _categoryRows.Clear();
        foreach (var c in plan.CategoryCounts)
        {
            _categoryRows.Add(new RestructureCategoryRow { Category = c.Category, Count = c.Count });
        }
        var moveCount = plan.Moves.Count;
        PlanStatusText.Text = moveCount == 0
            ? "Plan ready: nothing to move (already organized!)."
            : $"Plan ready: {moveCount:N0} files across {plan.CategoryCounts.Count} categories.";
        CategoryListCard.Visibility = moveCount > 0 ? Visibility.Visible : Visibility.Collapsed;
        SankeyCard.Visibility = moveCount > 0 ? Visibility.Visible : Visibility.Collapsed;
        Sankey.SetPlan(plan);
        TreeDiff.SetPlan(plan);

        // Compute Anchor / Mixed / Junk classification from per-source-
        // folder move ratios when the engine's authoritative classifier
        // isn't present; the UI derives the tiers from move counts vs
        // total-file counts.
        ComputeAndShowClassifier(plan);
        ShowConfidenceTiers(plan);

        // ApplyBar totals reflect the plan; the selected subset (which the
        // confidence-tier toggles drive) is computed in UpdateSelection.
        // macOS reference: platforms/apple/.../RestructureApplyBar.swift.
        ApplyBarTotalCount.Text = moveCount.ToString("N0");
        ApplyBarHint.Text = moveCount > 0
            ? "Originals stay put — applying creates shortcuts you can review."
            : "Generate a plan to enable Apply.";
        UpdateSelection();
    }

    /// <summary>
    /// Populate + show the butler confidence tiers (auto / review / ask).
    /// Hidden when the engine didn't stamp confidences (older build); the
    /// apply path then falls back to applying every move.
    /// </summary>
    private void ShowConfidenceTiers(RestructurePlan plan)
    {
        int auto = 0, review = 0, ask = 0;
        foreach (var m in plan.Moves)
        {
            switch (m.Confidence)
            {
                case "auto": auto++; break;
                case "ask": ask++; break;
                default: review++; break; // "review" or empty/unknown
            }
        }
        bool stamped = plan.Moves.Count > 0 && plan.Moves.Any(m => !string.IsNullOrEmpty(m.Confidence));
        ConfidenceStrip.Visibility = stamped ? Visibility.Visible : Visibility.Collapsed;
        AutoTierCount.Text = auto.ToString("N0");
        ReviewTierCount.Text = review.ToString("N0");
        AskTierCount.Text = ask.ToString("N0");
        // Butler default: auto-file the sure ones + review the medium; hold "ask".
        AutoTierToggle.IsChecked = true;
        ReviewTierToggle.IsChecked = true;
        AskTierToggle.IsChecked = false;
    }

    private bool ConfidenceStripActive => ConfidenceStrip.Visibility == Visibility.Visible;

    private bool IsBandSelected(string? confidence) => confidence switch
    {
        "auto" => AutoTierToggle.IsChecked == true,
        "ask" => AskTierToggle.IsChecked == true,
        _ => ReviewTierToggle.IsChecked == true, // "review" or empty/unknown
    };

    private System.Collections.Generic.List<RestructureMove> SelectedMoves(RestructurePlan plan)
        => plan.Moves.Where(m => IsBandSelected(m.Confidence)).ToList();

    private void OnTierToggle(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnTierToggle), UpdateSelection);

    /// <summary>Recompute the apply subset from the tier toggles, then refresh
    /// the ApplyBar count, primary-button label, and enabled state.</summary>
    private void UpdateSelection()
    {
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan is null) return;
        int selected = ConfidenceStripActive ? SelectedMoves(plan).Count : plan.Moves.Count;
        bool hasWork = selected > 0;
        ApplySymlinkButton.IsEnabled = hasWork;
        ApplyMovesButton.IsEnabled = hasWork;
        ApplyBarSelectedCount.Text = selected.ToString("N0");
        ApplySymlinkButtonText.Text = hasWork
            ? $"Apply as shortcuts ({selected:N0})"
            : "Apply as shortcuts";
        ApplyStatusText.Text = hasWork
            ? $"Ready to apply {selected:N0} of {plan.Moves.Count:N0} into '{plan.LibraryRoot}'."
            : "Select at least one tier to apply.";
        StepChip1Bg.Background = hasWork
            ? (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["GoldBrush"]
            : new Microsoft.UI.Xaml.Media.SolidColorBrush(
                Windows.UI.Color.FromArgb(0x44, 0xFF, 0xCC, 0x00));
    }

    /// <summary>
    /// Display engine-authoritative Anchor/Mixed/Junk counts from
    /// `plan.FolderClassifications`. The engine always emits this in the
    /// plan event; the engine is the single source of truth for these tiers.
    /// </summary>
    private void ComputeAndShowClassifier(RestructurePlan plan)
    {
        if (plan.Moves.Count == 0 || plan.FolderClassifications is not { } engineCounts)
        {
            ClassifierStrip.Visibility = Visibility.Collapsed;
            return;
        }
        AnchorCountText.Text = engineCounts.AnchorFolders.ToString("N0");
        MixedCountText.Text = engineCounts.MixedFolders.ToString("N0");
        JunkCountText.Text = engineCounts.JunkFolders.ToString("N0");
        ClassifierStrip.Visibility = Visibility.Visible;
    }

    private void SyncApplyResult()
    {
        var r = EngineClient.Instance.LastRestructureApplyResult;
        if (r is null) return;
        if (!string.IsNullOrEmpty(r.PrivilegeError))
        {
            ApplyStatusText.Text = r.PrivilegeError;
            return;
        }
        ApplyStatusText.Text = r.Failed == 0
            ? $"Applied {r.Applied:N0} moves successfully."
            : $"Applied {r.Applied:N0}, failed {r.Failed:N0}. Check %LOCALAPPDATA%\\FileID\\logs\\.";
        // step chip 2 fills once an Apply has succeeded. Visual
        // affordance that the two-step flow has advanced past "shortcuts".
        if (r.Failed == 0 && r.Applied > 0)
        {
            StepChip2Bg.Background =
                (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["GoldBrush"];
            StepChip2Bg.BorderThickness = new Thickness(0);
        }
    }

    private void OnVisualizationModeChanged(object sender, SelectionChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnVisualizationModeChanged), () =>
        {
            // ComboBox SelectedIndex="0" raises SelectionChanged during
            // InitializeComponent — before Sankey/TreeDiff/VisualizationHeader
            // are realized — so the fields are null on that first fire. The
            // XAML already encodes the index-0 state, so bail until they exist.
            if (Sankey is null || TreeDiff is null || VisualizationHeader is null) return;
            if (VisualizationModeCombo.SelectedItem is ComboBoxItem item && item.Tag is string mode)
            {
                var sankey = mode == "sankey";
                Sankey.Visibility = sankey ? Visibility.Visible : Visibility.Collapsed;
                TreeDiff.Visibility = sankey ? Visibility.Collapsed : Visibility.Visible;
                VisualizationHeader.Text = sankey
                    ? "Source folder → category flow"
                    : "Current ↔ proposed folder tree";
            }
        });

    private async void OnPlanClicked(object sender, RoutedEventArgs e)
    {
        var folder = AppViewModel.Instance.FolderPath;
        if (string.IsNullOrEmpty(folder))
        {
            PlanStatusText.Text = "Pick a library folder in the sidebar first.";
            return;
        }
        PlanStatusText.Text = "Computing plan…";
        await EngineClient.Instance.PlanRestructureAsync(folder);
    }

    private async void OnApplySymlinksClicked(object sender, RoutedEventArgs e)
    {
        await ApplyAsync(useSymlinks: true);
    }

    private async void OnApplyMovesClicked(object sender, RoutedEventArgs e)
    {
        await ApplyAsync(useSymlinks: false);
    }

    private async System.Threading.Tasks.Task ApplyAsync(bool useSymlinks)
    {
        var plan = EngineClient.Instance.LastRestructurePlan;
        if (plan is null || plan.Moves.Count == 0) return;
        System.Collections.Generic.IReadOnlyList<RestructureMove> moves =
            ConfidenceStripActive ? SelectedMoves(plan) : plan.Moves;
        if (moves.Count == 0) return;
        ApplyStatusText.Text = useSymlinks
            ? $"Creating {moves.Count:N0} symlinks…"
            : $"Moving {moves.Count:N0} files…";
        await EngineClient.Instance.ApplyRestructureAsync(plan.LibraryRoot, moves, useSymlinks);
    }
}
