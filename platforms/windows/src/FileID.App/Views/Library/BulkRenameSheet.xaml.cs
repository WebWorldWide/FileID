// BulkRenameSheet code-behind. Each row is a small custom panel built in
// code (avoids a DataTemplate dance for what is two controls per row).
// Apply emits engine `renameFiles` IPC with the entries for which the
// "include" checkbox is on.

using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views.Library;

public sealed partial class BulkRenameSheet : UserControl
{
    public sealed class RenamePlan
    {
        public long FileId { get; init; }
        public string CurrentPath { get; init; } = string.Empty;
        public string ProposedName { get; set; } = string.Empty;
        public bool Include { get; set; } = true;
    }

    private readonly ObservableCollection<RenamePlan> _items = new();

    public BulkRenameSheet()
    {
        InitializeComponent();
    }

    public void SetPlan(IReadOnlyList<RenamePlan> plan)
    {
        _items.Clear();
        foreach (var p in plan) _items.Add(p);
        SelectionText.Text = plan.Count == 1
            ? "1 rename pending. Toggle off any row you don't want."
            : $"{plan.Count} renames pending. Toggle off any row you don't want.";

        // Build out the row UI. Pure code-behind so we don't have to
        // fight a DataTemplate compile-time issue.
        var stack = new StackPanel { Spacing = 6 };
        foreach (var item in _items)
        {
            stack.Children.Add(BuildRow(item));
        }
        RenameRepeater.ItemsSource = _items
            .Select(p => BuildRow(p))
            .ToList();
    }

    private FrameworkElement BuildRow(RenamePlan plan)
    {
        var currentName = Path.GetFileName(plan.CurrentPath);
        var grid = new Grid
        {
            ColumnSpacing = 10,
            Padding = new Thickness(8, 6, 8, 6),
        };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var include = new CheckBox
        {
            IsChecked = plan.Include,
            VerticalAlignment = VerticalAlignment.Center,
            MinWidth = 0,
        };
        include.Checked += (_, _) => plan.Include = true;
        include.Unchecked += (_, _) => plan.Include = false;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(include, $"Include {currentName} in rename");

        var current = new TextBlock
        {
            Text = currentName,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        };

        var proposed = new TextBox
        {
            Text = plan.ProposedName,
            VerticalAlignment = VerticalAlignment.Center,
            PlaceholderText = "new filename",
        };
        proposed.TextChanged += (_, _) => plan.ProposedName = proposed.Text ?? string.Empty;
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(proposed, $"New name for {currentName}");

        Grid.SetColumn(include, 0);
        Grid.SetColumn(current, 1);
        Grid.SetColumn(proposed, 2);
        grid.Children.Add(include);
        grid.Children.Add(current);
        grid.Children.Add(proposed);
        return grid;
    }

    public async Task<bool> CommitAsync()
    {
        var entries = _items
            .Where(p => p.Include
                        && !string.IsNullOrWhiteSpace(p.ProposedName)
                        && !p.ProposedName.Contains('/')
                        && !p.ProposedName.Contains('\\'))
            .Select(p => new RenameEntry(p.FileId, p.ProposedName.Trim()))
            .ToArray();

        if (entries.Length == 0)
        {
            StatusText.Text = "Nothing to rename — every row is excluded or has an empty name.";
            return false;
        }

        StatusText.Text = "Renaming...";
        try
        {
            // Snapshot the inverse rename (file_id → previous filename) so
            // Ctrl+Z can undo. We push BEFORE the rename fires so the user
            // sees the entry available even on partial failure (the engine
            // emits per-file ok/fail in the BulkActionResult).
            var inverse = _items
                .Where(p => p.Include
                            && !string.IsNullOrWhiteSpace(p.ProposedName)
                            && !p.ProposedName.Contains('/')
                            && !p.ProposedName.Contains('\\'))
                .Select(p => new RenameEntry(p.FileId, System.IO.Path.GetFileName(p.CurrentPath)))
                .ToArray();
            Services.UndoStack.Instance.Push(
                $"rename {entries.Length} file{(entries.Length == 1 ? "" : "s")}",
                async () =>
                {
                    try
                    {
                        await EngineClient.Instance.RenameFilesAsync(inverse);
                        return true;
                    }
                    catch { return false; }
                });

            await EngineClient.Instance.RenameFilesAsync(entries);
            StatusText.Text = $"Sent {entries.Length} rename(s).";
            return true;
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Failed: {ex.Message}";
            return false;
        }
    }
}
