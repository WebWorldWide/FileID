using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Workbenches;

/// <summary>Browse curated and catalog models for this PC, install one on request,
/// and benchmark it locally.</summary>
internal sealed class ModelsWorkbench : WorkbenchPanel
{
    private readonly ComboBox _role = new() { ItemsSource = ModelCatalogPresentation.Roles.Select(r => r.Label).ToArray(), SelectedIndex = 0, Width = 160 };
    private readonly CheckBox _onlyFit = new() { Content = "Only models that fit this PC", IsChecked = true };
    private readonly TextBlock _hardware = Text("");
    private readonly StackPanel _list = new() { Spacing = 8 };
    private IReadOnlyList<ModelCatalogItem> _models = Array.Empty<ModelCatalogItem>();

    public ModelsWorkbench() : base("Model catalog")
    {
        Inputs.Children.Add(Text("Permissively licensed local models pinned to exact Hugging Face revisions. Candidates have not been measured on this PC yet. Benchmark runs locally rendered probes (colour, counting, text) and records speed, memory and answer accuracy on this PC only; tuning keeps the fastest llama.cpp settings that give identical answers. Nothing downloads until you choose Install."));
        Inputs.Children.Add(Row(_role, _onlyFit, ActionButton("Refresh catalog", RefreshAsync), ActionButton("Stop benchmark", _ => Engine.CancelBenchmarkAsync())));
        Inputs.Children.Add(_hardware);
        Inputs.Children.Add(_list);
        _role.SelectionChanged += (_, _) => Run(RefreshAsync);
        _onlyFit.Checked += (_, _) => Render();
        _onlyFit.Unchecked += (_, _) => Render();
    }

    protected override void OnOpened()
    {
        Engine.PropertyChanged += OnEngineChanged;
        Run(RefreshAsync);
    }

    protected override void OnClosed() => Engine.PropertyChanged -= OnEngineChanged;

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        DebugLog.SafeRun("ModelsWorkbench.OnEngineChanged", () =>
        {
            if (e.PropertyName == nameof(EngineClient.BenchmarkProgress) && Engine.BenchmarkProgress is { } p)
            {
                DispatcherQueue.TryEnqueue(() => Status.Text = $"Benchmarking {p.ModelKind} · {p.Phase} {p.Completed}/{p.Total}");
            }
        });
    }

    private string Role => ModelCatalogPresentation.Roles[Math.Max(0, _role.SelectedIndex)].Role;

    private async Task RefreshAsync(CancellationToken ct)
    {
        var catalog = await Engine.ListModelsAsync(Role, ct);
        await UI(() =>
        {
            _models = catalog.Models;
            _hardware.Text = FormattableString.Invariant($"This PC: {catalog.PhysicalMemoryGB:0} GB memory · {catalog.VramMB} MB dedicated GPU memory · {catalog.Models.Count} models listed");
            Render();
        }, ct);
    }

    private void Render()
    {
        _list.Children.Clear();
        var shown = ModelCatalogPresentation.Visible(_models, _onlyFit.IsChecked == true);
        if (shown.Count == 0) _list.Children.Add(Text("No catalog models match these filters."));
        foreach (var model in shown)
        {
            var card = new StackPanel { Spacing = 4 };
            card.Children.Add(new TextBlock { Text = ModelCatalogPresentation.Summary(model), TextWrapping = TextWrapping.Wrap });
            card.Children.Add(Text($"{model.Upstream} · {model.Id}"));
            if (model.Benchmark is { } benchmark) card.Children.Add(new TextBlock { Text = ModelCatalogPresentation.BenchmarkSummary(benchmark), TextWrapping = TextWrapping.Wrap });
            var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
            if (ModelCatalogPresentation.CanInstall(model))
            {
                actions.Children.Add(ActionButton("Install", async ct =>
                {
                    await Engine.PrewarmModelAsync(model.Id);
                    await UI(() => Status.Text = $"Downloading {model.Id} from its pinned Hugging Face revision. Refresh when it completes.", ct);
                }));
            }
            if (ModelCatalogPresentation.CanBenchmark(model))
            {
                actions.Children.Add(ActionButton("Benchmark", ct => BenchmarkAsync(model.Id, false, ct)));
                actions.Children.Add(ActionButton("Benchmark + tune", ct => BenchmarkAsync(model.Id, true, ct)));
            }
            card.Children.Add(actions);
            _list.Children.Add(card);
        }
    }

    private async Task BenchmarkAsync(string modelKind, bool tune, CancellationToken ct)
    {
        await UI(() => Status.Text = $"Benchmarking {modelKind}…", ct);
        var result = await Engine.BenchmarkModelAsync(modelKind, tune, ct);
        await UI(() => Status.Text = result.Benchmark is { } benchmark
            ? $"{modelKind}: {ModelCatalogPresentation.BenchmarkSummary(benchmark)}"
            : $"{modelKind}: {result.Message ?? result.Status}", ct);
        await RefreshAsync(ct);
    }
}
