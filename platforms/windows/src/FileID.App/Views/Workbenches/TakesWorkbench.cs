using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Views.Workbenches;

/// <summary>Best Takes (DESIGN_V3_PORT "Best Takes"): the engine groups bursts and repeated clips by
/// itself and scores each try without a model. The user checks the starred best try, keeps all, skips,
/// or moves the rest to Trash with Undo. The goal-ranking panel is left out: the shared engine reports
/// it as unavailable.</summary>
internal sealed class TakesWorkbench : WorkbenchPanel
{
    private IReadOnlyList<CatalogTakeAutoGroup> _groups = Array.Empty<CatalogTakeAutoGroup>();
    private CatalogTakeAutoGroup? _current;
    private IReadOnlyList<CatalogTakeQuality> _qualities = Array.Empty<CatalogTakeQuality>();
    private readonly HashSet<long> _keep = new();
    private long _focused;
    private string? _lastBatch;
    private CatalogTakeDecision? _undoDecision;
    private bool _rendering;

    private readonly TextBlock _groupHeader = SectionLabel("Possible groups");
    private readonly ProgressBar _groupBar = new() { IsIndeterminate = true, Width = 40, Visibility = Visibility.Collapsed };
    private readonly ListView _groupList = new() { SelectionMode = ListViewSelectionMode.Single, Padding = new Thickness(6, 8, 6, 8) };
    private readonly TextBlock _title = new() { FontSize = 28 };
    private readonly TextBlock _date = Meta("");
    private readonly Border _skipped;
    private readonly Button _trash = Primary(new Button { Content = "Keep the best, move the other 0 to Trash" });
    private readonly Button _keepAll = Secondary(new Button { Content = "Keep all" });
    private readonly Button _skip = Ghost(new Button { Content = "Skip group" });
    private readonly Button _undo = new() { Content = "Undo", Visibility = Visibility.Collapsed };
    private readonly Image _bestImage = new() { Height = 340, Stretch = Stretch.Uniform };
    private readonly Image _otherImage = new() { Height = 340, Stretch = Stretch.Uniform };
    private readonly TextBlock _bestHeader = SectionLabel("Best");
    private readonly TextBlock _otherHeader = SectionLabel("This try");
    private readonly TextBlock _bestWhy = Meta("");
    private readonly TextBlock _otherWhy = Meta("");
    private readonly Button _bestKeep = Ghost(new Button());
    private readonly Button _otherKeep = Ghost(new Button());
    private readonly TextBlock _stripHeader = SectionLabel("All 0 photos · 0 to keep");
    private readonly StackPanel _strip = new() { Orientation = Orientation.Horizontal, Spacing = 8 };
    private readonly StackPanel _review = new() { Spacing = 16, Visibility = Visibility.Collapsed };
    private readonly TextBlock _empty = Meta("No tries to compare yet. After a scan, Best Takes groups bursts and repeated clips here by itself. Scan a folder with photo bursts or several tries of a clip.");

    public TakesWorkbench() : base("Best Takes")
    {
        _skipped = Chip("SKIPPED");
        _skipped.Visibility = Visibility.Collapsed;

        var lookAgain = Ghost(new Button { Content = "Look again" });
        lookAgain.Click += (_, _) => Run(LoadGroupsAsync);
        var header = new Grid { ColumnSpacing = 8, Padding = new Thickness(14, 10, 8, 4) };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(_groupBar, 1);
        Grid.SetColumn(lookAgain, 2);
        header.Children.Add(_groupHeader);
        header.Children.Add(_groupBar);
        header.Children.Add(lookAgain);
        var intro = Meta("Bursts and repeated clips from your scans. Biggest and newest first.");
        intro.Margin = new Thickness(14, 0, 14, 8);
        var pane = new Grid();
        pane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        pane.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        pane.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        Grid.SetRow(intro, 1);
        Grid.SetRow(_groupList, 2);
        pane.Children.Add(header);
        pane.Children.Add(intro);
        pane.Children.Add(_groupList);
        UseSidePane(pane, 280, paneOnLeft: true);

        _groupList.SelectionChanged += (_, _) =>
        {
            if (_rendering || _groupList.SelectedIndex < 0 || _groupList.SelectedIndex >= _groups.Count) return;
            var group = _groups[_groupList.SelectedIndex];
            if (_current?.GroupKey == group.GroupKey) return;
            Run(ct => ScoreAsync(group, ct));
        };

        _trash.Click += async (_, _) =>
        {
            if (_current is not { } group || TakeReviewPolicy.TrashIds(_qualities, _keep).Count == 0) return;
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = TakeReviewPolicy.ConfirmHeading(group, _qualities, _keep),
                Content = TakeReviewPolicy.ConfirmBody(_qualities, _keep),
                PrimaryButtonText = "Move to Trash",
                CloseButtonText = "Cancel",
                DefaultButton = ContentDialogButton.Close,
            };
            if (await dialog.ShowAsync() == ContentDialogResult.Primary) Run(TrashAsync);
        };
        _keepAll.Click += (_, _) => Run(ct => DecideAsync("keepAll", "Kept all. This group won't be suggested again.", ct));
        _skip.Click += (_, _) => Run(ct => DecideAsync("skip", "Skipped. It's at the bottom of the list.", ct));
        _undo.Click += (_, _) => Run(UndoAsync);
        _bestKeep.Click += (_, _) => ToggleOnPane(true);
        _otherKeep.Click += (_, _) => ToggleOnPane(false);

        var heading = Row(_title, _date, _skipped);
        heading.VerticalAlignment = VerticalAlignment.Bottom;
        _review.Children.Add(heading);
        _review.Children.Add(Meta("Tries of the same shot, taken moments apart. FileID starred the one that looks best. Check it, then keep it and clear out the rest."));
        _review.Children.Add(Row(_trash, _keepAll, _skip, _undo));
        var compare = new Grid { ColumnSpacing = 12 };
        compare.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        compare.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        var best = V3Panel("Best try", null, _bestHeader, _bestImage, _bestWhy, _bestKeep);
        var other = V3Panel("Compare with", null, _otherHeader, _otherImage, _otherWhy, _otherKeep);
        Grid.SetColumn(other, 1);
        compare.Children.Add(best);
        compare.Children.Add(other);
        _review.Children.Add(compare);
        _review.Children.Add(_stripHeader);
        _review.Children.Add(new ScrollViewer
        {
            Content = _strip,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Auto,
            VerticalScrollBarVisibility = ScrollBarVisibility.Disabled,
            HorizontalScrollMode = ScrollMode.Enabled,
        });
        Inputs.Children.Add(_empty);
        Inputs.Children.Add(_review);
    }

    protected override void OnOpened() => Run(LoadGroupsAsync);

    private static string DateText(double seconds) =>
        DateTimeOffset.FromUnixTimeSeconds((long)seconds).ToLocalTime().ToString("ddd d MMM, h:mm tt");

    private static FileID.IpcSchema.CatalogRequest Request(string action, IReadOnlyList<long>? fileIds = null, CatalogTakeDecision? decision = null, int? limit = null) =>
        new(Guid.NewGuid().ToString(), action, null, null, null, null, null, fileIds, Limit: limit, TakeDecision: decision);

    private async Task LoadGroupsAsync(CancellationToken ct)
    {
        await UI(() => _groupBar.Visibility = Visibility.Visible, ct);
        try
        {
            var response = await Engine.CatalogAsync(Request("autoTakeGroups", limit: 100), ct);
            if (response.Status != "ok") throw new InvalidOperationException(response.Message ?? "Couldn't look for groups.");
            var groups = response.TakeGroups ?? Array.Empty<CatalogTakeAutoGroup>();
            CatalogTakeAutoGroup? next = null;
            await UI(() => next = RenderGroups(groups), ct);
            if (next is not null) await ScoreAsync(next, ct);
        }
        finally
        {
            await UI(() => _groupBar.Visibility = Visibility.Collapsed, ct);
        }
    }

    private CatalogTakeAutoGroup? RenderGroups(IReadOnlyList<CatalogTakeAutoGroup> groups)
    {
        _rendering = true;
        try
        {
            _groups = groups;
            _groupHeader.Text = $"POSSIBLE GROUPS · {TakeReviewPolicy.OpenCount(groups)}";
            _groupList.Items.Clear();
            foreach (var group in groups)
            {
                var date = DateText(group.CapturedAt);
                var title = new TextBlock { Text = TakeReviewPolicy.GroupTitle(group), FontWeight = Microsoft.UI.Text.FontWeights.Medium };
                var meta = Meta(TakeReviewPolicy.IsOpen(group) ? date : $"{date} · Skipped");
                var stack = new StackPanel { Spacing = 2 };
                stack.Children.Add(title);
                stack.Children.Add(meta);
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(stack, TakeReviewPolicy.GroupSpoken(group, date));
                _groupList.Items.Add(stack);
            }
            var currentKey = _current?.GroupKey;
            var keepIndex = -1;
            for (var i = 0; i < groups.Count; i++)
            {
                if (groups[i].GroupKey == currentKey) keepIndex = i;
            }
            if (keepIndex >= 0)
            {
                _groupList.SelectedIndex = keepIndex;
                return null;
            }
            _current = null;
            _qualities = Array.Empty<CatalogTakeQuality>();
            if (TakeReviewPolicy.FirstOpen(groups) is { } first)
            {
                _groupList.SelectedIndex = first;
                return groups[first];
            }
            _review.Visibility = Visibility.Collapsed;
            _empty.Visibility = Visibility.Visible;
            return null;
        }
        finally
        {
            _rendering = false;
        }
    }

    private async Task ScoreAsync(CatalogTakeAutoGroup group, CancellationToken ct)
    {
        await UI(() =>
        {
            _current = group;
            _undo.Visibility = Visibility.Collapsed;
            Status.Text = $"Checking sharpness, faces and exposure in {group.Shots.Count} {TakeReviewPolicy.Noun(group, group.Shots.Count)}…";
        }, ct);
        var ids = group.Shots.SelectMany(s => s.FileIDs).ToArray();
        var response = await Engine.CatalogAsync(Request("scoreTakes", ids), ct);
        if (response.Status != "ok")
        {
            await UI(() =>
            {
                _review.Visibility = Visibility.Collapsed;
                Status.Text = $"Couldn't score these photos. {response.Message}";
            }, ct);
            return;
        }
        var qualities = response.TakeQualities ?? Array.Empty<CatalogTakeQuality>();
        await UI(() => RenderReview(group, qualities), ct);
    }

    private void RenderReview(CatalogTakeAutoGroup group, IReadOnlyList<CatalogTakeQuality> qualities)
    {
        if (qualities.Count == 0)
        {
            _review.Visibility = Visibility.Collapsed;
            Status.Text = "The engine returned no scores for this group.";
            return;
        }
        _qualities = qualities;
        _keep.Clear();
        foreach (var id in TakeReviewPolicy.DefaultKeep(qualities)) _keep.Add(id);
        _focused = TakeReviewPolicy.Best(qualities)?.PrimaryFileID ?? 0;
        _title.Text = TakeReviewPolicy.GroupTitle(group);
        _date.Text = DateText(group.CapturedAt);
        _skipped.Visibility = TakeReviewPolicy.IsOpen(group) ? Visibility.Collapsed : Visibility.Visible;
        _skip.IsEnabled = TakeReviewPolicy.IsOpen(group);
        _empty.Visibility = Visibility.Collapsed;
        _review.Visibility = Visibility.Visible;
        Status.Text = "";
        RebuildStrip(group);
        Refresh(group);
    }

    private void RebuildStrip(CatalogTakeAutoGroup group)
    {
        _strip.Children.Clear();
        var bestId = TakeReviewPolicy.Best(_qualities)?.PrimaryFileID;
        for (var i = 0; i < _qualities.Count; i++)
        {
            var quality = _qualities[i];
            var image = new Image { Width = 112, Height = 76, Stretch = Stretch.UniformToFill };
            LoadImage(image, quality.Path, 224);
            var star = quality.PrimaryFileID == bestId ? " ★" : "";
            var stack = new StackPanel { Spacing = 4 };
            stack.Children.Add(image);
            stack.Children.Add(new TextBlock { Text = $"{i + 1}{star}", FontFamily = new FontFamily("Consolas"), FontSize = 10.5 });
            var button = new Button { Content = stack, Padding = new Thickness(4) };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(button,
                $"Try {i + 1}{(star.Length > 0 ? ", the best" : "")}, {System.IO.Path.GetFileName(quality.Path)}");
            var id = quality.PrimaryFileID;
            button.Click += (_, _) => { _focused = id; if (_current is { } g) Refresh(g); };
            button.RightTapped += (_, e) => { e.Handled = true; Toggle(id); };
            _strip.Children.Add(button);
        }
    }

    private void Toggle(long primaryFileId)
    {
        TakeReviewPolicy.Toggle(_keep, primaryFileId);
        if (_current is { } group) Refresh(group);
    }

    private void ToggleOnPane(bool best)
    {
        if (TakeReviewPolicy.ComparePair(_qualities, _focused) is not { } pair) return;
        Toggle(_qualities[best ? pair.Best : pair.Other].PrimaryFileID);
    }

    private void Refresh(CatalogTakeAutoGroup group)
    {
        var (label, reason) = TakeReviewPolicy.TrashButton(_qualities, _keep);
        _trash.Content = label;
        _trash.IsEnabled = reason is null;
        ToolTipService.SetToolTip(_trash, reason);
        _stripHeader.Text = TakeReviewPolicy.FilmstripHeader(group, _keep, _qualities.Count).ToUpperInvariant();
        if (TakeReviewPolicy.ComparePair(_qualities, _focused) is not { } pair) return;
        var best = _qualities[pair.Best];
        var other = _qualities[pair.Other];
        _bestHeader.Text = $"BEST · {pair.Best + 1} OF {_qualities.Count}";
        _otherHeader.Text = (other.Rank == 2 && _focused == best.PrimaryFileID ? "RUNNER-UP" : "THIS TRY") + $" · {pair.Other + 1} OF {_qualities.Count}";
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_bestImage, $"Best try, {System.IO.Path.GetFileName(best.Path)}");
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_otherImage, $"{_otherHeader.Text}, {System.IO.Path.GetFileName(other.Path)}");
        LoadImage(_bestImage, best.Path, 1200);
        LoadImage(_otherImage, other.Path, 1200);
        _bestWhy.Text = Why(best);
        _otherWhy.Text = Why(other);
        _bestKeep.Content = _keep.Contains(best.PrimaryFileID) ? "Keeping ✓" : "Keep this one";
        _otherKeep.Content = _keep.Contains(other.PrimaryFileID) ? "Keeping ✓" : "Keep this one";
    }

    private static string Why(CatalogTakeQuality quality) =>
        $"{System.IO.Path.GetFileName(quality.Path)} — {(quality.Reasons.Count == 0 ? "No clear differences." : string.Join(" · ", quality.Reasons))}";

    private static void LoadImage(Image target, string path, int decodeWidth)
    {
        try
        {
            var image = new BitmapImage { DecodePixelWidth = decodeWidth };
            image.UriSource = new Uri(path);
            target.Source = image;
        }
        catch (Exception)
        {
            target.Source = null;
        }
    }

    private async Task TrashAsync(CancellationToken ct)
    {
        var ids = TakeReviewPolicy.TrashIds(_qualities, _keep);
        if (ids.Count == 0) return;
        await UI(() => Status.Text = "Moving to Trash…", ct);
        var result = await Engine.WaitForBulkActionResultAsync(
            "trashFiles",
            () => Engine.TrashFilesAsync(ids),
            TimeSpan.FromSeconds(120),
            ct: ct);
        if (result.Succeeded == 0)
        {
            var reason = result.Messages?.FirstOrDefault(m => !m.Ok)?.Message ?? "Nothing was moved.";
            await UI(() => Status.Text = $"Couldn't move to Trash. {reason}", ct);
            return;
        }
        _lastBatch = result.Action.Contains(':') ? result.Action[(result.Action.IndexOf(':') + 1)..] : null;
        _undoDecision = null;
        var failed = result.Failed > 0 ? $" · {result.Failed} couldn't be moved" : "";
        await UI(() =>
        {
            Status.Text = $"Moved {result.Succeeded} to Trash{failed}";
            _undo.Visibility = _lastBatch is null ? Visibility.Collapsed : Visibility.Visible;
        }, ct);
        await LoadGroupsAsync(ct);
    }

    private async Task DecideAsync(string kind, string message, CancellationToken ct)
    {
        if (_current is not { } group) return;
        var response = await Engine.CatalogAsync(Request("decideTakeGroup", decision: TakeReviewPolicy.Decision(group, kind)), ct);
        if (response.Status != "ok")
        {
            await UI(() => Status.Text = response.Message ?? "Couldn't save that decision.", ct);
            return;
        }
        _undoDecision = TakeReviewPolicy.Decision(group, "reopen");
        _lastBatch = null;
        await UI(() =>
        {
            Status.Text = message;
            _undo.Visibility = Visibility.Visible;
        }, ct);
        await LoadGroupsAsync(ct);
    }

    private async Task UndoAsync(CancellationToken ct)
    {
        if (_undoDecision is { } decision)
        {
            _undoDecision = null;
            await Engine.CatalogAsync(Request("decideTakeGroup", decision: decision), ct);
            await UI(() =>
            {
                Status.Text = "Put back in the list.";
                _undo.Visibility = Visibility.Collapsed;
            }, ct);
        }
        else if (_lastBatch is { } batch)
        {
            _lastBatch = null;
            var restored = await Engine.RestoreFromTrashAsync(batch);
            await UI(() =>
            {
                Status.Text = restored ? "Put the files back." : "Some files couldn't be put back. Check the Trash.";
                _undo.Visibility = Visibility.Collapsed;
            }, ct);
        }
        await LoadGroupsAsync(ct);
    }
}
