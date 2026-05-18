// RestructureView code-behind. Wires Generate plan / Preview as symlinks /
// Apply (move) buttons to the engine's planRestructure + applyRestructure
// IPC. Subscribes to the EngineClient's LastRestructurePlan +
// LastRestructureApplyResult observables to refresh the UI.

using System.Collections.ObjectModel;
using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Restructure;

public sealed partial class RestructureView : UserControl
{
    private readonly ObservableCollection<RestructureCategoryRow> _categoryRows = new();

    public RestructureView()
    {
        InitializeComponent();
        CategoryRepeater.ItemsSource = _categoryRows;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Sankey.RibbonInvoked += OnSankeyRibbonInvoked;
        Unloaded += (_, _) =>
        {
            EngineClient.Instance.PropertyChanged -= OnEngineChanged;
            Sankey.RibbonInvoked -= OnSankeyRibbonInvoked;
        };
    }

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
            if (e.PropertyName == nameof(EngineClient.LastRestructurePlan))
            {
                Services.DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(SyncPlan);
            }
            else if (e.PropertyName == nameof(EngineClient.LastRestructureApplyResult))
            {
                Services.DebugLog.Debug($"[ENGINE-SUB:RestructureView] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(SyncApplyResult);
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

        var hasWork = moveCount > 0;
        ApplySymlinkButton.IsEnabled = hasWork;
        ApplyMovesButton.IsEnabled = hasWork;
        ApplyStatusText.Text = hasWork
            ? $"Ready to apply {moveCount:N0} moves into '{plan.LibraryRoot}'."
            : "Nothing to apply.";

        // update ApplyBar selection summary + step chips + primary
        // button label to reflect the plan count. macOS reference at
        // platforms/apple/.../RestructureApplyBar.swift.
        ApplyBarSelectedCount.Text = moveCount.ToString("N0");
        ApplyBarTotalCount.Text = moveCount.ToString("N0");
        ApplyBarHint.Text = hasWork
            ? "Originals stay put — applying creates shortcuts you can review."
            : "Generate a plan to enable Apply.";
        ApplySymlinkButtonText.Text = hasWork
            ? $"Apply as shortcuts ({moveCount:N0})"
            : "Apply as shortcuts";
        // Step chip 1 fills only when we actually have something to apply.
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
    {
        if (VisualizationModeCombo.SelectedItem is ComboBoxItem item && item.Tag is string mode)
        {
            var sankey = mode == "sankey";
            Sankey.Visibility = sankey ? Visibility.Visible : Visibility.Collapsed;
            TreeDiff.Visibility = sankey ? Visibility.Collapsed : Visibility.Visible;
            VisualizationHeader.Text = sankey
                ? "Source folder → category flow"
                : "Current ↔ proposed folder tree";
        }
    }

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
        ApplyStatusText.Text = useSymlinks
            ? $"Creating {plan.Moves.Count:N0} symlinks…"
            : $"Moving {plan.Moves.Count:N0} files…";
        await EngineClient.Instance.ApplyRestructureAsync(plan.LibraryRoot, plan.Moves, useSymlinks);
    }
}
