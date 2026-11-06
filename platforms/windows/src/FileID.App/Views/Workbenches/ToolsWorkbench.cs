using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.Storage;

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
    private IReadOnlyList<ToolCapability> _capabilities = Array.Empty<ToolCapability>();
    private readonly MediaToolChoice _media = new();
    private readonly ComboBox _tool = new() { MinWidth = 160 };
    private readonly ComboBox _container = new() { MinWidth = 120 };
    private readonly TextBlock _fixedFormat = Meta("");
    private readonly NumberBox _trimStart = new() { Minimum = 0, Maximum = 21600, SmallChange = 0.1, Width = 120 };
    private readonly NumberBox _trimEnd = new() { Minimum = 0, Maximum = 21600, SmallChange = 0.1, Width = 120 };
    private readonly ComboBox _trimMode = new() { MinWidth = 140 };
    private readonly ComboBox _proxySide = new() { MinWidth = 120 };
    private readonly ComboBox _rotation = new() { MinWidth = 140 };
    private readonly TextBlock _mediaNote = Meta("");
    private readonly MediaPlayerElement _playerView = new() { Height = 200, AreTransportControlsEnabled = true, Visibility = Visibility.Collapsed };
    private readonly List<(FrameworkElement Key, FrameworkElement Value, Func<bool> Visible)> _rows = new();
    private MediaPlayer? _player;
    private string? _playerPath;
    private int _playerRevision;
    private bool _syncingMedia;

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
        foreach (var container in MediaToolChoice.Containers) _container.Items.Add(new ComboBoxItem { Content = container.ToUpperInvariant(), Tag = container });
        _trimMode.ItemsSource = new[] { "Keyframe", "Exact" };
        _trimMode.SelectedIndex = 0;
        _proxySide.ItemsSource = MediaToolChoice.ProxySides.Select(side => $"{side} px").ToArray();
        _proxySide.SelectedIndex = 0;
        _rotation.ItemsSource = MediaToolChoice.Rotations.Select(rotation => rotation.Label).ToArray();
        _rotation.SelectedIndex = 0;
        _trimStart.NumberFormatter = TrimFormatter();
        _trimEnd.NumberFormatter = TrimFormatter();
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_trimStart, "Start time in seconds");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_trimEnd, "End time in seconds");
        var startHere = Ghost(ActionButton("Set from playhead", ct => SetFromPlayheadAsync(_trimStart)));
        var endHere = Ghost(ActionButton("Set from playhead", ct => SetFromPlayheadAsync(_trimEnd)));
        ToolTipService.SetToolTip(startHere, "Use the player's current time");
        ToolTipService.SetToolTip(endHere, "Use the player's current time");
        var row = 0;
        AddOutputRow(output, row++, "Tool", _tool, () => Capability?.Id == "video");
        AddOutputRow(output, row++, "Format", Row(_format, _container, _fixedFormat), () => true);
        _dimension.Header = null;
        _dimension.Width = 120;
        AddOutputRow(output, row++, "Longest side", Row(_dimension, Meta("px, up to 8192")), () => !IsMediaTool);
        AddOutputRow(output, row++, "Start", Row(_trimStart, Meta("s"), startHere), () => MediaTool == "trim");
        AddOutputRow(output, row++, "End", Row(_trimEnd, Meta("s"), endHere), () => MediaTool == "trim");
        AddOutputRow(output, row++, "Cut", _trimMode, () => MediaTool == "trim");
        AddOutputRow(output, row++, "Size", _proxySide, () => MediaTool == "proxy");
        AddOutputRow(output, row++, "Turn", _rotation, () => MediaTool == "rotate");
        var chooseFolder = Secondary(ActionButton("Choose…", ChooseDestinationAsync));
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(chooseFolder, "Choose a folder to save to");
        ToolTipService.SetToolTip(chooseFolder, "Choose a folder to save to");
        AddOutputRow(output, row, "Save to", Row(_destination, chooseFolder), () => true);
        Inputs.Children.Add(V3Panel("02 · Output", null, output, _mediaNote, _detail));

        _preview = Secondary(ActionButton("Preview", PreviewAsync));
        _execute = Primary(ActionButton("Export…", ExecuteAsync));
        _undo = Secondary(ActionButton("Undo last export…", UndoAsync));

        var previewPane = new StackPanel { Spacing = 10, Padding = new Thickness(14, 12, 14, 14) };
        previewPane.Children.Add(SectionLabel("03 · Preview"));
        previewPane.Children.Add(_playerView);
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
        _files.SelectionChanged += (_, _) => { _media.SettleContainer(SelectedPaths); SyncMediaControls(); Invalidate(); };
        _kind.SelectionChanged += (_, _) =>
        {
            _format.ItemsSource = Capability?.OutputFormats;
            _format.SelectedIndex = Capability?.OutputFormats.Count > 0 ? 0 : -1;
            SyncMediaControls();
            Invalidate();
        };
        _tool.SelectionChanged += (_, _) =>
        {
            if (_syncingMedia || _tool.SelectedItem is not ToolItem item) return;
            _media.Adopt(item.Id, SelectedPaths);
            SyncMediaControls();
            Invalidate();
        };
        _container.SelectionChanged += (_, _) =>
        {
            if (_syncingMedia || _container.SelectedItem is not ComboBoxItem { Tag: string container }) return;
            _media.Container = container;
            Invalidate();
        };
        _trimMode.SelectionChanged += (_, _) =>
        {
            if (_syncingMedia) return;
            _media.TrimExact = _trimMode.SelectedIndex == 1;
            _media.SettleContainer(SelectedPaths);
            SyncMediaControls();
            Invalidate();
        };
        _trimStart.ValueChanged += (_, _) => { if (!_syncingMedia && double.IsFinite(_trimStart.Value)) { _media.TrimStart = _trimStart.Value; Invalidate(); } };
        _trimEnd.ValueChanged += (_, _) => { if (!_syncingMedia && double.IsFinite(_trimEnd.Value)) { _media.TrimEnd = _trimEnd.Value; Invalidate(); } };
        _proxySide.SelectionChanged += (_, _) => { if (!_syncingMedia && _proxySide.SelectedIndex >= 0) { _media.ProxySide = MediaToolChoice.ProxySides[_proxySide.SelectedIndex]; Invalidate(); } };
        _rotation.SelectionChanged += (_, _) => { if (!_syncingMedia && _rotation.SelectedIndex >= 0) { _media.Rotation = MediaToolChoice.Rotations[_rotation.SelectedIndex].Degrees; Invalidate(); } };
        _format.SelectionChanged += (_, _) => Invalidate();
        _dimension.ValueChanged += (_, _) => Invalidate();
        SyncMediaControls();
        UpdateControls();
    }

    private static Windows.Globalization.NumberFormatting.DecimalFormatter TrimFormatter() => new()
    {
        FractionDigits = 2,
        IntegerDigits = 1,
        IsGrouped = false,
        NumberRounder = new Windows.Globalization.NumberFormatting.IncrementNumberRounder { Increment = 0.01 },
    };

    private string? MediaTool => Capability?.Id == "video" && !_media.IsExport ? _media.Tool : null;
    private bool IsMediaTool => MediaTool is not null;
    private IReadOnlyList<string> SelectedPaths => _files.SelectedItems.OfType<HitItem>().Select(item => item.Hit.Path).ToArray();
    private ToolCapability? RecipeCapability => MediaTool is { } tool ? _capabilities.FirstOrDefault(capability => capability.Id == tool) : Capability;

    /// <summary>Shows the rows for the chosen kind and video tool and greys out containers the recipe cannot take.</summary>
    private void SyncMediaControls()
    {
        _syncingMedia = true;
        try
        {
            foreach (var (key, value, visible) in _rows)
            {
                var visibility = visible() ? Visibility.Visible : Visibility.Collapsed;
                key.Visibility = visibility;
                value.Visibility = visibility;
            }
            var tool = MediaTool;
            _format.Visibility = tool is null ? Visibility.Visible : Visibility.Collapsed;
            _container.Visibility = tool is "trim" or "remux" or "rotate" ? Visibility.Visible : Visibility.Collapsed;
            _fixedFormat.Visibility = tool is "audio" or "proxy" ? Visibility.Visible : Visibility.Collapsed;
            _fixedFormat.Text = tool == "audio" ? "M4A · AAC" : "MP4 · H.264";
            var disabled = _media.DisabledContainers(SelectedPaths);
            foreach (var item in _container.Items.OfType<ComboBoxItem>())
            {
                item.IsEnabled = item.Tag is string container && !disabled.Contains(container);
                if (item.Tag as string == _media.Container) _container.SelectedItem = item;
            }
            _trimMode.SelectedIndex = _media.TrimExact ? 1 : 0;
            _mediaNote.Text = MediaNote(tool, _media.TrimExact);
            _mediaNote.Visibility = tool is null ? Visibility.Collapsed : Visibility.Visible;
            var capability = RecipeCapability;
            _detail.Text = PlainDetail(capability);
            ToolTipService.SetToolTip(_detail, capability?.Detail);
        }
        finally
        {
            _syncingMedia = false;
        }
        SyncPlayer();
    }

    private static string MediaNote(string? tool, bool exact) => tool switch
    {
        "trim" when exact => "Re-encodes to an H.264 MP4 so the cut lands on the exact frame. HDR videos are refused.",
        "trim" => "Copies the video without re-encoding. The new file may start a little early, at the keyframe before your start time.",
        "remux" => "Moves the video and sound into the other container without re-encoding. The container a file already has is greyed out.",
        "audio" => "Saves the soundtrack as AAC in an M4A file. Videos without sound are refused.",
        "proxy" => "A small H.264 copy for quick previews, linked to its original. Smaller videos aren't enlarged.",
        "rotate" => "Changes which way up the picture is shown, without re-encoding it.",
        _ => "",
    };

    /// <summary>Trim plays the first ticked file so its start and end can come from the playhead.</summary>
    private void SyncPlayer()
    {
        var path = MediaTool == "trim" && SelectedPaths is { Count: > 0 } paths ? paths[0] : null;
        if (path == _playerPath) return;
        _playerPath = path;
        var revision = ++_playerRevision;
        var previous = _player;
        _player = null;
        _playerView.SetMediaPlayer(null);
        previous?.Dispose();
        _playerView.Visibility = Visibility.Collapsed;
        if (path is null || !File.Exists(path)) return;
        Run(async ct =>
        {
            var file = await StorageFile.GetFileFromPathAsync(path);
            await UI(() =>
            {
                if (revision != _playerRevision) return;
                var player = new MediaPlayer { AutoPlay = false };
                player.MediaOpened += (_, _) => DebugLog.SafeRun("ToolsWorkbench.MediaOpened", () => DispatcherQueue.TryEnqueue(() =>
                {
                    if (revision != _playerRevision) return;
                    var seconds = player.PlaybackSession.NaturalDuration.TotalSeconds;
                    if (seconds > 0 && _media.TrimEnd <= _media.TrimStart) _trimEnd.Value = Math.Floor(seconds * 100) / 100;
                }));
                player.MediaFailed += (_, _) => DebugLog.SafeRun("ToolsWorkbench.MediaFailed", () => DispatcherQueue.TryEnqueue(() =>
                {
                    if (revision == _playerRevision) Status.Text = "Windows cannot play this video here. Type the start and end times instead.";
                }));
                _player = player;
                _playerView.SetMediaPlayer(player);
                _playerView.Visibility = Visibility.Visible;
                player.Source = MediaSource.CreateFromStorageFile(file);
            }, ct);
        });
    }

    private Task SetFromPlayheadAsync(NumberBox field)
    {
        if (_player is not { } player) throw new InvalidOperationException("Tick a video that can play here first.");
        field.Value = Math.Round(player.PlaybackSession.Position.TotalSeconds * 100) / 100;
        return Task.CompletedTask;
    }

    private void AddOutputRow(Grid grid, int row, string label, FrameworkElement value, Func<bool> visible)
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
        _rows.Add((key, value, visible));
    }

    private static string PlainDetail(ToolCapability? capability) => capability switch
    {
        null => "Choose what to export.",
        { Available: false } => "Not available on this PC yet. " + capability.Detail,
        { Id: "video" } => "Makes a new MP4 at the size and shape you choose. Your original video stays as it is.",
        { Id: "chapters" } => "Saves a video's chapters as a file other apps can read.",
        { Id: "photo" } => "Makes a new copy at the format and size you choose. Location and camera details are removed, and your original stays as it is.",
        { Id: "trim" } => "Keeps the part between the start and end times as a new file. Your original video stays as it is.",
        { Id: "remux" } => "Puts the same video and sound in a different container. Your original video stays as it is.",
        { Id: "audio" } => "Saves the soundtrack as a new audio file. Your original video stays as it is.",
        { Id: "proxy" } => "Makes a small preview copy linked to the original. Your original video stays as it is.",
        { Id: "rotate" } => "Saves a copy that plays the other way up. Your original video stays as it is.",
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
        AppViewModel.Instance.ToolFilesRequested -= OnToolFilesRequested;
        AppViewModel.Instance.ToolFilesRequested += OnToolFilesRequested;
        UseToolFiles();
        ApplyExecutionSnapshot();
        Run(async ct =>
        {
            await CapabilitiesAsync(ct);
            if (_execution.NeedsRecovery) await SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "history"), ct);
        });
    }
    protected override void OnClosed()
    {
        Engine.PropertyChanged -= OnEngineChanged;
        _player?.Pause();
    }

    private void OnToolFilesRequested(object? sender, EventArgs e)
        => DebugLog.SafeRun("ToolsWorkbench.OnToolFilesRequested", () => DispatcherQueue.TryEnqueue(UseToolFiles));

    /// <summary>Files Chat handed over: listed and ticked (up to the 100 an export takes).</summary>
    private void UseToolFiles()
    {
        if (AppViewModel.Instance.TakeToolFiles() is not { Count: > 0 } files) return;
        var items = files.DistinctBy(hit => hit.FileID).Select(hit => new HitItem(hit)).ToArray();
        _files.ItemsSource = items;
        _files.SelectedItems.Clear();
        foreach (var item in items.Take(100)) _files.SelectedItems.Add(item);
        Invalidate();
        Status.Text = items.Length > 100
            ? $"{items.Length:N0} files from Chat. The first 100 are ticked; an export takes up to 100."
            : $"{items.Length:N0} file{(items.Length == 1 ? "" : "s")} from Chat, ticked. Pick a format and a folder.";
    }
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
            if (_files.SelectedItems.Count is < 1 or > 100 || _destinationPath is null) return false;
            if (IsMediaTool) return _media.Recipe() is { } recipe && ToolPreviewState.Supports(RecipeCapability, recipe.Format, recipe.MaxDimension);
            var dimension = _dimension.Value;
            return double.IsFinite(dimension) && dimension == Math.Truncate(dimension) && dimension is >= 1 and <= 8192
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
        if (_execution.IsActive || _execution.NeedsRecovery || !CanPreview || Capability is not { } capability)
            throw new InvalidOperationException("Tick at least one file, choose a folder to save to, and pick a format and size this tool supports.");
        var recipe = IsMediaTool ? _media.Recipe()
            : _format.SelectedItem is string format ? new ToolRecipe(capability.Id, format, (uint)_dimension.Value) : null;
        if (recipe is null) throw new InvalidOperationException("Pick a format this tool supports.");
        var ids = _files.SelectedItems.Cast<HitItem>().Select(item => item.Hit.FileID).Distinct().Order().ToArray();
        return SendAsync(new ToolRequest(Guid.NewGuid().ToString(), "preview", ids, _destinationPath, recipe), ct);
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
                _capabilities = response.Capabilities;
                var recipes = response.Capabilities.Where(capability => capability.Id != "cancel" && !MediaToolChoice.IsMediaTool(capability.Id)).Select(capability => new CapabilityItem(capability)).ToArray();
                _kind.ItemsSource = recipes;
                _kind.SelectedIndex = recipes.Length > 0 ? 0 : -1;
                var tools = MediaToolChoice.Available(response.Capabilities).Select(tool => new ToolItem(tool.Id, tool.Label)).ToArray();
                if (!tools.Any(tool => tool.Id == _media.Tool)) _media.Adopt("export", SelectedPaths);
                _syncingMedia = true;
                _tool.ItemsSource = tools;
                _tool.SelectedItem = tools.FirstOrDefault(tool => tool.Id == _media.Tool);
                _syncingMedia = false;
                SyncMediaControls();
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
    private sealed record ToolItem(string Id, string Label)
    {
        public override string ToString() => Label;
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
