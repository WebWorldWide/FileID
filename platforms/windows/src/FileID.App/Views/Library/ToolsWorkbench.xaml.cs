using System;
using System.ComponentModel;
using System.Linq;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Library;

public sealed partial class ToolsWorkbench : UserControl, IDisposable
{
    private string? _requestID;
    private string? _action;
    private int _generation;
    private string? _destination;
    private string? _operationID;
    private bool _executed;
    private bool _hasUndo;
    private bool _disposed;
    private bool _initialized;

    public ToolsWorkbench()
    {
        InitializeComponent();
        _initialized = true;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Loaded += (_, _) => Send("capabilities");
        Unloaded += (_, _) => Dispose();
        SetBusy(false);
    }

    private async void Send(string action, ToolRecipe? recipe = null)
    {
        if (_disposed || _requestID != null) return;
        var requestID = Guid.NewGuid().ToString();
        _requestID = requestID;
        _generation = EngineClient.Instance.SpawnGeneration;
        _action = action;
        SetBusy(true);
        Status.Text = "Working…";
        try
        {
            if (action == "search")
            {
                await EngineClient.Instance.SendCommandAsync(new CatalogRequestCommand(new CatalogRequest(requestID, "search", Query.Text.Trim(), null, null, null, null, null)));
            }
            else
            {
                var ids = action == "preview" ? Hits.SelectedItems.Cast<ListViewItem>().Select(item => ((CatalogHit)item.Tag).FileID).Distinct().ToArray() : null;
                var request = new ToolRequest(requestID, action, FileIDs: ids, Destination: _destination, Recipe: recipe,
                    OperationID: action is "execute" or "undo" ? _operationID : null);
                await EngineClient.Instance.SendCommandAsync(new ToolRequestCommand(request));
            }
        }
        catch (Exception)
        {
            DispatcherQueue.TryEnqueue(() =>
            {
                if (_disposed || _requestID != requestID) return;
                _requestID = null;
                SetBusy(false);
                Status.Text = "The engine could not receive this request. Refresh tools before retrying.";
            });
        }
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs args)
        => DebugLog.SafeRun("ToolsWorkbench.OnEngineChanged", () =>
        {
            if (args.PropertyName is not nameof(EngineClient.SpawnGeneration) and not nameof(EngineClient.State) and not nameof(EngineClient.LastToolResponse) and not nameof(EngineClient.LastCatalogResponse)) return;
            DebugLog.Info($"[ENGINE-SUB:ToolsWorkbench] {args.PropertyName}");
            if (args.PropertyName is nameof(EngineClient.SpawnGeneration) or nameof(EngineClient.State))
            {
                DispatcherQueue.TryEnqueue(() =>
                {
                    if (_disposed || (_generation == EngineClient.Instance.SpawnGeneration
                        && EngineClient.Instance.State == EngineClient.LifecycleState.Ready))
                    {
                        return;
                    }
                    _requestID = null;
                    Invalidate();
                    Status.Text = "The engine stopped or restarted. Refresh tools; use Last export to check any interrupted operation.";
                });
                return;
            }
            if (args.PropertyName == nameof(EngineClient.LastToolResponse) && EngineClient.Instance.LastToolResponse is { } tools)
                DispatcherQueue.TryEnqueue(() => DebugLog.SafeRun("ToolsWorkbench.Receive", () => Receive(tools)));
            if (args.PropertyName == nameof(EngineClient.LastCatalogResponse) && EngineClient.Instance.LastCatalogResponse is { } catalog)
                DispatcherQueue.TryEnqueue(() => DebugLog.SafeRun("ToolsWorkbench.ReceiveSearch", () => ReceiveSearch(catalog)));
        });

    private bool Accepts(string requestID) => !_disposed && requestID == _requestID
        && _generation == EngineClient.Instance.SpawnGeneration;

    private void ReceiveSearch(CatalogResponse response)
    {
        if (_action != "search" || !Accepts(response.RequestID)) return;
        _requestID = null;
        Hits.Items.Clear();
        if (response.Status == "ok")
        {
            foreach (var hit in response.Hits.DistinctBy(hit => hit.FileID))
                Hits.Items.Add(new ListViewItem { Content = hit.Path, Tag = hit });
        }
        Status.Text = response.Message ?? (response.Status == "ok" ? "Select files for export." : "Search failed.");
        Invalidate();
    }

    private void Receive(ToolResponse response)
    {
        if (!Accepts(response.RequestID)) return;
        var action = _action;
        _requestID = null;
        Status.Text = response.Message;
        if (action == "capabilities" && response.Status == "ok")
        {
            Kind.Items.Clear();
            foreach (var capability in response.Capabilities.Where(capability => capability.Available))
            {
                var label = capability.Id switch { "photo" => "Photo conversion", "chapters" => "Chapter export", "video" => "Video conversion", _ => capability.Id };
                Kind.Items.Add(new ComboBoxItem { Content = label, Tag = capability });
            }
            Kind.SelectedIndex = Kind.Items.Count > 0 ? 0 : -1;
        }
        else
        {
            if (action == "preview" && response.Status == "ok") { _operationID = response.OperationID; _executed = false; }
            if (action == "history" && response.Status == "ok") { _operationID = response.OperationID; _executed = response.OperationID != null; }
            if (action == "execute") _executed = true;
            if (action == "undo" && response.Status == "ok") { _operationID = null; _executed = false; }
            _hasUndo = response.Outputs.Any(output => output.State == "completed");
            Outputs.Items.Clear();
            foreach (var output in response.Outputs)
                Outputs.Items.Add(new ListViewItem { Content = $"{output.OutputPath}\n{output.State} · {output.Message}" });
        }
        SetBusy(false);
    }

    private void Invalidate()
    {
        if (!_initialized || _requestID != null) return;
        _operationID = null;
        _executed = false;
        _hasUndo = false;
        Outputs.Items.Clear();
        SetBusy(false);
    }

    private ToolRecipe? Recipe()
    {
        if (Kind.SelectedItem is not ComboBoxItem { Tag: ToolCapability capability } || Format.SelectedItem is not string format) return null;
        if (!double.IsFinite(Dimension.Value) || Dimension.Value != Math.Truncate(Dimension.Value) || Dimension.Value < 1 || Dimension.Value > 8192) return null;
        return new ToolRecipe(capability.Id, format, (uint)Dimension.Value, capability.Id == "photo" ? Enlarge.IsOn : null);
    }

    private void SetBusy(bool busy)
    {
        Query.IsEnabled = Search.IsEnabled = Refresh.IsEnabled = Hits.IsEnabled = Choose.IsEnabled = Kind.IsEnabled = Format.IsEnabled = !busy;
        Dimension.IsEnabled = !busy && Kind.SelectedItem is ComboBoxItem { Tag: ToolCapability { Id: "photo" } };
        Enlarge.IsEnabled = Dimension.IsEnabled;
        History.IsEnabled = !busy;
        Preview.IsEnabled = !busy && Hits.SelectedItems.Count > 0 && _destination != null && Recipe() != null;
        Export.IsEnabled = !busy && _operationID != null && !_executed;
        Undo.IsEnabled = !busy && _operationID != null && _hasUndo;
    }

    private void OnSearch(object sender, RoutedEventArgs args) { if (!string.IsNullOrWhiteSpace(Query.Text)) Send("search"); }
    private void OnRefresh(object sender, RoutedEventArgs args) => Send("capabilities");
    private void OnHistory(object sender, RoutedEventArgs args) => Send("history");
    private void OnPreview(object sender, RoutedEventArgs args) { if (Recipe() is { } recipe) Send("preview", recipe); }
    private void OnExport(object sender, RoutedEventArgs args) => Send("execute");
    private void OnUndo(object sender, RoutedEventArgs args) => Send("undo");
    private void OnSelectionChanged(object sender, SelectionChangedEventArgs args) => Invalidate();
    private void OnFormatChanged(object sender, SelectionChangedEventArgs args) => Invalidate();
    private void OnDimensionChanged(NumberBox sender, NumberBoxValueChangedEventArgs args) => Invalidate();
    private void OnEnlargeChanged(object sender, RoutedEventArgs args) => Invalidate();

    private void OnKindChanged(object sender, SelectionChangedEventArgs args)
    {
        if (!_initialized) return;
        Format.Items.Clear();
        if (Kind.SelectedItem is ComboBoxItem { Tag: ToolCapability capability })
        {
            foreach (var format in capability.OutputFormats) Format.Items.Add(format);
            Format.SelectedIndex = Format.Items.Count > 0 ? 0 : -1;
            Detail.Text = capability.Detail;
            Enlarge.Visibility = capability.Id == "photo" ? Visibility.Visible : Visibility.Collapsed;
            Dimension.Visibility = capability.Id == "photo" ? Visibility.Visible : Visibility.Collapsed;
        }
        Invalidate();
    }

    private async void OnChoose(object sender, RoutedEventArgs args)
        => await DebugLog.SafeRunAsync("ToolsWorkbench.OnChoose", async () =>
        {
            if (_disposed || _requestID != null) return;
            var hwnd = App.HostWindow is { } window ? WinRT.Interop.WindowNative.GetWindowHandle(window) : IntPtr.Zero;
            var result = await FolderPickerService.PickFolderAsync(hwnd);
            if (_disposed) return;
            if (result.Path != null) { _destination = result.Path; Destination.Text = result.Path; Invalidate(); }
            else if (result.FailureReason != null)
            {
                Status.Text = result.FailureReason;
            }
        });

    public void Dispose()
    {
        if (_disposed) return;
        _disposed = true;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }
}
