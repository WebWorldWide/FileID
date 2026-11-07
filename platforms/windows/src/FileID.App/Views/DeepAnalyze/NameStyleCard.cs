using System.Numerics;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Theme.Controls;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Hosting;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.DeepAnalyze;

/// <summary>Describe Files ▸ Name style (macOS smartNamesCard "Name style" + Preview names):
/// the saved style, a proposeNames preview of the Library selection (or the files with
/// suggestions), editable names, and Rename through renameFiles with Undo.</summary>
internal sealed class NameStyleCard : UserControl
{
    private const double RowResponse = 0.35;
    private const double RowDamping = 0.8;

    private readonly Brush _gold = ThemeHelper.GetBrushSafe("GoldBrush");
    private readonly Brush _ai = ThemeHelper.GetBrushSafe("AiBrush");
    private readonly Brush _text2 = ThemeHelper.GetBrushSafe("Text2Brush");
    private readonly Brush _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
    private readonly Brush _chip = new SolidColorBrush(Color.FromArgb(0x1F, 0xB1, 0x9B, 0xCE));
    private readonly Brush _companionChip = new SolidColorBrush(Color.FromArgb(0x1F, 0xFF, 0xCC, 0x00));
    private readonly Style _rowContainer;
    private readonly ComboBox _style = new() { MinWidth = 180 };
    private readonly Slider _length = new() { Minimum = NameStylePolicy.MinLength, Maximum = NameStylePolicy.MaxLength, StepFrequency = 1, SmallChange = 1, LargeChange = 10, MinWidth = 220 };
    private readonly NumberBox _lengthBox = new() { Minimum = NameStylePolicy.MinLength, Maximum = NameStylePolicy.MaxLength, SmallChange = 1, LargeChange = 10, SpinButtonPlacementMode = NumberBoxSpinButtonPlacementMode.Compact, Width = 96 };
    private readonly TextBlock _lengthText = new() { VerticalAlignment = VerticalAlignment.Center };
    private readonly ToggleSwitch _people = new() { Header = "Add confirmed people" };
    private readonly ToggleSwitch _event = new() { Header = "Add event" };
    private readonly ToggleSwitch _date = new() { Header = "Add date" };
    private readonly ToggleSwitch _learn = new() { Header = "Learn from my renames" };
    private readonly TextBlock _learned = new() { TextWrapping = TextWrapping.Wrap, VerticalAlignment = VerticalAlignment.Center, MaxWidth = 560 };
    private readonly Button _reset = new() { Content = "Reset", IsEnabled = false };
    private readonly Button _preview = new() { Content = "Preview names" };
    private readonly Button _apply = new() { Content = "Rename", IsEnabled = false };
    private readonly ProgressRing _busy = new() { Width = 16, Height = 16, IsActive = false, Visibility = Visibility.Collapsed };
    private readonly TextBlock _status = new() { TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
    private readonly Grid _header;
    private readonly ListView _list = new() { SelectionMode = ListViewSelectionMode.None, MaxHeight = 440, Visibility = Visibility.Collapsed };
    private IReadOnlyList<NameProposalRow> _rows = Array.Empty<NameProposalRow>();
    private readonly Dictionary<long, RowView> _views = new();
    private bool _syncingChecks;
    private CancellationTokenSource? _previewCts;
    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _debounce;
    private int _generation;
    private bool _previewShown;
    private bool _syncing;
    private bool _applying;
    private bool _unloaded;

    public NameStyleCard()
    {
        _rowContainer = new Style(typeof(ListViewItem));
        _rowContainer.Setters.Add(new Setter(Control.HorizontalContentAlignmentProperty, HorizontalAlignment.Stretch));
        _rowContainer.Setters.Add(new Setter(Control.PaddingProperty, new Thickness(0)));
        _rowContainer.Setters.Add(new Setter(FrameworkElement.MinHeightProperty, 36.0));
        _list.ItemContainerStyle = _rowContainer;

        foreach (var (_, name) in NameStylePolicy.Styles) _style.Items.Add(name);
        AutomationProperties.SetName(_style, "Name style");
        AutomationProperties.SetName(_length, "Longest name in characters");
        AutomationProperties.SetName(_lengthBox, "Longest name in characters");
        _lengthText.Foreground = _text3;
        _learned.Foreground = _text3;
        _status.Foreground = _text2;
        AutomationProperties.SetName(_reset, "Reset learned name style");
        ToolTipService.SetToolTip(_reset, "Forget the style FileID learned from your renames");
        ToolTipService.SetToolTip(_learn, "When you edit a suggested name before renaming, later suggestions follow your style. Stays on this PC.");
        _preview.Style = ThemeHelper.GetStyleSafe("AccentButtonStyle");
        AutomationProperties.SetHelpText(_preview, "Show the new names before anything is renamed");

        var title = new TextBlock { Text = "Name style", Style = ThemeHelper.GetStyleSafe("SubtitleTextBlockStyle") };
        AutomationProperties.SetHeadingLevel(title, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level2);
        var caption = new TextBlock
        {
            Text = "How FileID builds new names from what it knows: chapters, descriptions, tags, events and the people you named. Nothing is renamed until you press Rename.",
            TextWrapping = TextWrapping.Wrap,
            Foreground = _text2,
            Style = ThemeHelper.GetStyleSafe("CaptionTextBlockStyle"),
        };

        var lengthRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        lengthRow.Children.Add(_length);
        lengthRow.Children.Add(_lengthBox);
        lengthRow.Children.Add(_lengthText);
        var styleRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 16 };
        styleRow.Children.Add(Labeled("Style", _style));
        styleRow.Children.Add(Labeled("Longest name", lengthRow));
        var toggles = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 24 };
        toggles.Children.Add(_people);
        toggles.Children.Add(_event);
        toggles.Children.Add(_date);
        toggles.Children.Add(_learn);
        var learnedRow = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        learnedRow.Children.Add(_learned);
        learnedRow.Children.Add(_reset);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10 };
        actions.Children.Add(_preview);
        actions.Children.Add(_apply);
        actions.Children.Add(_busy);

        _header = new Grid { ColumnSpacing = 10, Padding = new Thickness(12, 0, 12, 0), Visibility = Visibility.Collapsed };
        Columns(_header);
        AddHeader("Original", 1);
        AddHeader("Proposed", 2);
        AddHeader("Reason", 3);

        var stack = new StackPanel { Spacing = 12 };
        stack.Children.Add(title);
        stack.Children.Add(caption);
        stack.Children.Add(styleRow);
        stack.Children.Add(toggles);
        stack.Children.Add(learnedRow);
        stack.Children.Add(actions);
        stack.Children.Add(_status);
        stack.Children.Add(_header);
        stack.Children.Add(_list);
        Content = new GlassCard { Content = stack, HorizontalContentAlignment = HorizontalAlignment.Stretch };

        LoadSettings();
        _style.SelectionChanged += (_, _) => DebugLog.SafeRun("NameStyleCard.Style", OnStyleChanged);
        _length.ValueChanged += (_, e) => DebugLog.SafeRun("NameStyleCard.Length", () => OnLengthChanged(e.NewValue));
        _lengthBox.ValueChanged += (_, e) => DebugLog.SafeRun("NameStyleCard.LengthBox", () => OnLengthChanged(e.NewValue));
        _people.Toggled += (_, _) => DebugLog.SafeRun("NameStyleCard.People", OnStyleChanged);
        _event.Toggled += (_, _) => DebugLog.SafeRun("NameStyleCard.Event", OnStyleChanged);
        _date.Toggled += (_, _) => DebugLog.SafeRun("NameStyleCard.Date", OnStyleChanged);
        _learn.Toggled += (_, _) => DebugLog.SafeRun("NameStyleCard.Learn", OnLearnToggled);
        _reset.Click += (_, _) => DebugLog.SafeRun("NameStyleCard.Reset", () => _ = DebugLog.SafeRunAsync("NameStyleCard.ResetAsync", () => LoadLearnedAsync(true)));
        _preview.Click += (_, _) => DebugLog.SafeRun("NameStyleCard.Preview", () => _ = DebugLog.SafeRunAsync("NameStyleCard.PreviewAsync", PreviewAsync));
        _apply.Click += (_, _) => DebugLog.SafeRun("NameStyleCard.Apply", () => _ = DebugLog.SafeRunAsync("NameStyleCard.ApplyAsync", ApplyAsync));
        Unloaded += (_, _) => DebugLog.SafeRun("NameStyleCard.Unloaded", OnUnloaded);
        Loaded += (_, _) => DebugLog.SafeRun("NameStyleCard.Loaded", () =>
        {
            _unloaded = false;
            _ = DebugLog.SafeRunAsync("NameStyleCard.LearnedAsync", () => LoadLearnedAsync(false));
        });
    }

    private StackPanel Labeled(string label, UIElement control)
    {
        var panel = new StackPanel { Spacing = 4 };
        panel.Children.Add(new TextBlock { Text = label, Foreground = _text3, Style = ThemeHelper.GetStyleSafe("CaptionTextBlockStyle") });
        panel.Children.Add(control);
        return panel;
    }

    private static void Columns(Grid grid)
    {
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(32) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1.3, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
    }

    private void AddHeader(string text, int column)
    {
        var label = new TextBlock { Text = text, Foreground = _text3, Style = ThemeHelper.GetStyleSafe("CaptionTextBlockStyle") };
        Grid.SetColumn(label, column);
        _header.Children.Add(label);
    }

    private void LoadSettings()
    {
        _syncing = true;
        try
        {
            var settings = AppViewModel.Instance.Settings;
            var style = NameStylePolicy.SanitizeStyle(settings.NamingStyle);
            _style.SelectedIndex = Math.Max(0, NameStylePolicy.Styles.ToList().FindIndex(s => s.Id == style));
            var length = NameStylePolicy.ClampLength(settings.NamingMaxLength);
            _length.Value = length;
            _lengthBox.Value = length;
            _lengthText.Text = $"{length} characters";
            _people.IsOn = settings.NamingIncludePeople;
            _event.IsOn = settings.NamingIncludeEvent;
            _date.IsOn = settings.NamingIncludeDate;
            _learn.IsOn = settings.NamingLearn;
            _learned.Text = NameStylePolicy.LearnedSummary(Array.Empty<CatalogHit>(), settings.NamingLearn).Text;
            _date.IsEnabled = NameStylePolicy.DateToggleEnabled(style);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("NameStyleCard settings read failed: " + ex.Message);
        }
        finally { _syncing = false; }
    }

    private string SelectedStyle =>
        _style.SelectedIndex >= 0 && _style.SelectedIndex < NameStylePolicy.Styles.Count
            ? NameStylePolicy.Styles[_style.SelectedIndex].Id
            : NameStylePolicy.DefaultStyle;

    private CatalogNaming CurrentNaming() =>
        NameStylePolicy.Naming(SelectedStyle, NameStylePolicy.ClampLength(_length.Value), _people.IsOn, _event.IsOn, _date.IsOn, _learn.IsOn);

    private void OnLengthChanged(double value)
    {
        if (_syncing) return;
        var length = NameStylePolicy.ClampLength(value);
        _syncing = true;
        try
        {
            if ((int)_length.Value != length) _length.Value = length;
            if (double.IsNaN(_lengthBox.Value) || (int)_lengthBox.Value != length) _lengthBox.Value = length;
            _lengthText.Text = $"{length} characters";
        }
        finally { _syncing = false; }
        OnStyleChanged();
    }

    private void OnStyleChanged()
    {
        if (_syncing) return;
        var naming = CurrentNaming();
        _date.IsEnabled = NameStylePolicy.DateToggleEnabled(naming.Style);
        try
        {
            var settings = AppViewModel.Instance.Settings;
            settings.NamingStyle = naming.Style;
            settings.NamingMaxLength = naming.MaxLength;
            settings.NamingIncludePeople = _people.IsOn;
            settings.NamingIncludeEvent = _event.IsOn;
            settings.NamingIncludeDate = _date.IsOn;
            settings.NamingLearn = _learn.IsOn;
            settings.Save();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("NameStyleCard settings save failed: " + ex.Message);
        }
        if (_previewShown) SchedulePreview();
    }

    private void OnLearnToggled()
    {
        OnStyleChanged();
        if (!_syncing) _ = DebugLog.SafeRunAsync("NameStyleCard.LearnedAsync", () => LoadLearnedAsync(false));
    }

    private async Task LoadLearnedAsync(bool reset)
    {
        if (_unloaded) return;
        _reset.IsEnabled = false;
        try
        {
            var response = await EngineClient.Instance.CatalogAsync(
                NameStylePolicy.PreferencesRequest("names-learned-" + Guid.NewGuid().ToString("N"), reset));
            if (_unloaded || response.Status != "ok") return;
            ShowLearned(response.Hits);
            if (reset && _previewShown) SchedulePreview();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("NameStyleCard learned style read failed: " + ex.Message);
        }
    }

    private void ShowLearned(IEnumerable<CatalogHit> hits)
    {
        var (text, samples) = NameStylePolicy.LearnedSummary(hits, _learn.IsOn);
        _learned.Text = text;
        _reset.IsEnabled = samples > 0;
    }

    private async Task LearnFromAsync(IReadOnlyList<NameProposalRow> rows, IReadOnlyCollection<long> confirmed, CatalogNaming naming)
    {
        if (!naming.Learn) return;
        var edits = NameStylePolicy.Edits(rows, confirmed);
        if (edits.Count == 0) return;
        try
        {
            var response = await EngineClient.Instance.CatalogAsync(
                NameStylePolicy.LearnRequest("names-learn-" + Guid.NewGuid().ToString("N"), edits, naming));
            if (!_unloaded && response.Status == "ok") ShowLearned(response.Hits);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("NameStyleCard learning from renames failed: " + ex.Message);
        }
    }

    private void SchedulePreview()
    {
        if (_debounce is null)
        {
            _debounce = DispatcherQueue.CreateTimer();
            _debounce.IsRepeating = false;
            _debounce.Interval = NameStylePolicy.PreviewDelay;
            _debounce.Tick += (_, _) => DebugLog.SafeRun("NameStyleCard.Debounce", () => _ = DebugLog.SafeRunAsync("NameStyleCard.DebouncedPreview", PreviewAsync));
        }
        _debounce.Stop();
        _debounce.Start();
    }

    private void OnUnloaded()
    {
        _unloaded = true;
        _debounce?.Stop();
        _previewCts?.Cancel();
    }

    private static async Task<IReadOnlyList<long>> ResolveFileIdsAsync(CancellationToken ct)
    {
        var selection = SelectionRegistry.Instance.LibrarySelection;
        if (selection.Count > 0) return NameStylePolicy.FileIds(selection, Array.Empty<long>());
        using var store = new ReadStore(AppPaths.DbPath);
        await store.OpenAsync(ct).ConfigureAwait(true);
        var pending = await store.PendingProposedRenamesAsync(NameStylePolicy.MaxFiles, ct).ConfigureAwait(true);
        return NameStylePolicy.FileIds(Array.Empty<long>(), pending.Select(p => p.Id).ToArray());
    }

    private async Task PreviewAsync()
    {
        if (_unloaded) return;
        var generation = ++_generation;
        _previewCts?.Cancel();
        var cts = new CancellationTokenSource();
        _previewCts = cts;
        _previewShown = true;
        SetBusy(true, "Working out names…");
        try
        {
            var ids = await ResolveFileIdsAsync(cts.Token);
            if (generation != _generation || _unloaded) return;
            if (ids.Count == 0)
            {
                SetBusy(false, "Choose files in Library first, or describe some files so they have suggested names.");
                return;
            }
            var request = NameStylePolicy.Request("names-" + Guid.NewGuid().ToString("N"), ids, CurrentNaming());
            var response = await EngineClient.Instance.CatalogAsync(request, cts.Token);
            if (generation != _generation || _unloaded) return;
            if (response.Status != "ok")
            {
                SetBusy(false, response.Message ?? "FileID couldn't suggest names right now.");
                return;
            }
            _rows = NameStylePolicy.Rows(response.Hits, _rows);
            RenderRows();
            var ready = _rows.Count(r => r.CanCheck);
            SetBusy(false, ready == _rows.Count
                ? $"{ready:N0} new name{(ready == 1 ? "" : "s")}."
                : $"{ready:N0} new name{(ready == 1 ? "" : "s")} · {_rows.Count - ready:N0} keep their names.");
        }
        catch (OperationCanceledException)
        {
        }
        catch (Exception ex)
        {
            if (generation == _generation && !_unloaded) SetBusy(false, SqliteErrorTranslator.Humanize(ex));
        }
    }

    private void RenderRows()
    {
        _list.Items.Clear();
        _views.Clear();
        var animate = !ReducedMotion.Instance.IsReduced;
        foreach (var row in _rows) _list.Items.Add(BuildRow(row, animate));
        var any = _rows.Count > 0;
        _header.Visibility = any ? Visibility.Visible : Visibility.Collapsed;
        _list.Visibility = any ? Visibility.Visible : Visibility.Collapsed;
        SyncApply();
    }

    private sealed record RowView(Grid Grid, CheckBox Check, TextBox? Box, TextBlock? Follows);

    private Grid BuildRow(NameProposalRow row, bool animate)
    {
        var grid = new Grid { ColumnSpacing = 10, Padding = new Thickness(12, 4, 12, 4), Opacity = row.CanCheck ? 1 : 0.5 };
        Columns(grid);
        var check = new CheckBox { IsChecked = row.Checked, IsEnabled = row.CanCheck, MinWidth = 0, VerticalAlignment = VerticalAlignment.Center };
        AutomationProperties.SetName(check, "Rename " + row.CurrentName);
        var original = new TextBlock
        {
            Text = row.IsFollower ? "\u21B3 " + row.CurrentName : row.CurrentName,
            Foreground = _text2,
            TextTrimming = TextTrimming.CharacterEllipsis,
            VerticalAlignment = VerticalAlignment.Center,
            Margin = new Thickness(row.IsFollower ? 16 : 0, 0, 0, 0),
        };
        ToolTipService.SetToolTip(original, row.Path);
        UIElement proposed;
        TextBox? box = null;
        TextBlock? follows = null;
        if (row.CanCheck && row.IsFollower)
        {
            follows = new TextBlock { Text = row.Edited, Foreground = _gold, Opacity = row.Checked ? 0.85 : 0.45, TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center };
            ToolTipService.SetToolTip(follows, "Follows the name above");
            proposed = follows;
        }
        else if (row.CanCheck)
        {
            box = new TextBox { Text = row.Edited, Foreground = _gold, IsEnabled = row.Checked, VerticalAlignment = VerticalAlignment.Center };
            AutomationProperties.SetName(box, "New name for " + row.CurrentName);
            var editor = box;
            box.TextChanged += (_, _) => DebugLog.SafeRun("NameStyleCard.Edit", () =>
            {
                var followers = NameStylePolicy.Edit(_rows, row.FileId, editor.Text);
                AutomationProperties.SetName(grid, NameStylePolicy.AccessibleName(row, _rows));
                foreach (var follower in followers)
                {
                    if (!_views.TryGetValue(follower.FileId, out var view)) continue;
                    if (view.Follows is not null) view.Follows.Text = follower.Edited;
                    AutomationProperties.SetName(view.Grid, NameStylePolicy.AccessibleName(follower, _rows));
                }
                SyncApply();
            });
            proposed = box;
        }
        else
        {
            proposed = new TextBlock { Text = "Keeps its name", Foreground = _text3, VerticalAlignment = VerticalAlignment.Center };
        }
        if (row.CanCheck)
        {
            check.Checked += (_, _) => DebugLog.SafeRun("NameStyleCard.Check", () => OnChecked(row, true));
            check.Unchecked += (_, _) => DebugLog.SafeRun("NameStyleCard.Uncheck", () => OnChecked(row, false));
        }
        var chips = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Center };
        if (NameStylePolicy.CompanionChip(row, _rows) is { } companions)
        {
            var companionChip = new Border
            {
                Background = _companionChip,
                CornerRadius = new CornerRadius(4),
                Padding = new Thickness(7, 2, 7, 2),
                Child = new TextBlock { Text = companions, Foreground = _gold, FontSize = 11.5 },
            };
            ToolTipService.SetToolTip(companionChip, "Renamed together with this file");
            chips.Children.Add(companionChip);
        }
        chips.Children.Add(new Border
        {
            Background = _chip,
            CornerRadius = new CornerRadius(4),
            Padding = new Thickness(7, 2, 7, 2),
            Child = LearnedTip(new TextBlock { Text = NameStylePolicy.ReasonChip(row), Foreground = _ai, FontSize = 11.5 }, row),
        });
        Grid.SetColumn(original, 1);
        Grid.SetColumn((FrameworkElement)proposed, 2);
        Grid.SetColumn(chips, 3);
        grid.Children.Add(check);
        grid.Children.Add(original);
        grid.Children.Add(proposed);
        grid.Children.Add(chips);
        AutomationProperties.SetName(grid, NameStylePolicy.AccessibleName(row, _rows));
        _views[row.FileId] = new RowView(grid, check, box, follows);
        if (animate) grid.Loaded += (_, _) => DebugLog.SafeRun("NameStyleCard.RowIn", () => SpringIn(grid));
        return grid;
    }

    private static TextBlock LearnedTip(TextBlock chip, NameProposalRow row)
    {
        if (NameStylePolicy.LearnedHelp(row.Learned) is { } help) ToolTipService.SetToolTip(chip, help);
        return chip;
    }

    private void OnChecked(NameProposalRow row, bool isChecked)
    {
        if (_syncingChecks) return;
        _syncingChecks = true;
        try
        {
            foreach (var member in NameStylePolicy.SetChecked(_rows, row.FileId, isChecked))
            {
                if (!_views.TryGetValue(member.FileId, out var view)) continue;
                view.Check.IsChecked = isChecked;
                if (view.Box is not null) view.Box.IsEnabled = isChecked;
                if (view.Follows is not null) view.Follows.Opacity = isChecked ? 0.85 : 0.45;
            }
        }
        finally { _syncingChecks = false; }
        SyncApply();
    }

    private static void SpringIn(UIElement row)
    {
        ElementCompositionPreview.SetIsTranslationEnabled(row, true);
        var visual = ElementCompositionPreview.GetElementVisual(row);
        visual.Properties.InsertVector3("Translation", new Vector3(0, 8, 0));
        visual.Opacity = 0;
        SpringEasing.AnimateOpacity(row, 1f, RowResponse, RowDamping);
        SpringEasing.AnimateScalar(row, "Translation.Y", 0f, RowResponse, RowDamping);
    }

    private void SyncApply()
    {
        var count = NameStylePolicy.Renames(_rows).Count;
        _apply.Content = count == 0 ? "Rename" : $"Rename {count:N0} file{(count == 1 ? "" : "s")}";
        _apply.IsEnabled = count > 0 && !_applying;
        AutomationProperties.SetName(_apply, _apply.Content as string ?? "Rename");
    }

    private void SetBusy(bool busy, string status)
    {
        _busy.IsActive = busy;
        _busy.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
        _status.Text = status;
        _status.Visibility = string.IsNullOrEmpty(status) ? Visibility.Collapsed : Visibility.Visible;
    }

    private async Task ApplyAsync()
    {
        var entries = NameStylePolicy.Renames(_rows);
        if (entries.Count == 0 || _applying) return;
        _applying = true;
        SyncApply();
        SetBusy(true, "Renaming…");
        var rows = _rows;
        var naming = CurrentNaming();
        try
        {
            var engine = EngineClient.Instance;
            var result = await engine.WaitForBulkActionResultAsync(
                "renameFiles", () => engine.RenameFilesAsync(entries), TimeSpan.FromSeconds(30 + entries.Count / 4.0));
            var confirmed = BulkActionResultTruth.ConfirmedSuccessfulFileIds(result, entries.Select(e => e.FileId));
            var inverse = NameStylePolicy.UndoEntries(rows, entries, confirmed);
            if (inverse.Count > 0)
            {
                UndoStack.Instance.Push($"rename {inverse.Count} file{(inverse.Count == 1 ? "" : "s")}", async () =>
                {
                    try
                    {
                        var back = await engine.WaitForBulkActionResultAsync(
                            "renameFiles", () => engine.RenameFilesAsync(inverse), TimeSpan.FromSeconds(30 + inverse.Count / 4.0)).ConfigureAwait(false);
                        return back.Action == "renameFiles"
                            && BulkActionResultTruth.ConfirmsExactSuccess(back, inverse.Select(e => e.FileId).ToArray());
                    }
                    catch (Exception ex)
                    {
                        DebugLog.Warn("Name style rename undo failed: " + ex.Message);
                        return false;
                    }
                });
            }
            await LearnFromAsync(rows, confirmed, naming);
            var failed = entries.Count - confirmed.Count;
            var first = result.Messages.FirstOrDefault(m => !m.Ok)?.Message;
            var text = $"Renamed {confirmed.Count:N0} file{(confirmed.Count == 1 ? "" : "s")}"
                + (failed > 0 ? $" · {failed:N0} couldn't be renamed" + (first is null ? "" : $": {first}") : "")
                + (inverse.Count > 0 ? ". Press Ctrl+Z, or Undo in Library, to put the names back." : ".");
            _applying = false;
            if (_unloaded) return;
            await PreviewAsync();
            SetBusy(false, text);
        }
        catch (Exception ex)
        {
            _applying = false;
            if (!_unloaded) SetBusy(false, SqliteErrorTranslator.Humanize(ex));
        }
        finally
        {
            _applying = false;
            if (!_unloaded) SyncApply();
        }
    }
}
