// SessionChangesSheet code-behind. Renders ChangeLog snapshots; each row
// owns a per-entry Undo/Retry button. Rebuild-on-Changed is safe here:
// the sheet is only visible inside a modal ContentDialog, Changed fires at
// user-action cadence (not an event burst), and rows re-materialize from a
// consistent snapshot on the dispatcher (CLAUDE.md: snapshot +
// DispatcherQueue.TryEnqueue; brushes cached at ctor, never per-row).

using System;
using System.ComponentModel;
using FileID.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views;

public sealed partial class SessionChangesSheet : UserControl
{
    private static readonly SolidColorBrush GoldBrush = new(Microsoft.UI.Colors.Gold);
    private readonly SolidColorBrush _dimBrush;
    private readonly SolidColorBrush _warnBrush;
    private bool _unloaded;

    public SessionChangesSheet()
    {
        InitializeComponent();
        _dimBrush = new SolidColorBrush(Microsoft.UI.Colors.Gray);
        _warnBrush = new SolidColorBrush(Microsoft.UI.Colors.Orange);
        ChangeLog.Instance.Changed += OnChangeLogChanged;
        Unloaded += (_, _) =>
        {
            _unloaded = true;
            ChangeLog.Instance.Changed -= OnChangeLogChanged;
        };
        Loaded += (_, _) => Rebuild();
    }

    private void OnChangeLogChanged(object? sender, EventArgs e)
        => DispatcherQueue.TryEnqueue(() =>
        {
            if (_unloaded) return;
            Rebuild();
        });

    private void Rebuild()
        => DebugLog.SafeRun(nameof(Rebuild), () =>
        {
            var snapshot = ChangeLog.Instance.Snapshot();
            EmptyText.Visibility = snapshot.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
            var rows = new System.Collections.Generic.List<UIElement>(snapshot.Count);
            foreach (var entry in snapshot) rows.Add(BuildRow(entry));
            RowsHost.ItemsSource = rows;
        });

    private Grid BuildRow(ChangeLogEntry entry)
    {
        var grid = new Grid { ColumnSpacing = 10, Padding = new Thickness(4, 8, 4, 8) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var glyph = new FontIcon
        {
            Glyph = entry.Kind switch
            {
                ChangeKind.Rename => "",      // Rename
                ChangeKind.Trash => "",       // Delete
                ChangeKind.Restructure => "", // Folder
                ChangeKind.PeopleMerge => "", // People
                ChangeKind.Tags => "",        // Tag
                _ => "",                      // History
            },
            FontSize = 14,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(glyph, 0);

        var textStack = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        var label = new TextBlock
        {
            Text = entry.Label,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        var detailText = entry.Status switch
        {
            ChangeStatus.Undone => "Undone",
            ChangeStatus.UndoFailed => "Undo failed" + (entry.StatusDetail is null ? "" : " — " + entry.StatusDetail),
            ChangeStatus.NotUndoable => entry.StatusDetail ?? "This change can no longer be undone.",
            _ => FormatTimestamp(entry.Timestamp),
        };
        var detail = new TextBlock
        {
            Text = detailText,
            FontSize = 11,
            TextTrimming = TextTrimming.CharacterEllipsis,
            Foreground = entry.Status == ChangeStatus.UndoFailed ? _warnBrush : _dimBrush,
        };
        if (entry.StatusDetail is not null || entry.Status != ChangeStatus.Undoable)
        {
            ToolTipService.SetToolTip(detail, detailText);
        }
        textStack.Children.Add(label);
        textStack.Children.Add(detail);
        Grid.SetColumn(textStack, 1);

        if (entry.Status is ChangeStatus.Undone or ChangeStatus.NotUndoable)
        {
            glyph.Foreground = _dimBrush;
            label.Foreground = _dimBrush;
        }

        grid.Children.Add(glyph);
        grid.Children.Add(textStack);

        if (entry.Status is ChangeStatus.Undoable or ChangeStatus.UndoFailed)
        {
            var isRetry = entry.Status == ChangeStatus.UndoFailed;
            var button = new Button
            {
                Content = isRetry ? "Retry" : "Undo",
                VerticalAlignment = VerticalAlignment.Center,
                BorderBrush = GoldBrush,
            };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(
                button, (isRetry ? "Retry undo of " : "Undo ") + entry.Label);
            button.Click += async (_, _) =>
                await DebugLog.SafeRunAsync("SessionChangesSheet.UndoRow", async () =>
                {
                    button.IsEnabled = false;
                    var ok = isRetry
                        ? await ChangeLog.Instance.RetryAsync(entry)
                        : await ChangeLog.Instance.UndoAsync(entry);
                    // Rebuild fires via ChangeLog.Changed; on failure the row
                    // re-renders as UndoFailed with the reason — no silent drop.
                    if (!ok) DebugLog.Info($"[CHANGES] undo declined/failed for '{entry.Label}'");
                });
            Grid.SetColumn(button, 2);
            grid.Children.Add(button);
        }
        else if (entry.Status == ChangeStatus.Undone)
        {
            var pill = new Border
            {
                CornerRadius = new CornerRadius(8),
                Padding = new Thickness(8, 2, 8, 2),
                VerticalAlignment = VerticalAlignment.Center,
                Child = new TextBlock { Text = "Undone", FontSize = 11, Foreground = _dimBrush },
            };
            Grid.SetColumn(pill, 2);
            grid.Children.Add(pill);
        }

        return grid;
    }

    private static string FormatTimestamp(DateTimeOffset ts)
    {
        var delta = DateTimeOffset.Now - ts;
        if (delta.TotalSeconds < 60) return "just now";
        if (delta.TotalMinutes < 60) return $"{(int)delta.TotalMinutes} min ago";
        return ts.ToString("t");
    }
}
