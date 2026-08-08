// BulkTagSheet code-behind. Hosted inside a ContentDialog so Esc dismisses
// for free. The Apply path emits engine `applyTags` IPC and surfaces the
// engine's BulkActionResult via a status line.

using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;

namespace FileID.Views.Library;

public sealed partial class BulkTagSheet : UserControl
{
    private IReadOnlyList<long> _fileIds = Array.Empty<long>();
    // "Replace existing" wipes every user tag before writing the new set. A
    // nested ContentDialog can't open while this sheet's host dialog is
    // mid-deferral (WinUI allows only one open at a time), so the confirmation
    // is an explicit second Apply click gated by this flag instead. (F-C5-004)
    private bool _replaceConfirmed;

    public BulkTagSheet()
    {
        InitializeComponent();
    }

    public void SetSelection(IReadOnlyList<long> fileIds)
    {
        _fileIds = fileIds;
        SelectionText.Text = fileIds.Count == 1
            ? "Will tag 1 file."
            : $"Will tag {fileIds.Count} files.";
    }

    public string Mode =>
        RemoveRadio.IsChecked == true ? "remove" :
        ReplaceRadio.IsChecked == true ? "replace" :
        "add";

    public IReadOnlyList<string> Tags
    {
        get
        {
            var raw = TagsInput.Text ?? string.Empty;
            return raw.Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
                      .Where(t => t.Length > 0)
                      .Distinct(StringComparer.OrdinalIgnoreCase)
                      .ToArray();
        }
    }

    /// <summary>
    /// Validates + commits via engine IPC. Returns false if validation
    /// fails (caller can keep the dialog open).
    /// </summary>
    public async Task<bool> CommitAsync()
    {
        var tags = Tags;
        if (tags.Count == 0)
        {
            StatusText.Text = "Add at least one tag.";
            return false;
        }
        if (_fileIds.Count == 0)
        {
            StatusText.Text = "No files selected.";
            return false;
        }

        var mode = Mode;
        // Confirm the destructive "Replace existing" before it wipes user tags.
        if (mode == "replace")
        {
            if (!_replaceConfirmed)
            {
                _replaceConfirmed = true;
                StatusText.Text = _fileIds.Count == 1
                    ? "Replace deletes this file's existing tags. Click Apply again to confirm."
                    : $"Replace deletes existing tags on {_fileIds.Count} files. Click Apply again to confirm.";
                return false;
            }
        }
        else
        {
            // User switched off Replace — re-arm the confirm for next time.
            _replaceConfirmed = false;
        }

        try
        {
            StatusText.Text = "Saving existing tags for undo...";
            var priorUserTags = await Services.TagChangeJournal
                .CapturePriorUserTagsAsync(_fileIds)
                .ConfigureAwait(true);

            StatusText.Text = "Applying...";
            var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                "applyTags",
                () => EngineClient.Instance.ApplyTagsAsync(_fileIds, tags, mode),
                Services.BulkActionTimeout.ForFileCount(_fileIds.Count));
            var confirmedFileIds = Services.BulkActionResultTruth
                .ConfirmedSuccessfulFileIds(result, _fileIds);

            if (confirmedFileIds.Count > 0)
            {
                Services.TagChangeJournal.PushUndo(
                    Services.TagChangeJournal.FormatLabel(mode, confirmedFileIds.Count),
                    confirmedFileIds,
                    priorUserTags);
            }

            if (!Services.BulkActionResultTruth.ConfirmsExactSuccess(result, _fileIds))
            {
                var first = result.Messages.FirstOrDefault(m => !m.Ok)?.Message
                            ?? "the engine did not confirm every requested tag update";
                var notConfirmed = _fileIds.Distinct().Count() - confirmedFileIds.Count;
                var body = confirmedFileIds.Count > 0
                    ? $"Tagged {confirmedFileIds.Count}; {notConfirmed} not confirmed — {first}"
                    : $"No tag updates were confirmed — {first}";
                StatusText.Text = body;
                await ShowAlertAsync("Tagging incomplete", body);
                return false;
            }

            StatusText.Text = $"Tagged {result.Succeeded} file(s).";
            return true;
        }
        catch (Exception ex)
        {
            var msg = Services.SqliteErrorTranslator.Humanize(ex);
            StatusText.Text = $"Failed: {msg}";
            await ShowAlertAsync("Tagging failed", msg);
            return false;
        }
    }

    internal static List<(List<long> Ids, List<string> Tags)> GroupByTagSet(
        IReadOnlyList<long> fileIds, IReadOnlyDictionary<long, List<string>> priorTags)
        => Services.TagChangeJournal.GroupByTagSet(fileIds, priorTags);

    internal static async Task<bool> RestoreGroupsConfirmedAsync(
        IReadOnlyList<(List<long> Ids, List<string> Tags)> groups,
        Func<IReadOnlyList<long>, IReadOnlyList<string>, Task<BulkActionResult>> reverse)
        => await Services.TagChangeJournal
            .RestoreGroupsConfirmedAsync(groups, reverse)
            .ConfigureAwait(false);

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

    private void OnTagsKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter
            && Microsoft.UI.Input.InputKeyboardSource
                .GetKeyStateForCurrentThread(VirtualKey.Control)
                .HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down))
        {
            // Ctrl+Enter — caller listens via ContentDialog primary button;
            // this just signals via FocusManager. The dialog wires its own
            // primary-button handler that calls CommitAsync.
            e.Handled = true;
            var root = XamlRoot;
            if (root?.Content is FrameworkElement fe)
            {
                var btn = fe.FindName("BulkTagPrimaryButton") as Button;
                btn?.Focus(FocusState.Programmatic);
            }
        }
    }
}
