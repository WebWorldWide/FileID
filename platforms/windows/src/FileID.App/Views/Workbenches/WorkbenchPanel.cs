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
    protected StackPanel Body { get; } = new() { Spacing = 16, Margin = new Thickness(26, 20, 26, 20) };
    protected StackPanel Inputs { get; } = new() { Spacing = 10 };
    protected TextBlock Status { get; } = Text("Ready.");
    protected CancellationToken Lifetime => _lifetime?.Token ?? new CancellationToken(true);
    protected bool Busy { get; private set; }
    private CancellationTokenSource? _lifetime;
    private readonly ContentControl _inputHost = new();

    protected WorkbenchPanel(string title)
    {
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(this, title);
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

    /// <summary>Prototype v3 split: a fixed-width pane (pane fill) beside the
    /// scrolling page body, separated by a hairline (DESIGN_V3_PORT).</summary>
    protected void UseSidePane(UIElement pane, double width, bool paneOnLeft)
    {
        if (Content is ScrollViewer previous) previous.Content = null;
        Content = null;
        var grid = new Grid();
        var paneHost = new Border { Background = ThemeHelper.GetBrushSafe("PaneFillBrush"), Child = pane };
        var hairline = new Border { Background = ThemeHelper.GetBrushSafe("HairlineBrush") };
        var scroll = new ScrollViewer { Content = Body, VerticalScrollBarVisibility = ScrollBarVisibility.Auto };
        var columns = paneOnLeft
            ? new[] { new GridLength(width), new GridLength(1), new GridLength(1, GridUnitType.Star) }
            : new[] { new GridLength(1, GridUnitType.Star), new GridLength(1), new GridLength(width) };
        foreach (var column in columns) grid.ColumnDefinitions.Add(new ColumnDefinition { Width = column });
        Grid.SetColumn(paneHost, paneOnLeft ? 0 : 2);
        Grid.SetColumn(hairline, 1);
        Grid.SetColumn(scroll, paneOnLeft ? 2 : 0);
        grid.Children.Add(scroll);
        grid.Children.Add(hairline);
        grid.Children.Add(paneHost);
        Content = grid;
    }

    /// <summary>v3 panel: panel fill, radius 10, strong hairline; header row
    /// with an uppercase section label, optional trailing controls and a
    /// bottom hairline; content padded 14 / 12.</summary>
    protected static Border V3Panel(string header, UIElement? trailing, params UIElement[] content)
    {
        var headerGrid = new Grid
        {
            Padding = new Thickness(14, 10, 14, 10),
            BorderBrush = ThemeHelper.GetBrushSafe("HairlineBrush"),
            BorderThickness = new Thickness(0, 0, 0, 1),
            ColumnSpacing = 8,
        };
        headerGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        headerGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var label = SectionLabel(header);
        label.VerticalAlignment = VerticalAlignment.Center;
        headerGrid.Children.Add(label);
        if (trailing is FrameworkElement trailingElement)
        {
            trailingElement.VerticalAlignment = VerticalAlignment.Center;
            Grid.SetColumn(trailingElement, 1);
            headerGrid.Children.Add(trailingElement);
        }
        var body = new StackPanel { Spacing = 8, Padding = new Thickness(14, 12, 14, 12) };
        foreach (var element in content) body.Children.Add(element);
        var stack = new StackPanel();
        stack.Children.Add(headerGrid);
        stack.Children.Add(body);
        return new Border
        {
            Background = ThemeHelper.GetBrushSafe("PanelBrush"),
            BorderBrush = ThemeHelper.GetBrushSafe("HairlineStrongBrush"),
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(10),
            Child = stack,
        };
    }

    protected static TextBlock SectionLabel(string text)
    {
        var label = new TextBlock { Text = text.ToUpperInvariant() };
        if (ThemeHelper.GetStyleSafe("ShellSectionLabelStyle") is { } style) label.Style = style;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHeadingLevel(label, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level2);
        return label;
    }

    protected static TextBlock Meta(string text)
    {
        var meta = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
        if (ThemeHelper.GetStyleSafe("ShellMetaTextStyle") is { } style) meta.Style = style;
        meta.TextWrapping = TextWrapping.Wrap;
        return meta;
    }

    protected static Border Chip(string text)
    {
        var label = new TextBlock
        {
            Text = text,
            FontSize = 11.5,
            FontWeight = Microsoft.UI.Text.FontWeights.Medium,
            Foreground = ThemeHelper.GetBrushSafe("Text2Brush"),
        };
        try { if (Application.Current.Resources["BodyFontFamily"] is FontFamily family) label.FontFamily = family; }
        catch (Exception) { }
        return new Border
        {
            Padding = new Thickness(7, 2, 7, 2),
            CornerRadius = new CornerRadius(4),
            Background = ThemeHelper.GetBrushSafe("WhiteSubtleFillBrush"),
            Child = label,
        };
    }

    protected static T Styled<T>(T button, string style) where T : Button
    {
        if (ThemeHelper.GetStyleSafe(style) is { } resolved) button.Style = resolved;
        return button;
    }

    protected static T Primary<T>(T button) where T : Button => Styled(button, "ShellPrimaryButtonStyle");

    protected static T Secondary<T>(T button) where T : Button => Styled(button, "ShellSecondaryButtonStyle");

    protected static T Ghost<T>(T button) where T : Button => Styled(button, "ShellGhostButtonStyle");

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
    protected Button ControlButton(string label, Func<CancellationToken, Task> action)
    {
        var button = new Button { Content = label, IsEnabled = false };
        var running = false;
        button.Click += (_, _) => _ = DebugLog.SafeRunAsync("WorkbenchPanel.ControlButton", async () =>
        {
            if (running || Lifetime.IsCancellationRequested) return;
            running = true;
            var ct = Lifetime;
            button.IsEnabled = false;
            try { await action(ct); }
            catch (OperationCanceledException) when (ct.IsCancellationRequested) { }
            catch (Exception ex) { await UI(() => Status.Text = ex.Message, ct); }
            finally { running = false; await UI(UpdateControls, ct); }
        });
        Body.Children.Add(button);
        return button;
    }
    protected void Run(Func<CancellationToken, Task> action)
    {
        if (Busy || Lifetime.IsCancellationRequested) return;
        Busy = true;
        _inputHost.IsEnabled = false;
        Status.Text = "Working…";
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
