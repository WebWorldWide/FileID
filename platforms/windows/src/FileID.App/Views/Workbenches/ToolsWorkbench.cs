using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Workbenches;

internal sealed class ToolsWorkbench : WorkbenchPanel
{
    private readonly IntPtr _hwnd;
    private readonly TextBox _query = new() { PlaceholderText = "Find catalog files by name or description", MaxLength = 2000, MinWidth = 360 };
    private readonly ListView _files = new() { Height = 200, SelectionMode = ListViewSelectionMode.Multiple };
    private readonly ComboBox _kind = new() { MinWidth = 160 };
    private readonly ComboBox _format = new() { MinWidth = 120 };
    private readonly NumberBox _dimension = new() { Header = "Maximum pixels (1–8192)", Minimum = 1, Maximum = 8192, Value = 4096, Width = 220 };
    private readonly TextBlock _destination = Text("No output folder selected.");
    private readonly TextBlock _detail = Text("Loading local tool capabilities…");
    private readonly ListView _outputs = new() { Height = 180, SelectionMode = ListViewSelectionMode.None };
    private readonly TextBlock _executionStatus = Text("No export awaiting confirmation.");
    private readonly ToolPreviewState _plan = new();
    private readonly Button _preview;
    private readonly Button _execute;
    private readonly Button _undo;
    private readonly Button _cancel;
    private readonly ToolExecutionState _execution;
    private string? _appliedExecutionRequestID;
    private string? _destinationPath;
    private bool _cancelAvailable;
    private int _capabilitiesGeneration;

    public ToolsWorkbench(IntPtr hwnd) : base("File Tools")
    {
        _hwnd = hwnd;
        _execution = Engine.ToolExecution;
        _query.MinWidth = 280;
        _files.Height = 260;
        Inputs.Children.Add(V3Panel("01 · Recipe", _kind,
            Row(_query, Secondary(ActionButton("Find", SearchAsync))),
            _files,
            Meta("Select up to 100 catalog files. Preview first; exports create new versions and preserve originals.")));

        var output = new Grid { ColumnSpacing = 12, RowSpacing = 10 };
        output.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(92) });
        output.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        AddOutputRow(output, 0, "Format", _format);
        _dimension.Header = null;
        _dimension.Width = 120;
        AddOutputRow(output, 1, "Longest side", Row(_dimension, Meta("px, up to 8192")));
        var chooseFolder = Secondary(ActionButton("Choose…", ChooseDestinationAsync));
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(chooseFolder, "Choose output folder");
        AddOutputRow(output, 2, "Save to", Row(_destination, chooseFolder));
        Inputs.Children.Add(V3Panel("02 · Output", null, output, _detail));

        _preview = Secondary(ActionButton("Preview", PreviewAsync));
        _execute = Primary(ActionButton("Export…", ExecuteAsync));
        _undo = Secondary(ActionButton("Undo last export…", UndoAsync));
        ToolTipService.SetToolTip(_execute, "Export new versions; originals are preserved");
        var previewPane = new StackPanel { Spacing = 10, Padding = new Thickness(14, 12, 14, 14) };
        previewPane.Children.Add(SectionLabel("03 · Preview"));
        _outputs.Height = 300;
        previewPane.Children.Add(_outputs);
        previewPane.Children.Add(_executionStatus);
        previewPane.Children.Add(Row(_preview, _execute, _undo));
        previewPane.Children.Add(Row(
            Ghost(ActionButton("Show last export", ct => SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "history"), ct))),
            Ghost(ActionButton("Refresh tools", CapabilitiesAsync))));
        previewPane.Children.Add(Meta(ToolUndoIntegration.Detail));
        previewPane.Children.Add(Meta("Only recipes reported as available by the engine can run. Photo exports may strip metadata and normalize to SDR; review the capability details. Chapter VTT is markers, not speech subtitles. Video conversion does not imply stabilization, AI upscaling or tracked reframing. Cancel requests target the current export; wait for its final response. A timeout does not mean an export stopped: check Last export before retrying."));
        UseSidePane(new ScrollViewer { Content = previewPane, VerticalScrollBarVisibility = ScrollBarVisibility.Auto }, 380, paneOnLeft: false);
        _cancel = ControlButton("Cancel current export", CancelAsync);
        _files.SelectionChanged += (_, _) => Invalidate();
        _kind.SelectionChanged += (_, _) =>
        {
            _format.ItemsSource = Capability?.OutputFormats;
            _format.SelectedIndex = Capability?.OutputFormats.Count > 0 ? 0 : -1;
            _detail.Text = Capability?.Detail ?? "Select a tool.";
            Invalidate();
        };
        _format.SelectionChanged += (_, _) => Invalidate();
        _dimension.ValueChanged += (_, _) => Invalidate();
        UpdateControls();
    }
    private static void AddOutputRow(Grid grid, int row, string label, FrameworkElement value)
    {
        grid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var key = Meta(label);
        key.FontSize = 12.5;
        key.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        key.Foreground = ThemeHelper.GetBrushSafe("Text2Brush");
        key.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetRow(key, row);
        Grid.SetRow(value, row);
        Grid.SetColumn(value, 1);
        grid.Children.Add(key);
        grid.Children.Add(value);
    }

    private ToolCapability? Capability => (_kind.SelectedItem as CapabilityItem)?.Capability;
    protected override void OnOpened()
    {
        Engine.PropertyChanged += OnEngineChanged;
        ApplyExecutionSnapshot();
        Run(async ct =>
        {
            await CapabilitiesAsync(ct);
            if (_execution.NeedsRecovery) await SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "history"), ct);
        });
    }
    protected override void OnClosed() => Engine.PropertyChanged -= OnEngineChanged;
    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(EngineClient.ToolExecution)) return;
        DebugLog.SafeRun("ToolsWorkbench.OnEngineChanged", () =>
        {
            DebugLog.Info($"[ENGINE-SUB:ToolsWorkbench] {e.PropertyName}");
            var ct = Lifetime;
            _ = DebugLog.SafeRunAsync("ToolsWorkbench.ApplyExecutionSnapshot", () => UI(ApplyExecutionSnapshot, ct));
        });
    }
    private void ApplyExecutionSnapshot()
    {
        if (_execution.LastResponse is { } response && response.RequestID != _appliedExecutionRequestID)
        {
            _appliedExecutionRequestID = response.RequestID;
            _plan.Apply("execute", response);
            Status.Text = $"{response.Status} · {response.Message}";
            ApplyOutputs(response);
        }
        UpdateControls();
    }
    private void ApplyOutputs(ToolResponse response) =>
        _outputs.ItemsSource = response.Outputs.Select(output => $"{output.OutputPath}\n{output.State} · {output.Message}\nOriginal: {output.SourcePath}").ToArray();
    private bool CanPreview
    {
        get
        {
            var dimension = _dimension.Value;
            return _files.SelectedItems.Count is >= 1 and <= 100 && _destinationPath is not null
                && double.IsFinite(dimension) && dimension == Math.Truncate(dimension) && dimension is >= 1 and <= 8192
                && _format.SelectedItem is string format && ToolPreviewState.Supports(Capability, format, (uint)dimension);
        }
    }
    protected override void UpdateControls()
    {
        _preview.IsEnabled = CanPreview && !_execution.IsActive && !_execution.NeedsRecovery;
        _execute.IsEnabled = _plan.CanExecute && !_execution.IsActive && !_execution.NeedsRecovery;
        _undo.IsEnabled = ToolUndoIntegration.CanUndo(_plan.UndoID);
        _cancel.IsEnabled = _cancelAvailable && _capabilitiesGeneration == Engine.SpawnGeneration
            && _execution.IsActive && !_execution.IsStopping && _execution.Generation == Engine.SpawnGeneration;
        _executionStatus.Text = _execution.Label;
    }
    private void Invalidate()
    {
        if (_plan.PreviewID is not null) Status.Text = "Export inputs changed. Preview again before exporting.";
        _plan.Invalidate();
        UpdateControls();
    }
    private async Task SearchAsync(CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(_query.Text)) throw new InvalidOperationException("Enter a catalog query.");
        var response = await Engine.CatalogAsync(CatalogRequest("search", _query.Text, mode: "keyword"), ct);
        await UI(() =>
        {
            _files.ItemsSource = response.Status == "ok" ? response.Hits.DistinctBy(hit => hit.FileID).Select(hit => new HitItem(hit)).ToArray() : Array.Empty<HitItem>();
            _files.SelectedItems.Clear();
            Invalidate();
            Status.Text = response.Message ?? $"{response.Hits.Count} evidence matches; select catalog files for export.";
        }, ct);
    }
    private async Task ChooseDestinationAsync(CancellationToken ct)
    {
        var chosen = await FolderPickerService.PickFolderAsync(_hwnd);
        await UI(() =>
        {
            if (chosen.Path is { } path) { _destinationPath = path; _destination.Text = path; Invalidate(); Status.Text = "Output folder selected. Preview before exporting."; }
            else
            {
                Status.Text = chosen.FailureReason ?? "Folder selection cancelled.";
            }
        }, ct);
    }
    private async Task CapabilitiesAsync(CancellationToken ct)
    {
        await Engine.WaitForReadyAsync(TimeSpan.FromSeconds(30), ct);
        await SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "capabilities"), ct);
    }
    private Task PreviewAsync(CancellationToken ct)
    {
        UpdateControls();
        if (_execution.IsActive || _execution.NeedsRecovery || !CanPreview || Capability is not { } capability || _format.SelectedItem is not string format)
            throw new InvalidOperationException("Choose an available tool, valid dimension, files and an existing output folder.");
        var ids = _files.SelectedItems.Cast<HitItem>().Select(item => item.Hit.FileID).Distinct().Order().ToArray();
        return SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "preview", ids, _destinationPath,
            new ToolRecipe(capability.Id, format, (uint)_dimension.Value)), ct);
    }
    private async Task ExecuteAsync(CancellationToken ct)
    {
        if (_execution.IsActive || _execution.NeedsRecovery || !_plan.CanExecute) throw new InvalidOperationException("Preview a fresh export first; any prior export must be confirmed.");
        if (!await ConfirmAsync("Export new versions?", "Create the previewed files? Originals are preserved. Photo metadata may be stripped and output normalized to SDR. " + ToolUndoIntegration.Detail)) return;
        ct.ThrowIfCancellationRequested();
        var id = _plan.PreviewID!;
        var generation = Engine.SpawnGeneration;
        _plan.BeginExecute();
        UpdateControls();
        await SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "execute", OperationID: id), ct, generation);
    }
    private async Task UndoAsync(CancellationToken ct)
    {
        if (!ToolUndoIntegration.CanUndo(_plan.UndoID)) throw new InvalidOperationException(ToolUndoIntegration.Detail);
        if (_plan.UndoID is not { } id) return;
        if (!await ConfirmAsync("Undo export?", "Move unchanged exported files into internal recovery storage? Edited exports are preserved. Originals are unchanged.")) return;
        ct.ThrowIfCancellationRequested();
        await SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "undo", OperationID: id), ct);
    }
    private async Task CancelAsync(CancellationToken ct)
    {
        var generation = Engine.SpawnGeneration;
        if (!_cancelAvailable || _capabilitiesGeneration != generation)
            throw new InvalidOperationException("The current adapter does not report interruptible export support.");
        var id = _execution.RequireCancellationOwner(generation);
        _execution.BeginCancellation(generation);
        UpdateControls();
        Status.Text = "Stopping · waiting for the engine to acknowledge the request…";
        try
        {
            var response = await Engine.ToolAsync(new ToolRequest(Guid.NewGuid().ToString(), "cancel", OperationID: id), generation, ct);
            await UI(() =>
            {
                if (generation != Engine.SpawnGeneration || !_execution.ApplyCancellationAck(id, generation, response)) return;
                Status.Text = $"{response.Status} · {response.Message} · Awaiting final export results; cancellation acknowledgement is not a completion receipt.";
                UpdateControls();
            }, ct);
        }
        catch
        {
            await UI(() => _execution.CancellationFailed(id, generation), ct);
            throw;
        }
    }
    private async Task SendAsync(ToolRequest request, CancellationToken ct, int? expectedGeneration = null)
    {
        await Engine.WaitForReadyAsync(TimeSpan.FromSeconds(30), ct);
        var generation = expectedGeneration ?? Engine.SpawnGeneration;
        var response = await Engine.ToolAsync(request, generation, ct);
        await UI(() =>
        {
            if (generation != Engine.SpawnGeneration) return;
            Status.Text = $"{response.Status} · {response.Message}";
            if (request.Action == "capabilities")
            {
                _cancelAvailable = response.Status == "ok" && ToolExecutionState.SupportsCancellation(response.Capabilities);
                _capabilitiesGeneration = generation;
                var recipes = response.Capabilities.Where(capability => capability.Id != "cancel").Select(capability => new CapabilityItem(capability)).ToArray();
                _kind.ItemsSource = recipes;
                _kind.SelectedIndex = recipes.Length > 0 ? 0 : -1;
                Invalidate();
            }
            else
            {
                if (request.Action == "execute")
                {
                    ApplyExecutionSnapshot();
                }
                else
                {
                    _plan.Apply(request.Action, response);
                    ApplyOutputs(response);
                }
            }
            UpdateControls();
        }, ct);
    }
    private sealed record CapabilityItem(ToolCapability Capability)
    {
        public override string ToString() => $"{Capability.Id}{(Capability.Available ? "" : " · unavailable")}";
    }
}
