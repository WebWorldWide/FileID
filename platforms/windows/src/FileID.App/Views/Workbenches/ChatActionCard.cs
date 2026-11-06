using System.ComponentModel;
using System.Globalization;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Workbenches;

/// <summary>One proposed Chat action (CHAT.md port note, macOS ChatActionCard): what will happen,
/// the primary button, Cancel, live progress, then the result with Undo where one exists.
/// The card is the confirmation step: kinds that change files show their counts, examples and
/// undo story here, and nothing runs until the primary button is pressed.</summary>
internal sealed class ChatActionCard : UserControl
{
    private readonly ChatAction _action;
    private readonly ChatActionRunner _runner = ChatActionRunner.Instance;
    private readonly EngineClient _engine = EngineClient.Instance;
    private readonly Brush _text1 = ThemeHelper.GetBrushSafe("Text1Brush");
    private readonly Brush _text2 = ThemeHelper.GetBrushSafe("Text2Brush");
    private readonly Brush _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
    private readonly Brush _ai = ThemeHelper.GetBrushSafe("AiBrush");
    private readonly Brush _danger = ThemeHelper.GetBrushSafe("DangerBrush");
    private readonly Brush _delight = ThemeHelper.GetBrushSafe("DelightBrush");
    private readonly Brush _onAccent = ThemeHelper.GetBrushSafe("OnGoldBrush");
    private readonly Brush _accent;
    private readonly TextBlock _detail;
    private readonly Border _well;
    private readonly StackPanel _facts = new() { Orientation = Orientation.Horizontal, Spacing = 14 };
    private readonly StackPanel _examples = new() { Spacing = 3 };
    private readonly StackPanel _status = new() { Orientation = Orientation.Horizontal, Spacing = 10 };
    private readonly ProgressBar _progress = new() { IsIndeterminate = true, Width = 60, VerticalAlignment = VerticalAlignment.Center };
    private readonly FontIcon _statusIcon = new() { FontSize = 13, VerticalAlignment = VerticalAlignment.Top, Margin = new Thickness(0, 2, 0, 0) };
    private readonly TextBlock _statusText = new() { FontSize = 12.5, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true, MaxWidth = 620 };
    private readonly Button _primary;
    private readonly Button _cancel;
    private readonly Button _stop;
    private readonly Button _undo;
    private ChatActionStage? _shownStage;
    private object? _wellSource;

    public ChatActionCard(ChatAction action)
    {
        _action = action;
        var accent = ChatActionPolicy.Accent(action);
        var accentColor = accent switch
        {
            ChatActionAccent.Danger => Color.FromArgb(0xFF, 0xFF, 0x6A, 0x5E),
            ChatActionAccent.Gold => Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00),
            _ => Color.FromArgb(0xFF, 0xA0, 0xE2, 0xEA),
        };
        _accent = new SolidColorBrush(accentColor);
        var stroke = new SolidColorBrush(Color.FromArgb(0x59, accentColor.R, accentColor.G, accentColor.B));

        var icon = new FontIcon { Glyph = Glyph(action.Kind), FontSize = 12, Foreground = _accent, VerticalAlignment = VerticalAlignment.Center };
        AutomationProperties.SetAccessibilityView(icon, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
        var label = new TextBlock { Text = ChatActionPolicy.Label(action).ToUpperInvariant() };
        if (ThemeHelper.GetStyleSafe("ShellSectionLabelStyle") is { } labelStyle) label.Style = labelStyle;
        label.Foreground = _accent;
        label.VerticalAlignment = VerticalAlignment.Center;
        var header = new Grid { ColumnSpacing = 8 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(label, 1);
        header.Children.Add(icon);
        header.Children.Add(label);
        if (action.Source == "model")
        {
            var model = Line("Suggested by local AI", 11.5, _ai);
            Grid.SetColumn(model, 2);
            header.Children.Add(model);
        }

        _detail = new TextBlock { FontSize = 13, Foreground = _text1, TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
        UseFont(_detail, "BodyFontFamily");

        var wellStack = new StackPanel { Spacing = 6 };
        wellStack.Children.Add(_facts);
        wellStack.Children.Add(_examples);
        if (!string.IsNullOrWhiteSpace(action.Undo))
        {
            var undoStory = Line("Undo: " + action.Undo, 12, _text2);
            undoStory.TextWrapping = TextWrapping.Wrap;
            undoStory.TextTrimming = TextTrimming.None;
            wellStack.Children.Add(undoStory);
        }
        _well = new Border
        {
            Padding = new Thickness(10),
            CornerRadius = new CornerRadius(6),
            Background = new SolidColorBrush(Color.FromArgb(0x40, 0x00, 0x00, 0x00)),
            Child = wellStack,
            Visibility = Visibility.Collapsed,
        };

        UseFont(_statusText, "BodyFontFamily");
        _status.Children.Add(_progress);
        _status.Children.Add(_statusIcon);
        _status.Children.Add(_statusText);
        _status.Visibility = Visibility.Collapsed;

        _primary = ChatActionPolicy.IsDestructive(action) ? Danger(new Button()) : Styled(new Button(), "ShellPrimaryButtonStyle");
        _primary.Click += (_, _) => DebugLog.SafeRun("ChatActionCard.Primary", () => _ = DebugLog.SafeRunAsync("ChatActionCard.Run", () => _runner.RunAsync(_action)));
        _cancel = Styled(new Button { Content = "Cancel" }, "ShellSecondaryButtonStyle");
        _cancel.Click += (_, _) => DebugLog.SafeRun("ChatActionCard.Cancel", () => _runner.Cancel(_action));
        _stop = Styled(new Button { Content = "Stop" }, "ShellSecondaryButtonStyle");
        _stop.Click += (_, _) => DebugLog.SafeRun("ChatActionCard.Stop", () => _ = DebugLog.SafeRunAsync("ChatActionCard.StopAsync", () => _runner.StopAsync(_action)));
        _undo = Styled(new Button(), "ShellSecondaryButtonStyle");
        _undo.Click += (_, _) => DebugLog.SafeRun("ChatActionCard.Undo", () => _ = DebugLog.SafeRunAsync("ChatActionCard.UndoAsync", () => _runner.UndoAsync(_action)));
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        buttons.Children.Add(_primary);
        buttons.Children.Add(_cancel);
        buttons.Children.Add(_stop);
        buttons.Children.Add(_undo);

        var stack = new StackPanel { Spacing = 10 };
        stack.Children.Add(header);
        stack.Children.Add(_detail);
        stack.Children.Add(_well);
        stack.Children.Add(_status);
        stack.Children.Add(buttons);
        Content = new Border
        {
            Padding = new Thickness(12),
            CornerRadius = new CornerRadius(8),
            BorderThickness = new Thickness(1),
            BorderBrush = stroke,
            Background = new SolidColorBrush(Color.FromArgb(0x09, 0xFF, 0xFF, 0xFF)),
            Child = stack,
        };

        Loaded += (_, _) => DebugLog.SafeRun("ChatActionCard.Loaded", () =>
        {
            _runner.Changed += OnRunnerChanged;
            _engine.PropertyChanged += OnEngineChanged;
            Sync();
        });
        Unloaded += (_, _) => DebugLog.SafeRun("ChatActionCard.Unloaded", () =>
        {
            _runner.Changed -= OnRunnerChanged;
            _engine.PropertyChanged -= OnEngineChanged;
        });
        Sync();
    }

    public string ActionID => _action.Id;

    private void OnRunnerChanged(object? sender, string id)
        => DebugLog.SafeRun("ChatActionCard.OnRunnerChanged", () =>
        {
            if (id != _action.Id) return;
            DispatcherQueue.TryEnqueue(() => DebugLog.SafeRun("ChatActionCard.Sync", Sync));
        });

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("ChatActionCard.OnEngineChanged", () =>
        {
            if (e.PropertyName is not (nameof(EngineClient.LastRestructurePlan) or nameof(EngineClient.CanUndoRestructure))) return;
            if (_action.Kind is not ("applyFolderLayout" or "undoLastMove")) return;
            DebugLog.Info($"[ENGINE-SUB:ChatActionCard] {e.PropertyName}");
            DispatcherQueue.TryEnqueue(() => DebugLog.SafeRun("ChatActionCard.SyncEngine", Sync));
        });

    private void Sync()
    {
        var phase = _runner.Phase(_action.Id);
        var plan = _action.Kind == "applyFolderLayout" ? ChatActionPolicy.Plan(_engine.LastRestructurePlan, _action) : null;
        var canUndoMove = _engine.CanUndoRestructure;
        var proposed = phase.Stage == ChatActionStage.Proposed;
        var title = ChatActionPolicy.PrimaryTitle(_action, plan);

        _detail.Text = ChatActionPolicy.Detail(_action, plan, canUndoMove);
        _well.Visibility = _action.RequiresConfirmation && proposed ? Visibility.Visible : Visibility.Collapsed;
        var wellSource = _action.Kind == "applyFolderLayout" ? _engine.LastRestructurePlan : (object)_action;
        if (_well.Visibility == Visibility.Visible && !ReferenceEquals(wellSource, _wellSource))
        {
            _wellSource = wellSource;
            SyncWell(plan);
        }

        _primary.Content = title;
        _primary.Visibility = proposed ? Visibility.Visible : Visibility.Collapsed;
        _primary.IsEnabled = proposed && !ChatActionPolicy.IsUnavailable(_action, plan, canUndoMove);
        ToolTipService.SetToolTip(_primary, _primary.IsEnabled ? null : _action.Kind == "changeSetting" ? "This setting isn't on Windows yet." : _detail.Text);
        AutomationProperties.SetName(_primary, title);
        _cancel.Visibility = proposed && _action.RequiresConfirmation ? Visibility.Visible : Visibility.Collapsed;
        _stop.Visibility = _runner.CanStop(_action) ? Visibility.Visible : Visibility.Collapsed;
        var undoLabel = _runner.UndoLabel(_action.Id);
        _undo.Content = undoLabel ?? "Undo";
        _undo.Visibility = undoLabel is null ? Visibility.Collapsed : Visibility.Visible;

        SyncStatus(phase);
        AutomationProperties.SetName(this, $"{(_action.RequiresConfirmation ? "Needs your OK" : "Suggested action")}: {title}");
    }

    private void SyncWell(ChatPlanSummary? plan)
    {
        _facts.Children.Clear();
        if (ChatActionPolicy.FileCount(_action, plan) is { } count) _facts.Children.Add(Fact("Files", count.ToString("N0", CultureInfo.CurrentCulture)));
        if (_action.ByteCount is { } bytes) _facts.Children.Add(Fact("Size", ByteFormat.File(bytes)));
        _facts.Visibility = _facts.Children.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        _examples.Children.Clear();
        var examples = ChatActionPolicy.Examples(_action, plan);
        var full = plan is { TooLarge: false } ? plan.Examples : _action.Examples ?? Array.Empty<string>();
        for (var i = 0; i < examples.Count; i++)
        {
            var line = Line(ChatActionPolicy.MiddleTruncate(examples[i]), 11.5, _text2, "MonoFontFamily");
            ToolTipService.SetToolTip(line, i < full.Count ? full[i] : examples[i]);
            _examples.Children.Add(line);
        }
        var more = ChatActionPolicy.MoreCount(_action, plan);
        if (more > 0) _examples.Children.Add(Line($"and {more.ToString("N0", CultureInfo.CurrentCulture)} more", 11.5, _text3));
        _examples.Visibility = _examples.Children.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private void SyncStatus(ChatActionPhase phase)
    {
        var changed = _shownStage != phase.Stage;
        _shownStage = phase.Stage;
        switch (phase.Stage)
        {
            case ChatActionStage.Running:
                _progress.Visibility = Visibility.Visible;
                _statusIcon.Visibility = Visibility.Collapsed;
                _statusText.Text = string.IsNullOrWhiteSpace(phase.Text) ? "Working…" : phase.Text;
                _statusText.Foreground = _ai;
                break;
            case ChatActionStage.Done:
                _progress.Visibility = Visibility.Collapsed;
                _statusIcon.Visibility = Visibility.Visible;
                _statusIcon.Glyph = "\uE930";
                _statusIcon.Foreground = _delight;
                _statusText.Text = phase.Text;
                _statusText.Foreground = _text2;
                break;
            case ChatActionStage.Failed:
                _progress.Visibility = Visibility.Collapsed;
                _statusIcon.Visibility = Visibility.Visible;
                _statusIcon.Glyph = "\uE7BA";
                _statusIcon.Foreground = _danger;
                _statusText.Text = phase.Text;
                _statusText.Foreground = _danger;
                break;
            case ChatActionStage.Cancelled:
                _progress.Visibility = Visibility.Collapsed;
                _statusIcon.Visibility = Visibility.Collapsed;
                _statusText.Text = phase.Text;
                _statusText.Foreground = _text3;
                break;
            default:
                _status.Visibility = Visibility.Collapsed;
                return;
        }
        _status.Visibility = Visibility.Visible;
        AutomationProperties.SetName(_status, _statusText.Text);
        if (!changed) return;
        if (ReducedMotion.Instance.IsReduced)
        {
            SpringEasing.SetScalar(_status, "Opacity", 1f);
            return;
        }
        SpringEasing.SetScalar(_status, "Opacity", 0f);
        SpringEasing.AnimateOpacity(_status, 1f, SpringEasing.Tokens.Standard.Response, SpringEasing.Tokens.Standard.DampingFraction);
    }

    private StackPanel Fact(string label, string value)
    {
        var key = new TextBlock { Text = label.ToUpperInvariant() };
        if (ThemeHelper.GetStyleSafe("ShellSectionLabelStyle") is { } style) key.Style = style;
        var text = Line(value, 12.5, _text1, "MonoFontFamily");
        text.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        var stack = new StackPanel { Spacing = 2 };
        stack.Children.Add(key);
        stack.Children.Add(text);
        AutomationProperties.SetName(stack, $"{label}: {value}");
        return stack;
    }

    private Button Danger(Button button)
    {
        Styled(button, "ShellButtonStyle");
        button.Background = _danger;
        button.Foreground = _onAccent;
        button.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        return button;
    }

    private static Button Styled(Button button, string style)
    {
        if (ThemeHelper.GetStyleSafe(style) is { } resolved) button.Style = resolved;
        return button;
    }

    private static TextBlock Line(string text, double size, Brush foreground, string font = "BodyFontFamily")
    {
        var line = new TextBlock { Text = text, FontSize = size, Foreground = foreground, TextTrimming = TextTrimming.CharacterEllipsis, VerticalAlignment = VerticalAlignment.Center };
        UseFont(line, font);
        return line;
    }

    private static void UseFont(TextBlock text, string key)
    {
        try { if (Application.Current.Resources[key] is FontFamily family) text.FontFamily = family; }
        catch (Exception) { }
    }

    private static string Glyph(string kind) => kind switch
    {
        "scanFolder" or "rescan" => "\uE8F4",
        "describeFiles" => "\uE8D2",
        "stopJob" => "\uE71A",
        "findDuplicates" or "moveDuplicatesToTrash" => "\uE74D",
        "suggestFolderLayout" or "applyFolderLayout" or "undoLastMove" => "\uE8B7",
        "renameToSuggested" => "\uE8AC",
        "namePerson" or "mergePeople" => "\uE77B",
        "findMoments" or "search" => "\uE721",
        "exportFiles" => "\uE90F",
        "changeSetting" => "\uE713",
        _ => "\uE8A7",
    };
}
