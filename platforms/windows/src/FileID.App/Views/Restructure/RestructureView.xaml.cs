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
            XamlRoot = this.XamlRoot,
            Title = "Files in this flow",
            Content = sheet,
            CloseButtonText = "Done",
            DefaultButton = ContentDialogButton.Close,
        };
        try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
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

        // FEAT-CRIT-3: compute Anchor / Mixed / Junk classification from
        // per-source-folder move ratios. Engine doesn't classify
        // authoritatively yet (deferred to V14.8); the UI derives the
        // tiers from move counts vs total-file counts.
        ComputeAndShowClassifier(plan);

        var hasWork = moveCount > 0;
        ApplySymlinkButton.IsEnabled = hasWork;
        ApplyMovesButton.IsEnabled = hasWork;
        ApplyStatusText.Text = hasWork
            ? $"Ready to apply {moveCount:N0} moves into '{plan.LibraryRoot}'."
            : "Nothing to apply.";
    }

    /// <summary>
    /// V14.7.2: engine-authoritative Anchor/Mixed/Junk counts when the
    /// plan ships them (`FolderClassifications`). Falls back to the
    /// V14.7 C#-side approximation for older plans (or if the engine
    /// hasn't migrated). The fallback uses move-ratio homogeneity.
    /// </summary>
    private void ComputeAndShowClassifier(RestructurePlan plan)
    {
        if (plan.Moves.Count == 0)
        {
            ClassifierStrip.Visibility = Visibility.Collapsed;
            return;
        }

        uint anchor, mixed, junk;
        if (plan.FolderClassifications is { } engineCounts)
        {
            // Engine computed it — trust those numbers.
            anchor = engineCounts.AnchorFolders;
            mixed  = engineCounts.MixedFolders;
            junk   = engineCounts.JunkFolders;
        }
        else
        {
            // Fallback for older engine builds.
            var bySource = new System.Collections.Generic.Dictionary<string, int>(System.StringComparer.OrdinalIgnoreCase);
            foreach (var m in plan.Moves)
            {
                var srcFolder = System.IO.Path.GetDirectoryName(m.Source) ?? string.Empty;
                if (!bySource.ContainsKey(srcFolder)) bySource[srcFolder] = 0;
                bySource[srcFolder]++;
            }
            int a = 0, mx = 0, j = 0;
            foreach (var kv in bySource)
            {
                var catCounts = new System.Collections.Generic.Dictionary<string, int>();
                foreach (var m in plan.Moves)
                {
                    var srcFolder = System.IO.Path.GetDirectoryName(m.Source) ?? string.Empty;
                    if (!string.Equals(srcFolder, kv.Key, System.StringComparison.OrdinalIgnoreCase)) continue;
                    var destRoot = m.Destination
                        .Substring(plan.LibraryRoot.Length)
                        .TrimStart('\\', '/')
                        .Split('\\', '/')[0];
                    if (!catCounts.ContainsKey(destRoot)) catCounts[destRoot] = 0;
                    catCounts[destRoot]++;
                }
                int total = kv.Value;
                int topCat = 0;
                foreach (var c in catCounts.Values) if (c > topCat) topCat = c;
                double homogeneity = total > 0 ? (double)topCat / total : 0;
                if (total <= 2) j++;
                else if (homogeneity >= 0.80) a++;
                else mx++;
            }
            anchor = (uint)a; mixed = (uint)mx; junk = (uint)j;
        }
        AnchorCountText.Text = anchor.ToString("N0");
        MixedCountText.Text  = mixed.ToString("N0");
        JunkCountText.Text   = junk.ToString("N0");
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
