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
            Style = FileID.Services.ThemeHelper.GetStyleSafe("BodyStrongTextBlockStyle")!,
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
        var submittedPlans = _items
            .Where(p => p.Include
                        && !string.IsNullOrWhiteSpace(p.ProposedName)
                        && !p.ProposedName.Contains('/')
                        && !p.ProposedName.Contains('\\'))
            .ToArray();
        var entries = submittedPlans
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
            var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                "renameFiles",
                () => EngineClient.Instance.RenameFilesAsync(entries),
                TimeSpan.FromSeconds(30));
            var inverse = BuildConfirmedInverse(submittedPlans, result);
            if (inverse.Length > 0)
            {
                Services.UndoStack.Instance.Push(
                    $"rename {inverse.Length} file{(inverse.Length == 1 ? "" : "s")}",
                    Services.ChangeKind.Rename,
                    async () =>
                    {
                        var confirmed = await ReverseConfirmedAsync(
                            inverse,
                            renames => EngineClient.Instance.WaitForBulkActionResultAsync(
                                "renameFiles",
                                () => EngineClient.Instance.RenameFilesAsync(renames),
                                TimeSpan.FromSeconds(30))).ConfigureAwait(false);
                        if (!confirmed)
                        {
                            throw new InvalidOperationException(
                                "The engine did not confirm every reverse rename.");
                        }
                        return true;
                    });
            }

            if (!Services.BulkActionResultTruth.ConfirmsExactSuccess(
                    result,
                    entries.Select(entry => entry.FileId).ToArray()))
            {
                var first = result.Messages.FirstOrDefault(m => !m.Ok)?.Message
                            ?? "the engine did not confirm every requested rename";
                var notConfirmed = entries.Length - inverse.Length;
                var body = inverse.Length > 0
                    ? $"Renamed {inverse.Length}; {notConfirmed} not confirmed — {first}"
                    : $"No renames were confirmed — {first}";
                StatusText.Text = body;
                await ShowAlertAsync("Rename incomplete", body);
                return false;
            }

            StatusText.Text = $"Renamed {result.Succeeded} file(s).";
            return true;
        }
        catch (Exception ex)
        {
            var msg = Services.SqliteErrorTranslator.Humanize(ex);
            StatusText.Text = $"Failed: {msg}";
            await ShowAlertAsync("Rename failed", msg);
            return false;
        }
    }

    internal static RenameEntry[] BuildConfirmedInverse(
        IReadOnlyList<RenamePlan> submittedPlans,
        BulkActionResult result)
    {
        var byId = submittedPlans
            .GroupBy(plan => plan.FileId)
            .ToDictionary(group => group.Key, group => group.First());
        return Services.BulkActionResultTruth
            .ConfirmedSuccessfulFileIds(result, submittedPlans.Select(plan => plan.FileId))
            .Select(fileId =>
            {
                var plan = byId[fileId];
                return new RenameEntry(fileId, Path.GetFileName(plan.CurrentPath));
            })
            .ToArray();
    }

    internal static async Task<bool> ReverseConfirmedAsync(
        IReadOnlyList<RenameEntry> inverse,
        Func<IReadOnlyList<RenameEntry>, Task<BulkActionResult>> reverse)
    {
        if (inverse.Count == 0) return false;
        var result = await reverse(inverse).ConfigureAwait(false);
        return Services.BulkActionResultTruth.ConfirmsExactSuccess(
            result,
            inverse.Select(entry => entry.FileId).ToArray());
    }

    private async Task ShowAlertAsync(string title, string body)
    {
        // ContentDialog.ShowAsync can throw on a broken XamlRoot (mid-shutdown,
        // tab re-host). Catch + log so a failed alert never escalates.
        try
        {
            if (XamlRoot is null) return;
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = title,
                Content = body,
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        }
        catch
        {
            // Best-effort surfacing; the in-sheet StatusText still carries the message.
        }
    }
}
