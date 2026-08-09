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

        // Snapshot prior user tags BEFORE the replace deletes them, so the
        // action can be journaled for undo (the engine drops every source='user'
        // row first). Add/Remove are non-destructive, so they need no snapshot.
        IReadOnlyDictionary<long, List<string>>? priorUserTags =
            mode == "replace" ? await ReadUserTagsAsync(_fileIds).ConfigureAwait(true) : null;

        StatusText.Text = "Applying...";
        try
        {
            var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                "applyTags",
                () => EngineClient.Instance.ApplyTagsAsync(_fileIds, tags, mode),
                TimeSpan.FromSeconds(30));

            if (result.Failed > 0)
            {
                // Surface per-file engine failures. Keep the dialog open so the
                // user can adjust + retry; do NOT report success.
                var first = result.Messages.FirstOrDefault(m => !m.Ok)?.Message
                            ?? "see logs for details";
                var body = result.Succeeded > 0
                    ? $"Tagged {result.Succeeded}; {result.Failed} failed — {first}"
                    : $"{result.Failed} file(s) failed to tag — {first}";
                StatusText.Text = body;
                await ShowAlertAsync("Tagging incomplete", body);
                return false;
            }

            // Journal the replace so Ctrl+Z restores the wiped user tags.
            if (mode == "replace" && result.Succeeded > 0 && priorUserTags is not null)
            {
                JournalReplaceUndo(_fileIds, priorUserTags);
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

    // Read each file's current user tags (source='user') from the read-only DB,
    // mirroring FilePreviewSheet's direct-connection read. Best-effort: a DB it
    // can't open yields an empty map and the undo simply restores nothing.
    private static async Task<IReadOnlyDictionary<long, List<string>>> ReadUserTagsAsync(
        IReadOnlyList<long> fileIds)
    {
        var map = new Dictionary<long, List<string>>();
        if (fileIds.Count == 0) return map;
        await Task.Run(() =>
        {
            try
            {
                if (!System.IO.File.Exists(Services.AppPaths.DbPath)) return;
                using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                    new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                    {
                        DataSource = Services.AppPaths.DbPath,
                        Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                    }.ToString());
                conn.Open();
                using var cmd = conn.CreateCommand();
                // file ids are int64 drawn from our own DB selection (never user
                // text), so an inlined IN-list is injection-safe and sidesteps a
                // variable-count parameter dance.
                var inList = string.Join(",", fileIds);
                cmd.CommandText =
                    $"SELECT file_id, tag FROM tags WHERE source = 'user' AND file_id IN ({inList}) ORDER BY file_id, rowid";
                using var rdr = cmd.ExecuteReader();
                while (rdr.Read())
                {
                    var fid = rdr.GetInt64(0);
                    if (!map.TryGetValue(fid, out var list))
                    {
                        list = new List<string>();
                        map[fid] = list;
                    }
                    list.Add(rdr.GetString(1));
                }
            }
            catch
            {
                // DB unavailable / locked — undo degrades to a no-op restore.
            }
        }).ConfigureAwait(false);
        return map;
    }

    // Group the affected files by their identical prior user-tag set so the undo
    // restores each distinct set with a single applyTags(replace) per group (the
    // IPC command applies one tag list to many files). Pure + static so it's
    // unit-testable without the UI runtime.
    internal static List<(List<long> Ids, List<string> Tags)> GroupByTagSet(
        IReadOnlyList<long> fileIds, IReadOnlyDictionary<long, List<string>> priorTags)
    {
        var groups = new Dictionary<string, (List<long> Ids, List<string> Tags)>(StringComparer.Ordinal);
        foreach (var id in fileIds)
        {
            var tags = priorTags.TryGetValue(id, out var t) ? t : new List<string>();
            // Order-insensitive key so files with the same set share one batch.
            var key = string.Join("\u0001", tags.OrderBy(x => x, StringComparer.Ordinal));
            if (!groups.TryGetValue(key, out var g))
            {
                g = (new List<long>(), new List<string>(tags));
                groups[key] = g;
            }
            g.Ids.Add(id);
        }
        return groups.Values.ToList();
    }

    private static void JournalReplaceUndo(
        IReadOnlyList<long> fileIds, IReadOnlyDictionary<long, List<string>> priorUserTags)
    {
        var groups = GroupByTagSet(fileIds, priorUserTags);
        var label = fileIds.Count == 1 ? "replace tags" : $"replace tags on {fileIds.Count} files";
        Services.UndoStack.Instance.Push(label, async () =>
        {
            try
            {
                // Restore each file's prior user-tag set; "replace" with the
                // captured tags resets exactly (an empty set clears them again).
                foreach (var (ids, tags) in groups)
                {
                    await EngineClient.Instance.WaitForBulkActionResultAsync(
                        "applyTags",
                        () => EngineClient.Instance.ApplyTagsAsync(ids, tags, "replace"),
                        TimeSpan.FromSeconds(30)).ConfigureAwait(false);
                }
                return true;
            }
            catch (Exception ex)
            {
                Services.DebugLog.Warn("Bulk-tag replace undo failed: " + ex.Message);
                return false;
            }
        });
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
