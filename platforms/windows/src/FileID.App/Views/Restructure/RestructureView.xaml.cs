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
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(EngineClient.LastRestructurePlan))
        {
            DispatcherQueue.TryEnqueue(SyncPlan);
        }
        else if (e.PropertyName == nameof(EngineClient.LastRestructureApplyResult))
        {
            DispatcherQueue.TryEnqueue(SyncApplyResult);
        }
    }

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

        var hasWork = moveCount > 0;
        ApplySymlinkButton.IsEnabled = hasWork;
        ApplyMovesButton.IsEnabled = hasWork;
        ApplyStatusText.Text = hasWork
            ? $"Ready to apply {moveCount:N0} moves into '{plan.LibraryRoot}'."
            : "Nothing to apply.";
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
