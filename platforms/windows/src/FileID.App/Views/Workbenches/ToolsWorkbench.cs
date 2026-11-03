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
    private readonly TextBox _query = new() { PlaceholderText = "Search for files by name or by what's in them", MaxLength = 2000, MinWidth = 360 };
    private readonly ListView _files = new() { Height = 200, SelectionMode = ListViewSelectionMode.Multiple };
    private readonly ComboBox _kind = new() { MinWidth = 160 };
    private readonly ComboBox _format = new() { MinWidth = 120 };
    private readonly NumberBox _dimension = new() { Header = "Maximum pixels (1–8192)", Minimum = 1, Maximum = 8192, Value = 4096, Width = 220 };
    private readonly TextBlock _destination = Text("No folder chosen yet.");
    private readonly TextBlock _detail = Text("Checking what this PC can export…");
    private readonly ListView _outputs = new() { Height = 180, SelectionMode = ListViewSelectionMode.None };
    private readonly TextBlock _executionStatus = Text("Nothing exported yet.");
    private readonly ToolPreviewState _plan = new();
    private readonly Button _preview;
    private readonly Button _execute;
    private readonly Button _undo;
    private readonly Button _cancel;
    private readonly Button _find;
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
        _find = Secondary(ActionButton("Find", SearchAsync));
        _query.TextChanged += (_, _) => SyncFind();
        SyncFind();
        Inputs.Children.Add(V3Panel("01 · Pick files", _kind,
            Row(_query, _find),
            _files,
            Meta("Search for files by name or by what's in them, then tick the ones to export (up to 100). Exports are new files; your originals stay as they are.")));

        var output = new Grid { ColumnSpacing = 12, RowSpacing = 10 };
        output.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(92) });
        output.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        AddOutputRow(output, 0, "Format", _format);
        _dimension.Header = null;
        _dimension.Width = 120;
        AddOutputRow(output, 1, "Longest side", Row(_dimension, Meta("px, up to 8192")));
        var chooseFolder = Secondary(ActionButton("Choose…", ChooseDestinationAsync));
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(chooseFolder, "Choose a folder to save to");
        ToolTipService.SetToolTip(chooseFolder, "Choose a folder to save to");
        AddOutputRow(output, 2, "Save to", Row(_destination, chooseFolder));
        Inputs.Children.Add(V3Panel("02 · Output", null, output, _detail));

        _preview = Secondary(ActionButton("Preview", PreviewAsync));
        _execute = Primary(ActionButton("Export…", ExecuteAsync));
        _undo = Secondary(ActionButton("Undo last export…", UndoAsync));

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
        previewPane.Children.Add(Meta("Only tools this PC can run are offered. Photo exports remove location and camera details. Chapter files list chapters, not speech subtitles. Video exports don't stabilize, upscale or track subjects. Check reframed videos before sharing them. If an export seems stuck, press Show last export before trying again."));
        UseSidePane(new ScrollViewer { Content = previewPane, VerticalScrollBarVisibility = ScrollBarVisibility.Auto }, 380, paneOnLeft: false);
        _cancel = ControlButton("Cancel current export", CancelAsync);
        _files.SelectionChanged += (_, _) => Invalidate();
        _kind.SelectionChanged += (_, _) =>
        {
            _format.ItemsSource = Capability?.OutputFormats;
            _format.SelectedIndex = Capability?.OutputFormats.Count > 0 ? 0 : -1;
            _detail.Text = PlainDetail(Capability);
            ToolTipService.SetToolTip(_detail, Capability?.Detail);
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

    private static string PlainDetail(ToolCapability? capability) => capability switch
    {
        null => "Choose what to export.",
        { Available: false } => "Not available on this PC yet. " + capability.Detail,
        { Id: "video" } => "Makes a new MP4 at the size and shape you choose. Your original video stays as it is.",
        { Id: "chapters" } => "Saves a video's chapters as a file other apps can read.",
        { Id: "photo" } => "Makes a new copy at the format and size you choose. Location and camera details are removed, and your original stays as it is.",
        _ => capability.Detail,
    };

    private void SyncFind()
    {
        var empty = string.IsNullOrWhiteSpace(_query.Text);
        _find.IsEnabled = !empty;
        ToolTipService.SetToolTip(_find, empty ? "Type part of a file name or what's in it first" : "Search the library");
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
        ToolTipService.SetToolTip(_preview, _preview.IsEnabled
            ? "Show the files this export will create, before writing anything"
            : _execution.IsActive ? "Wait for the current export to finish"
            : _files.SelectedItems.Count == 0 ? "Tick at least one file first"
            : _destinationPath is null ? "Choose a folder to save to first"
            : "Choose a format and a size this tool supports");
        ToolTipService.SetToolTip(_execute, _execute.IsEnabled
            ? "Create the new files in the chosen folder"
            : _execution.IsActive ? "Wait for the current export to finish"
            : "Press Preview first to check what will be created");
        ToolTipService.SetToolTip(_undo, _undo.IsEnabled ? "Remove the files the last export created" : "Nothing exported yet");
    }
    private void Invalidate()
    {
        if (_plan.PreviewID is not null) Status.Text = "Something changed. Press Preview again before exporting.";
        _plan.Invalidate();
        UpdateControls();
    }
    private async Task SearchAsync(CancellationToken ct)
    {
        if (string.IsNullOrWhiteSpace(_query.Text)) throw new InvalidOperationException("Type part of a file name or what's in it first.");
        var response = await Engine.CatalogAsync(CatalogRequest("search", _query.Text, mode: "keyword"), ct);
        await UI(() =>
        {
            _files.ItemsSource = response.Status == "ok" ? response.Hits.DistinctBy(hit => hit.FileID).Select(hit => new HitItem(hit)).ToArray() : Array.Empty<HitItem>();
            _files.SelectedItems.Clear();
            Invalidate();
            Status.Text = response.Message ?? (response.Hits.Count == 0
                ? "No matches. Try another word."
                : $"{response.Hits.Count:N0} match{(response.Hits.Count == 1 ? "" : "es")}. Tick the files to export.");
        }, ct);
    }
    private async Task ChooseDestinationAsync(CancellationToken ct)
    {
        var chosen = await FolderPickerService.PickFolderAsync(_hwnd);
        await UI(() =>
        {
            if (chosen.Path is { } path) { _destinationPath = path; _destination.Text = path; Invalidate(); Status.Text = "Folder chosen. Press Preview to check what will be created."; }
            else
            {
                Status.Text = chosen.FailureReason ?? "No folder chosen.";
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
            throw new InvalidOperationException("Tick at least one file, choose a folder to save to, and pick a format and size this tool supports.");
        var ids = _files.SelectedItems.Cast<HitItem>().Select(item => item.Hit.FileID).Distinct().Order().ToArray();
        return SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "preview", ids, _destinationPath,
            new ToolRecipe(capability.Id, format, (uint)_dimension.Value)), ct);
    }
    private async Task ExecuteAsync(CancellationToken ct)
    {
        if (_execution.IsActive || _execution.NeedsRecovery || !_plan.CanExecute) throw new InvalidOperationException("Press Preview first. If an export is still running, wait for it to finish.");
        if (!await ConfirmAsync("Export new files?", "FileID creates the files shown in the preview. Your originals stay as they are. Photo exports remove location and camera details. " + ToolUndoIntegration.Detail)) return;
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
        if (!await ConfirmAsync("Undo export?", "FileID moves the exported files it created, if you haven't changed them, into its own recovery folder. Files you edited are kept. Your originals are not changed.")) return;
        ct.ThrowIfCancellationRequested();
        await SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "undo", OperationID: id), ct);
    }
    private async Task CancelAsync(CancellationToken ct)
    {
        var generation = Engine.SpawnGeneration;
        if (!_cancelAvailable || _capabilitiesGeneration != generation)
            throw new InvalidOperationException("This export can't be stopped. Wait for it to finish.");
        var id = _execution.RequireCancellationOwner(generation);
        _execution.BeginCancellation(generation);
        UpdateControls();
        Status.Text = "Stopping…";
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
        public override string ToString() => Name + (Capability.Available ? "" : " · not available");

        private string Name => Capability.Id switch
        {
            "photo" => "Photo",
            "video" => "Video",
            "chapters" => "Chapters",
            "videoEnhancement" => "Video enhancement",
            _ => Capability.Id,
        };
    }
}
