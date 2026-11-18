// BulkTagSheet code-behind. Hosted inside a ContentDialog so Esc dismisses

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
    // A nested ContentDialog can't open while this sheet's host dialog is mid-deferral (WinUI allows only one open at a time), so the confirmation is an explicit second Apply click gated by this flag instead.
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
            _replaceConfirmed = false;
        }

        StatusText.Text = "Applying...";
        try
        {
            var priorUserTags = await Services.TagChangeJournal.CapturePriorUserTagsAsync(_fileIds)
                .ConfigureAwait(true);
            var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                "applyTags",
                () => EngineClient.Instance.ApplyTagsAsync(_fileIds, tags, mode),
                TimeSpan.FromSeconds(30));

            var confirmedFileIds = Services.BulkActionResultTruth.ConfirmedSuccessfulFileIds(
                result, _fileIds);
            if (result.Action == "applyTags")
            {
                Services.TagChangeJournal.PushUndo(
                    Services.TagChangeJournal.FormatLabel(mode, confirmedFileIds.Count),
                    confirmedFileIds,
                    priorUserTags);
            }

            if (result.Failed > 0 || result.Action != "applyTags"
                || !Services.BulkActionResultTruth.ConfirmsExactSuccess(result, _fileIds))
            {
                // Keep the dialog open so the user can adjust + retry; do NOT report success.
                var first = result.Messages.FirstOrDefault(m => !m.Ok)?.Message
                            ?? "see logs for details";
                var body = result.Failed == 0
                    ? "The engine response did not confirm every tag change. Check the files before retrying."
                    : result.Succeeded > 0
                    ? $"Tagged {result.Succeeded}; {result.Failed} failed — {first}"
                    : $"{result.Failed} file(s) failed to tag — {first}";
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
        IReadOnlyList<long> fileIds,
        IReadOnlyDictionary<long, List<string>> priorTags)
        => Services.TagChangeJournal.GroupByTagSet(fileIds, priorTags);

    internal static Task<bool> RestoreGroupsConfirmedAsync(
        IReadOnlyList<(List<long> Ids, List<string> Tags)> groups,
        Func<IReadOnlyList<long>, IReadOnlyList<string>, Task<BulkActionResult>> restore)
        => Services.TagChangeJournal.RestoreGroupsConfirmedAsync(groups, restore);

    private async Task ShowAlertAsync(string title, string body)
    {
        // Catch + log so a failed alert never escalates.
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
        }
    }

    private void OnTagsKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key == VirtualKey.Enter
            && Microsoft.UI.Input.InputKeyboardSource
                .GetKeyStateForCurrentThread(VirtualKey.Control)
                .HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down))
        {
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
