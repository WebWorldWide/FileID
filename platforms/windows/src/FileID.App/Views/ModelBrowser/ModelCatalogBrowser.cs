using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views.ModelBrowser;

/// <summary>Find a local model (macOS ModelCatalogBrowser): the curated defaults and the pinned
/// catalog from listModels, filtered by what fits this PC's memory and graphics memory, with
/// download (terms first where required), on-device benchmark and, for Describe Files, Use.</summary>
internal sealed class ModelCatalogBrowser : UserControl
{
    private readonly EngineClient _engine = EngineClient.Instance;
    private readonly bool _describeFilesOnly;
    private readonly Action<ModelCatalogItem>? _onUse;
    private readonly TextBox _search = new() { PlaceholderText = "Search model, publisher or license", MinWidth = 260 };
    private readonly ComboBox _role = new() { ItemsSource = ModelCatalogPresentation.Roles.Select(r => r.Label).ToArray(), SelectedIndex = 0, MinWidth = 130 };
    private readonly CheckBox _onlyFit = new() { Content = "Fits this PC", IsChecked = true };
    private readonly CheckBox _installedOnly = new() { Content = "Downloaded", IsChecked = false };
    private readonly Button _refresh = new() { Content = "Refresh" };
    private readonly Button _stopBenchmark = new() { Content = "Stop benchmark" };
    private readonly TextBlock _hardware;
    private readonly TextBlock _count;
    private readonly TextBlock _status;
    private readonly StackPanel _list = new() { Spacing = 8 };
    private readonly Brush _text1 = ThemeHelper.GetBrushSafe("Text1Brush");
    private readonly Brush _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
    private readonly Brush _warning = ThemeHelper.GetBrushSafe("WarningBrush");
    private readonly Brush _gold = ThemeHelper.GetBrushSafe("GoldBrush");
    private readonly Brush _rowFill = ThemeHelper.GetBrushSafe("PanelBrush");
    private readonly Brush _rowStroke = ThemeHelper.GetBrushSafe("HairlineStrongBrush");
    private IReadOnlyList<ModelCatalogItem> _models = Array.Empty<ModelCatalogItem>();
    private string? _selectedId;
    private CancellationTokenSource? _lifetime;
    private int _refreshing;

    public ModelCatalogBrowser(bool describeFilesOnly, string? selectedId = null, Action<ModelCatalogItem>? onUse = null)
    {
        _describeFilesOnly = describeFilesOnly;
        _selectedId = selectedId;
        _onUse = onUse;
        _hardware = Caption("");
        _count = Caption("");
        _status = Caption("Downloads start only when you choose Download. Models run on this PC.");
        AutomationProperties.SetName(_search, "Search models");
        AutomationProperties.SetName(_role, "Model role");
        ToolTipService.SetToolTip(_onlyFit, "Hide models whose memory estimate is larger than this PC's memory or graphics memory allows");
        ToolTipService.SetToolTip(_stopBenchmark, "Stop the benchmark that is running");
        _role.Visibility = describeFilesOnly ? Visibility.Collapsed : Visibility.Visible;

        var filters = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        foreach (var element in new UIElement[] { _search, _role, _onlyFit, _installedOnly, _refresh, _stopBenchmark })
        {
            if (element is FrameworkElement fe) fe.VerticalAlignment = VerticalAlignment.Center;
            filters.Children.Add(element);
        }

        var stack = new StackPanel { Spacing = 10 };
        stack.Children.Add(_hardware);
        stack.Children.Add(new ScrollViewer
        {
            Content = filters,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
        });
        stack.Children.Add(_count);
        stack.Children.Add(_list);
        stack.Children.Add(_status);
        Content = stack;

        _search.TextChanged += (_, _) => DebugLog.SafeRun("ModelCatalogBrowser.Search", Render);
        _onlyFit.Checked += (_, _) => DebugLog.SafeRun("ModelCatalogBrowser.Fit", Render);
        _onlyFit.Unchecked += (_, _) => DebugLog.SafeRun("ModelCatalogBrowser.Fit", Render);
        _installedOnly.Checked += (_, _) => DebugLog.SafeRun("ModelCatalogBrowser.Installed", Render);
        _installedOnly.Unchecked += (_, _) => DebugLog.SafeRun("ModelCatalogBrowser.Installed", Render);
        _role.SelectionChanged += (_, _) => DebugLog.SafeRun("ModelCatalogBrowser.Role", Refresh);
        _refresh.Click += (_, _) => DebugLog.SafeRun("ModelCatalogBrowser.Refresh", Refresh);
        _stopBenchmark.Click += (_, _) => _ = DebugLog.SafeRunAsync("ModelCatalogBrowser.StopBenchmark", () => _engine.CancelBenchmarkAsync());
        Loaded += (_, _) => DebugLog.SafeRun("ModelCatalogBrowser.Loaded", () =>
        {
            _lifetime?.Dispose();
            _lifetime = new CancellationTokenSource();
            _engine.PropertyChanged += OnEngineChanged;
            Refresh();
        });
        Unloaded += (_, _) => DebugLog.SafeRun("ModelCatalogBrowser.Unloaded", () =>
        {
            _engine.PropertyChanged -= OnEngineChanged;
            _lifetime?.Cancel();
            _lifetime?.Dispose();
            _lifetime = null;
        });
    }

    private string? Role => _describeFilesOnly ? "vision" : ModelCatalogPresentation.Roles[Math.Max(0, _role.SelectedIndex)].Role;

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("ModelCatalogBrowser.OnEngineChanged", () =>
        {
            if (e.PropertyName is not (nameof(EngineClient.BenchmarkProgress) or nameof(EngineClient.ModelDownloadProgress))) return;
            DebugLog.Debug($"[ENGINE-SUB:ModelCatalogBrowser] {e.PropertyName}");
            var benchmark = _engine.BenchmarkProgress;
            var download = _engine.ModelDownloadProgress;
            var property = e.PropertyName;
            DispatcherQueue.TryEnqueue(() => DebugLog.SafeRun("ModelCatalogBrowser.Progress", () =>
            {
                if (_lifetime is null) return;
                if (property == nameof(EngineClient.BenchmarkProgress) && benchmark is { } p)
                {
                    _status.Text = $"Benchmarking {NameFor(p.ModelKind)} · {p.Phase} {p.Completed}/{p.Total}";
                }
                else if (property == nameof(EngineClient.ModelDownloadProgress) && download is { } d && _models.Any(m => m.Id == d.ModelKind))
                {
                    _status.Text = FormattableString.Invariant($"{NameFor(d.ModelKind)}: {d.Message} ({Math.Round(d.Fraction * 100)}%)");
                    if (d.Fraction >= 1) Refresh();
                }
            }));
        });

    private string NameFor(string id) => _models.FirstOrDefault(m => m.Id == id)?.Name ?? id;

    private void Refresh()
    {
        if (_lifetime is not { } lifetime || Interlocked.Exchange(ref _refreshing, 1) != 0) return;
        var ct = lifetime.Token;
        var role = Role;
        _refresh.IsEnabled = false;
        _ = DebugLog.SafeRunAsync("ModelCatalogBrowser.RefreshAsync", async () =>
        {
            try
            {
                var catalog = await _engine.ListModelsAsync(role, ct);
                if (ct.IsCancellationRequested) return;
                _models = catalog.Models;
                _hardware.Text = ModelCatalogPresentation.HardwareSummary(catalog);
                Render();
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested) { }
            catch (Exception ex)
            {
                DebugLog.Warn("ModelCatalogBrowser: listModels failed: " + ex.Message);
                if (!ct.IsCancellationRequested) _status.Text = "The model list couldn't be read right now. FileID may still be starting; press Refresh in a moment.";
            }
            finally
            {
                Interlocked.Exchange(ref _refreshing, 0);
                if (!ct.IsCancellationRequested) _refresh.IsEnabled = true;
            }
        });
    }

    private void Render()
    {
        var filter = new ModelCatalogFilter(_search.Text, _onlyFit.IsChecked == true, _installedOnly.IsChecked == true, _describeFilesOnly);
        var matching = ModelCatalogPresentation.Filter(_models, filter);
        var shown = matching.Take(ModelCatalogPresentation.MaxRows).ToArray();
        _count.Text = ModelCatalogPresentation.CountSummary(shown.Length, matching.Count);
        _list.Children.Clear();
        if (shown.Length == 0)
        {
            _list.Children.Add(Caption(_models.Count == 0 ? "Loading the model list…" : "No models match these filters. Clear the search or turn off Fits this PC."));
            return;
        }
        foreach (var model in shown) _list.Children.Add(Row(model));
    }

    private Border Row(ModelCatalogItem model)
    {
        var name = new TextBlock { Text = model.Name, FontSize = 13.5, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, Foreground = _text1, TextTrimming = TextTrimming.CharacterEllipsis };
        var text = new StackPanel { Spacing = 3 };
        text.Children.Add(name);
        text.Children.Add(Caption($"{model.Upstream} · {model.Id}"));
        text.Children.Add(Caption(ModelCatalogPresentation.Summary(model)));
        if (ModelCatalogPresentation.TermsPolicyKey(model) is not null) text.Children.Add(Caption(ModelCatalogPresentation.GemmaTermsNote, _warning));
        if (ModelCatalogPresentation.FitNote(model) is { } fit) text.Children.Add(Caption(fit, _warning));
        if (model.Benchmark is { } benchmark) text.Children.Add(Caption(ModelCatalogPresentation.BenchmarkSummary(benchmark), _gold));

        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        if (ModelCatalogPresentation.CanInstall(model))
        {
            actions.Children.Add(RowButton("Download", $"Download {model.Name} from its pinned Hugging Face revision", () => DownloadAsync(model)));
        }
        if (ModelCatalogPresentation.CanBenchmark(model))
        {
            actions.Children.Add(RowButton("Benchmark", "Measure speed, memory and answers on this PC with built-in test images. Nothing is downloaded.", () => BenchmarkAsync(model, false)));
            actions.Children.Add(RowButton("Benchmark + tune", "Also try llama.cpp settings and keep the fastest one that gives identical answers", () => BenchmarkAsync(model, true)));
        }
        if (_onUse is not null && ModelCatalogPresentation.IsDescribeFilesModel(model))
        {
            var use = RowButton("Use for Describe Files", "Describe files with this model", () =>
            {
                _selectedId = model.Id;
                _onUse(model);
                return Task.CompletedTask;
            });
            if (ModelCatalogPresentation.CanUseForDescribeFiles(model))
            {
                if (ThemeHelper.GetStyleSafe("ShellPrimaryButtonStyle") is { } primary) use.Style = primary;
            }
            else
            {
                use.IsEnabled = false;
                ToolTipService.SetToolTip(use, model.Fits ? "Download this model first" : "Too big for this PC's memory");
            }
            actions.Children.Add(use);
        }
        if (actions.Children.Count > 0) text.Children.Add(actions);

        var selected = string.Equals(model.Id, _selectedId, StringComparison.Ordinal);
        var row = new Border
        {
            Padding = new Thickness(14, 10, 14, 10),
            CornerRadius = new CornerRadius(10),
            Background = _rowFill,
            BorderBrush = selected ? _gold : _rowStroke,
            BorderThickness = new Thickness(selected ? 2 : 1),
            Child = text,
        };
        AutomationProperties.SetName(row, selected ? $"{model.Name}, selected" : model.Name);
        return row;
    }

    private Button RowButton(string label, string tooltip, Func<Task> action)
    {
        var button = new Button { Content = label };
        if (ThemeHelper.GetStyleSafe("ShellSecondaryButtonStyle") is { } style) button.Style = style;
        ToolTipService.SetToolTip(button, tooltip);
        button.Click += (_, _) => _ = DebugLog.SafeRunAsync("ModelCatalogBrowser." + label, async () =>
        {
            button.IsEnabled = false;
            try { await action(); }
            catch (Exception ex) { _status.Text = ex.Message; }
            finally { button.IsEnabled = true; }
        });
        return button;
    }

    private async Task DownloadAsync(ModelCatalogItem model)
    {
        if (!await ModelLicenseGate.EnsureCatalogTermsAcceptedAsync(model))
        {
            _status.Text = $"{model.Name} was not downloaded because its terms were not accepted.";
            return;
        }
        await _engine.PrewarmModelAsync(model.Id);
        _status.Text = $"Downloading {model.Name} from its pinned Hugging Face revision. Progress shows here.";
    }

    private async Task BenchmarkAsync(ModelCatalogItem model, bool tune)
    {
        if (_lifetime is not { } lifetime) return;
        var ct = lifetime.Token;
        _status.Text = $"Benchmarking {model.Name}…";
        var result = await _engine.BenchmarkModelAsync(model.Id, tune, ct);
        if (ct.IsCancellationRequested) return;
        _status.Text = result.Benchmark is { } benchmark
            ? $"{model.Name}: {ModelCatalogPresentation.BenchmarkSummary(benchmark)}"
            : $"{model.Name}: {result.Message ?? result.Status}";
        Refresh();
    }

    private TextBlock Caption(string text, Brush? foreground = null)
    {
        var caption = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap };
        if (ThemeHelper.GetStyleSafe("CaptionTextBlockStyle") is { } style) caption.Style = style;
        caption.Foreground = foreground ?? _text3;
        return caption;
    }
}
