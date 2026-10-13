using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views.Workbenches;

internal abstract class WorkbenchPanel : UserControl
{
    protected EngineClient Engine { get; } = EngineClient.Instance;
    protected StackPanel Body { get; } = new() { Spacing = 12, Margin = new Thickness(20) };
    protected StackPanel Inputs { get; } = new() { Spacing = 10 };
    protected TextBlock Status { get; } = Text("Ready.");
    protected CancellationToken Lifetime => _lifetime?.Token ?? new CancellationToken(true);
    protected bool Busy { get; private set; }
    private CancellationTokenSource? _lifetime;
    private readonly ContentControl _inputHost = new();

    protected WorkbenchPanel(string title)
    {
        Body.Children.Add(new TextBlock
        {
            Text = title,
            FontSize = 24,
            FontWeight = Microsoft.UI.Text.FontWeights.SemiBold,
            Foreground = (Brush)Application.Current.Resources["GoldBrush"]
        });
        Body.Children.Add(Status);
        _inputHost.Content = Inputs;
        Body.Children.Add(_inputHost);
        Content = new ScrollViewer { Content = Body, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        Loaded += (_, _) =>
        {
            _lifetime?.Dispose();
            _lifetime = new CancellationTokenSource();
            Busy = false;
            _inputHost.IsEnabled = true;
            OnOpened();
        };
        Unloaded += (_, _) =>
        {
            _lifetime?.Cancel();
            _lifetime?.Dispose();
            _lifetime = null;
            OnClosed();
        };
    }

    protected virtual void OnOpened() { }
    protected virtual void OnClosed() { }
    protected static TextBlock Text(string text) => new() { Text = text, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
    protected static StackPanel Row(params UIElement[] elements)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        foreach (var element in elements) row.Children.Add(element);
        return row;
    }
    protected Button ActionButton(string label, Func<CancellationToken, Task> action)
    {
        var button = new Button { Content = label };
        button.Click += (_, _) => Run(action);
        return button;
    }
    protected void Run(Func<CancellationToken, Task> action)
    {
        if (Busy || Lifetime.IsCancellationRequested) return;
        Busy = true;
        _inputHost.IsEnabled = false;
        Status.Text = "Waiting for the local engine…";
        _ = RunCoreAsync(action, Lifetime);
    }
    private async Task RunCoreAsync(Func<CancellationToken, Task> action, CancellationToken ct)
    {
        try { await action(ct); }
        catch (OperationCanceledException) when (ct.IsCancellationRequested) { }
        catch (Exception ex) { await UI(() => Status.Text = ex.Message, ct); }
        finally
        {
            await UI(() => { Busy = false; _inputHost.IsEnabled = true; UpdateControls(); }, ct);
        }
    }
    protected virtual void UpdateControls() { }
    protected Task UI(Action action, CancellationToken ct)
    {
        if (ct.IsCancellationRequested) return Task.CompletedTask;
        var done = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
        if (!DispatcherQueue.TryEnqueue(() =>
        {
            try { if (!ct.IsCancellationRequested) action(); done.TrySetResult(); }
            catch (Exception ex) { done.TrySetException(ex); }
        }))
        {
            done.TrySetException(new InvalidOperationException("The panel dispatcher is unavailable."));
        }

        return done.Task;
    }
    protected async Task<bool> ConfirmAsync(string title, string message)
    {
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = title,
            Content = message,
            PrimaryButtonText = "Confirm",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close
        };
        return await dialog.ShowAsync() == ContentDialogResult.Primary;
    }
    protected static CatalogRequest CatalogRequest(string action, string? query = null, long? fileID = null,
        CatalogChapter? chapter = null, string? chapterID = null, string? jobID = null, string? mode = null) =>
        new(Guid.NewGuid().ToString(), action, query, fileID, chapter, chapterID, jobID, null, SearchMode: mode, Limit: action == "search" ? 100 : null);
    protected sealed record HitItem(CatalogHit Hit)
    {
        public override string ToString() => EvidencePanel.Label(Hit);
    }
}
