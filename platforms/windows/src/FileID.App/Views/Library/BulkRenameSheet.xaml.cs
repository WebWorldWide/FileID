// BulkRenameSheet code-behind. Uses a virtualized XAML ItemTemplate for rows.
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

public sealed class RenamePlan
{
    public long FileId { get; init; }
    public string CurrentPath { get; init; } = string.Empty;
    public string ProposedName { get; set; } = string.Empty;
    public bool? Include { get; set; } = true;
    public string CurrentName => Path.GetFileName(CurrentPath);
}

public sealed partial class BulkRenameSheet : UserControl
{
    internal readonly ObservableCollection<RenamePlan> _items = new();

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

        RenameRepeater.ItemsSource = _items;
    }

    public async Task<bool> CommitAsync()
    {
        var selectedPlans = _items
            .Where(p => p.Include == true
                        && !string.IsNullOrWhiteSpace(p.ProposedName)
                        && !p.ProposedName.Contains('/')
                        && !p.ProposedName.Contains('\\'))
            .Select(p => new RenamePlan
            {
                FileId = p.FileId,
                CurrentPath = p.CurrentPath,
                ProposedName = p.ProposedName.Trim(),
            })
            .ToArray();
        var entries = selectedPlans
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

            var inverse = BuildConfirmedInverse(selectedPlans, result);
            if (inverse.Count > 0)
            {
                Services.UndoStack.Instance.Push(
                    $"rename {inverse.Count} file{(inverse.Count == 1 ? "" : "s")}",
                    async () =>
                    {
                        try
                        {
                            return await ReverseConfirmedAsync(
                                inverse,
                                renames => EngineClient.Instance.WaitForBulkActionResultAsync(
                                    "renameFiles",
                                    () => EngineClient.Instance.RenameFilesAsync(renames),
                                    TimeSpan.FromSeconds(30)));
                        }
                        catch (Exception ex)
                        {
                            Services.DebugLog.Warn("Bulk rename undo failed: " + ex.Message);
                            return false;
                        }
                    });
            }

            if (result.Failed > 0 || !Services.BulkActionResultTruth.ConfirmsExactSuccess(
                    result, entries.Select(entry => entry.FileId).ToArray()))
            {
                // Surface per-file engine failures (in use, permission, name
                // collision). Keep the sheet open so the user can fix + retry;
                // do NOT report success.
                var first = result.Messages.FirstOrDefault(m => !m.Ok)?.Message
                            ?? "see logs for details";
                var body = result.Failed == 0
                    ? "The engine response did not confirm every rename. Check the files before retrying."
                    : result.Succeeded > 0
                    ? $"Renamed {result.Succeeded}; {result.Failed} failed — {first}"
                    : $"{result.Failed} rename(s) failed — {first}";
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

    internal static IReadOnlyList<RenameEntry> BuildConfirmedInverse(
        IReadOnlyList<RenamePlan> plans,
        BulkActionResult result)
    {
        if (result.Action != "renameFiles") return Array.Empty<RenameEntry>();
        var requested = plans
            .Where(plan => plan.Include == true
                && !string.IsNullOrWhiteSpace(plan.ProposedName)
                && !plan.ProposedName.Contains('/')
                && !plan.ProposedName.Contains('\\'))
            .ToArray();
        var confirmed = Services.BulkActionResultTruth.ConfirmedSuccessfulFileIds(
            result, requested.Select(plan => plan.FileId)).ToHashSet();
        return requested
            .Where(plan => confirmed.Contains(plan.FileId))
            .Select(plan => new RenameEntry(plan.FileId, Path.GetFileName(plan.CurrentPath)))
            .ToArray();
    }

    internal static async Task<bool> ReverseConfirmedAsync(
        IReadOnlyList<RenameEntry> inverse,
        Func<IReadOnlyList<RenameEntry>, Task<BulkActionResult>> reverse)
    {
        if (inverse.Count == 0) return false;
        var result = await reverse(inverse).ConfigureAwait(false);
        return result.Action == "renameFiles"
            && Services.BulkActionResultTruth.ConfirmsExactSuccess(
                result, inverse.Select(entry => entry.FileId).ToArray());
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
