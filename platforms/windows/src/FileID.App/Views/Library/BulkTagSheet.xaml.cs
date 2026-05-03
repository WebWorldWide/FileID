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

    public string Mode => ReplaceRadio.IsChecked == true ? "replace" : "add";

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

        StatusText.Text = "Applying...";
        try
        {
            await EngineClient.Instance.ApplyTagsAsync(_fileIds, tags, Mode);
            StatusText.Text = $"Sent {tags.Count} tag(s) for {_fileIds.Count} file(s).";
            return true;
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Failed: {ex.Message}";
            return false;
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
            var root = this.XamlRoot;
            if (root?.Content is FrameworkElement fe)
            {
                var btn = fe.FindName("BulkTagPrimaryButton") as Button;
                btn?.Focus(FocusState.Programmatic);
            }
        }
    }
}
