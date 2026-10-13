using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
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
    private readonly ToolPreviewState _plan = new();
    private readonly Button _preview;
    private readonly Button _execute;
    private readonly Button _undo;
    private string? _destinationPath;

    public ToolsWorkbench(IntPtr hwnd) : base("File Tools")
    {
        _hwnd = hwnd;
        Inputs.Children.Add(Row(_query, ActionButton("Find files", SearchAsync)));
        Inputs.Children.Add(Text("Select up to 100 catalog files. Preview first; exports create new versions and preserve originals."));
        Inputs.Children.Add(_files);
        Inputs.Children.Add(Row(_kind, _format, _dimension));
        Inputs.Children.Add(_destination);
        Inputs.Children.Add(ActionButton("Choose existing output folder", ChooseDestinationAsync));
        Inputs.Children.Add(_detail);
        _preview = ActionButton("Preview export", PreviewAsync);
        _execute = ActionButton("Export new versions…", ExecuteAsync);
        _undo = ActionButton("Undo last export…", UndoAsync);
        Inputs.Children.Add(Row(ActionButton("Refresh capabilities", CapabilitiesAsync), ActionButton("Last export", ct => SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "history"), ct)), _preview, _execute, _undo));
        Inputs.Children.Add(Text(ToolUndoIntegration.Detail));
        Inputs.Children.Add(_outputs);
        Inputs.Children.Add(Text("Photo exports may strip metadata and normalize to SDR; review the capability details. Chapter VTT is markers, not speech subtitles. Video export, stabilization, AI upscaling, tracked reframing and export cancellation are unavailable in this adapter. A timeout does not mean an export stopped: check Last export before retrying."));
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
    private ToolCapability? Capability => (_kind.SelectedItem as CapabilityItem)?.Capability;
    protected override void OnOpened() => Run(CapabilitiesAsync);
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
        _preview.IsEnabled = CanPreview;
        _execute.IsEnabled = _plan.CanExecute;
        _undo.IsEnabled = ToolUndoIntegration.CanUndo(_plan.UndoID);
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
    private Task CapabilitiesAsync(CancellationToken ct) => SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "capabilities"), ct);
    private Task PreviewAsync(CancellationToken ct)
    {
        UpdateControls();
        if (!CanPreview || Capability is not { } capability || _format.SelectedItem is not string format)
            throw new InvalidOperationException("Choose an available tool, valid dimension, files and an existing output folder.");
        var ids = _files.SelectedItems.Cast<HitItem>().Select(item => item.Hit.FileID).Distinct().Order().ToArray();
        return SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "preview", ids, _destinationPath,
            new ToolRecipe(capability.Id, format, (uint)_dimension.Value)), ct);
    }
    private async Task ExecuteAsync(CancellationToken ct)
    {
        if (!_plan.CanExecute) throw new InvalidOperationException("Preview a fresh export first.");
        if (!await ConfirmAsync("Export new versions?", "Create the previewed files? Originals are preserved. Photo metadata may be stripped and output normalized to SDR. " + ToolUndoIntegration.Detail)) return;
        ct.ThrowIfCancellationRequested();
        var id = _plan.PreviewID;
        _plan.BeginExecute();
        UpdateControls();
        await SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "execute", OperationID: id), ct);
    }
    private async Task UndoAsync(CancellationToken ct)
    {
        if (!ToolUndoIntegration.CanUndo(_plan.UndoID)) throw new InvalidOperationException(ToolUndoIntegration.Detail);
        if (_plan.UndoID is not { } id) return;
        if (!await ConfirmAsync("Undo export?", "Move unchanged exported files into internal recovery storage? Edited exports are preserved. Originals are unchanged.")) return;
        ct.ThrowIfCancellationRequested();
        await SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "undo", OperationID: id), ct);
    }
    private async Task SendAsync(ToolRequest request, CancellationToken ct)
    {
        var response = await Engine.ToolAsync(request, ct);
        await UI(() =>
        {
            Status.Text = $"{response.Status} · {response.Message}";
            if (request.Action == "capabilities")
            {
                _kind.ItemsSource = response.Capabilities.Select(capability => new CapabilityItem(capability)).ToArray();
                _kind.SelectedIndex = response.Capabilities.Count > 0 ? 0 : -1;
                Invalidate();
            }
            else
            {
                _plan.Apply(request.Action, response);
                _outputs.ItemsSource = response.Outputs.Select(output => $"{output.OutputPath}\n{output.State} · {output.Message}\nOriginal: {output.SourcePath}").ToArray();
            }
            UpdateControls();
        }, ct);
    }
    private sealed record CapabilityItem(ToolCapability Capability)
    {
        public override string ToString() => $"{Capability.Id}{(Capability.Available ? "" : " · unavailable")}";
    }
}
