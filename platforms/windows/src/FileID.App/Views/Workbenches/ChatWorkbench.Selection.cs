using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Workbenches;

/// <summary>Questions scoped to selected files (macOS <c>ChatScopeChip</c> and
/// <c>SelectionResultsStrip</c>): the scope chip above the composer, and the
/// matches / not-sure tiles under the latest answer, which can be chosen again
/// for the next question, opened, or saved as a Best Takes group.</summary>
internal sealed partial class ChatWorkbench
{
    private readonly StackPanel _scopeRow = new() { Orientation = Orientation.Horizontal, Spacing = 6, Visibility = Visibility.Collapsed };
    private readonly StackPanel _selectionHost = new() { Spacing = 10, Margin = new Thickness(12, 4, 12, 4), Visibility = Visibility.Collapsed };
    private readonly TextBlock _selectionNote = Meta("");
    private readonly HashSet<long> _chosen = new();
    private readonly List<ToggleButton> _selectionTiles = new();
    private readonly Brush _scopeFill = new SolidColorBrush(Color.FromArgb(0x1F, 0xFF, 0xCC, 0x00));
    private readonly Brush _badgeFill = new SolidColorBrush(Color.FromArgb(0x73, 0x00, 0x00, 0x00));
    private IReadOnlyList<CatalogHit> _selectionHits = Array.Empty<CatalogHit>();
    private string? _scopedRequestID;
    private string _lastQuestion = "";
    private Button? _askSelection;
    private Button? _groupSelection;
    private Button? _allSelection;
    private ThumbnailService? _selectionThumbs;

    private void OnChatScopeChanged(object? sender, EventArgs e)
        => DebugLog.SafeRun("ChatWorkbench.OnChatScopeChanged", () => DispatcherQueue.TryEnqueue(SyncScope));

    private void SyncScope()
    {
        var scope = AppViewModel.Instance.ChatScope;
        _scopeRow.Children.Clear();
        _scopeRow.Visibility = scope is null ? Visibility.Collapsed : Visibility.Visible;
        _input.PlaceholderText = scope is null ? "Ask about your files (up to 2,000 characters)" : "Ask about the selected files";
        if (scope is null) return;
        var label = TextLine(scope.Label, 12, _gold);
        label.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        label.VerticalAlignment = VerticalAlignment.Center;
        var remove = Ghost(new Button { Content = new FontIcon { Glyph = "", FontSize = 9 }, Padding = new Thickness(4), MinWidth = 22, MinHeight = 22 });
        ToolTipService.SetToolTip(remove, "Ask about the whole library again");
        AutomationProperties.SetName(remove, $"Remove {scope.Label} from the question");
        remove.Click += (_, _) => DebugLog.SafeRun("ChatWorkbench.RemoveScope", () => AppViewModel.Instance.ChatScope = null);
        var content = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        content.Children.Add(label);
        content.Children.Add(remove);
        var chip = new Border
        {
            Child = content,
            Padding = new Thickness(9, 2, 4, 2),
            CornerRadius = new CornerRadius(13),
            Background = _scopeFill,
            BorderBrush = _chipActiveStroke,
            BorderThickness = new Thickness(1),
        };
        AutomationProperties.SetName(chip, $"Question scope: {scope.Label}");
        _scopeRow.Children.Add(chip);
    }

    /// <summary>Notes a send so a completed scoped answer can narrow the next question.</summary>
    private void BeginScopedSend(ChatRequest request)
    {
        _scopedRequestID = request.ScopeFileIDs is null ? null : request.RequestID;
        _lastQuestion = request.Text ?? "";
        _chosen.Clear();
        _selectionNote.Text = "";
    }

    private void NarrowScope(ChatResponse response)
    {
        if (response.Status != "completed" || response.RequestID != _scopedRequestID) return;
        if (ChatSelectionPolicy.Narrowed(response.Hits) is { } narrowed) AppViewModel.Instance.ChatScope = narrowed;
    }

    private void RenderSelection(IReadOnlyList<CatalogHit> hits)
    {
        if (!ChatSelectionPolicy.HasResults(hits))
        {
            _selectionHost.Visibility = Visibility.Collapsed;
            return;
        }
        _selectionHost.Visibility = Visibility.Visible;
        if (SameResults(hits, _selectionHits)) return;
        _selectionHits = hits;
        _chosen.RemoveWhere(id => !hits.Any(hit => hit.FileID == id));
        _selectionTiles.Clear();
        _selectionHost.Children.Clear();
        _selectionThumbs ??= new ThumbnailService();
        AddSelectionRow("Matches", "yes", _gold, hits);
        AddSelectionRow("Not sure", "unsure", _ai, hits);
        _askSelection = Secondary(new Button { Content = "Ask about these", Height = 28, MinHeight = 28 });
        ToolTipService.SetToolTip(_askSelection, "Ask the next question about only these files");
        _askSelection.Click += (_, _) => DebugLog.SafeRun("ChatWorkbench.AskSelection", AskAboutChosen);
        _groupSelection = Secondary(new Button { Content = "Make a group", Height = 28, MinHeight = 28 });
        _groupSelection.Click += (_, _) => Run(MakeGroupAsync);
        _allSelection = Ghost(new Button { Content = "Select all", Height = 28, MinHeight = 28 });
        _allSelection.Click += (_, _) => DebugLog.SafeRun("ChatWorkbench.SelectAllResults", ToggleAllResults);
        _selectionHost.Children.Add(Row(_askSelection, _groupSelection, _allSelection));
        _selectionHost.Children.Add(_selectionNote);
        SyncSelectionButtons();
    }

    private static bool SameResults(IReadOnlyList<CatalogHit> a, IReadOnlyList<CatalogHit> b) =>
        a.Count == b.Count && a.Zip(b).All(pair => pair.First.FileID == pair.Second.FileID && pair.First.Match == pair.Second.Match && pair.First.Text == pair.Second.Text);

    private void AddSelectionRow(string title, string verdict, Brush tone, IReadOnlyList<CatalogHit> hits)
    {
        var items = hits.Where(hit => hit.Match == verdict).ToArray();
        if (items.Length == 0) return;
        var heading = SectionLabel($"{title} · {items.Length}");
        heading.Foreground = tone;
        _selectionHost.Children.Add(heading);
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Padding = new Thickness(3) };
        foreach (var hit in items) row.Children.Add(SelectionTile(hit, tone));
        _selectionHost.Children.Add(new ScrollViewer
        {
            Content = row,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
            HorizontalScrollMode = ScrollMode.Enabled,
            VerticalScrollMode = ScrollMode.Disabled,
        });
    }

    private StackPanel SelectionTile(CatalogHit hit, Brush tone)
    {
        var image = new Image { Width = 132, Height = 92, Stretch = Stretch.UniformToFill };
        LoadSelectionThumb(image, hit.Path);
        var badge = new Border
        {
            Child = new FontIcon { Glyph = hit.Match == "yes" ? "" : "", FontSize = 12, Foreground = tone },
            Background = _badgeFill,
            CornerRadius = new CornerRadius(10),
            Padding = new Thickness(4),
            Margin = new Thickness(4),
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Top,
        };
        var picture = new Grid { Width = 132, Height = 92, CornerRadius = new CornerRadius(8) };
        picture.Children.Add(image);
        picture.Children.Add(badge);
        var tile = new ToggleButton { Content = picture, Padding = new Thickness(0), CornerRadius = new CornerRadius(8), BorderThickness = new Thickness(1), BorderBrush = _hairlineStrong, Tag = hit.FileID };
        AutomationProperties.SetName(tile, ChatSelectionPolicy.MatchSpoken(hit));
        AutomationProperties.SetHelpText(tile, "Toggles this file for the next question. Double-click opens it.");
        ToolTipService.SetToolTip(tile, $"{Path.GetFileName(hit.Path)} · {hit.Text}");
        tile.ContextFlyout = RevealMenu(hit.Path);
        tile.Checked += (_, _) => DebugLog.SafeRun("ChatWorkbench.ChooseResult", () => { _chosen.Add(hit.FileID); tile.BorderBrush = _gold; SyncSelectionButtons(); });
        tile.Unchecked += (_, _) => DebugLog.SafeRun("ChatWorkbench.UnchooseResult", () => { _chosen.Remove(hit.FileID); tile.BorderBrush = _hairlineStrong; SyncSelectionButtons(); });
        tile.DoubleTapped += (_, _) => DebugLog.SafeRun("ChatWorkbench.OpenResult", () => OpenSource(IndexOfSource(hit.FileID)));
        _selectionTiles.Add(tile);
        var reason = TextLine(hit.Text, 11.5, _text2);
        reason.TextWrapping = TextWrapping.Wrap;
        reason.MaxLines = 3;
        reason.Width = 132;
        var column = new StackPanel { Spacing = 5 };
        column.Children.Add(tile);
        column.Children.Add(reason);
        return column;
    }

    private int IndexOfSource(long fileID)
    {
        for (var i = 0; i < _sourceHits.Count; i++) if (_sourceHits[i].FileID == fileID) return i;
        return -1;
    }

    private void LoadSelectionThumb(Image target, string path)
    {
        var thumbs = _selectionThumbs;
        if (thumbs is null) return;
        var ui = DispatcherQueue;
        var ct = Lifetime;
        _ = DebugLog.SafeRunAsync("ChatWorkbench.SelectionThumbnail", async () =>
        {
            var bitmap = await thumbs.RequestAsync(path, null, ct).ConfigureAwait(false);
            if (bitmap is null || ct.IsCancellationRequested) return;
            ui.TryEnqueue(() =>
            {
                if (!ct.IsCancellationRequested) target.Source = bitmap;
            });
        });
    }

    private void SyncSelectionButtons()
    {
        if (_askSelection is null || _groupSelection is null || _allSelection is null) return;
        _askSelection.Content = ChatSelectionPolicy.AskLabel(_selectionHits, _chosen.Count);
        var count = ChatSelectionPolicy.Target(_selectionHits, _chosen).Count;
        _groupSelection.IsEnabled = count >= 2;
        ToolTipService.SetToolTip(_groupSelection, count < 2 ? "Choose at least two files" : "Save these as a group in Best Takes");
        var every = _chosen.Count == _selectionHits.Count;
        _allSelection.Content = every ? "Clear" : "Select all";
        AutomationProperties.SetName(_allSelection, every ? "Clear the selection" : "Select every file in this answer");
    }

    private void ToggleAllResults()
    {
        var every = _chosen.Count == _selectionHits.Count;
        foreach (var tile in _selectionTiles) tile.IsChecked = !every;
    }

    private void AskAboutChosen()
    {
        var ids = ChatSelectionPolicy.Target(_selectionHits, _chosen);
        var kinds = _selectionHits.Where(hit => ids.Contains(hit.FileID)).Select(hit => ChatSelectionPolicy.KindOf(hit.Path)).ToArray();
        AppViewModel.Instance.ChatScope = ChatSelectionPolicy.Selected(ids, kinds);
        foreach (var tile in _selectionTiles) tile.IsChecked = false;
        _input.Focus(FocusState.Programmatic);
    }

    private async Task MakeGroupAsync(CancellationToken ct)
    {
        var ids = ChatSelectionPolicy.Target(_selectionHits, _chosen);
        if (ids.Count < 2) throw new InvalidOperationException("Choose at least two files for a group.");
        var title = new string(_lastQuestion.Trim().Take(80).ToArray());
        if (title.Length == 0) title = "Picked in Chat";
        var request = new CatalogRequest(Guid.NewGuid().ToString(), "saveEvent", null, null, null, null, null, null,
            Event: new CatalogEvent(Guid.NewGuid().ToString(), title, title, ids, true));
        var response = await Engine.CatalogAsync(request, ct);
        await UI(() => _selectionNote.Text = response.Status == "ok"
            ? $"Saved {ids.Count} files as a group in Best Takes."
            : response.Message ?? "The group couldn't be saved. Try again.", ct);
    }

    private void DisposeSelection()
    {
        _selectionThumbs?.Dispose();
        _selectionThumbs = null;
    }
}
