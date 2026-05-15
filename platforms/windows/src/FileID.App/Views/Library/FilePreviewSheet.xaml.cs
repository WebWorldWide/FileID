// FilePreviewSheet code-behind. Hosted inside a ContentDialog (so Esc /
// click-outside dismiss come for free). Real preview body uses the same
// shell-thumbnail provider chain Explorer uses (handles HEIC, RAW, Office,
// .pages, etc) -- no special pdfium / Media Foundation roundtrips needed
// for the visual preview at this fidelity.

using System;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Library;

public sealed partial class FilePreviewSheet : UserControl
{
    public string FilePath { get; private set; } = string.Empty;
    public long FileId { get; private set; }

    // V14.7.2: sibling navigation. The host (Library tab) sets the
    // siblings list + current index when opening the sheet so <-/->
    // can step through tiles without closing + reopening.
    private System.Collections.Generic.IReadOnlyList<FileID.ViewModels.FileTile>? _siblings;
    private int _siblingIndex;

    public event EventHandler? RequestClose;

    public FilePreviewSheet()
    {
        InitializeComponent();
        // Esc closes the sheet via the dialog's CloseButton; we also
        // hook the KeyDown for <-/-> siblings nav so users don't have
        // to bring the dialog buttons into focus.
        KeyDown += OnKeyDown;
        IsTabStop = true;
    }

    /// <summary>V14.7.2: set the siblings list so the user can <-/->
    /// through neighboring tiles without closing the sheet.
    /// Internal because FileTile is internal; LibraryView calls this
    /// from the same assembly when opening the sheet.</summary>
    internal void SetSiblings(System.Collections.Generic.IReadOnlyList<FileID.ViewModels.FileTile> siblings, int currentIndex)
    {
        _siblings = siblings;
        _siblingIndex = currentIndex;
        UpdateNavButtons();
    }

    private void UpdateNavButtons()
    {
        if (PrevButton == null || NextButton == null) return;
        PrevButton.IsEnabled = _siblings != null && _siblingIndex > 0;
        NextButton.IsEnabled = _siblings != null && _siblings.Count > 0 && _siblingIndex < _siblings.Count - 1;
    }

    private void OnKeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        switch (e.Key)
        {
            case Windows.System.VirtualKey.Left:
                NavigateSibling(-1);
                e.Handled = true;
                break;
            case Windows.System.VirtualKey.Right:
                NavigateSibling(+1);
                e.Handled = true;
                break;
            case Windows.System.VirtualKey.Escape:
                RequestClose?.Invoke(this, EventArgs.Empty);
                e.Handled = true;
                break;
        }
    }

    private void OnPrevClicked(object sender, RoutedEventArgs e) => NavigateSibling(-1);
    private void OnNextClicked(object sender, RoutedEventArgs e) => NavigateSibling(+1);

    private void NavigateSibling(int delta)
    {
        if (_siblings is null || _siblings.Count == 0) return;
        var next = _siblingIndex + delta;
        if (next < 0 || next >= _siblings.Count) return;
        _siblingIndex = next;
        var t = _siblings[next];
        SetFile(t.Path, t.Kind, t.SizeBytes, t.ModifiedAt, t.Id, t.HasFaces, t.HasText);
        UpdateNavButtons();
    }

    public async void SetFile(string path, string kind, long sizeBytes, double? modifiedAt, long fileId = 0,
                               bool hasFaces = false, bool hasText = false)
    {
        FileId = fileId;
        // Toggle the badge overlay synchronously (don't wait for the async
        // thumbnail render). Tile-level booleans come from the DB — no
        // additional IPC needed here.
        FacesBadge.Visibility = hasFaces ? Visibility.Visible : Visibility.Collapsed;
        TextBadge.Visibility = hasText ? Visibility.Visible : Visibility.Collapsed;

        // Clear stale tag input + status between siblings.
        TagInput.Text = string.Empty;
        TagStatusText.Visibility = Visibility.Collapsed;

        // Async-void -> must wrap the entire body. Any unhandled exception
        // here would terminate the dispatcher and crash the window.
        try
        {
            await SetFileCoreAsync(path, kind, sizeBytes, modifiedAt);
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("FilePreviewSheet.SetFile threw: " + ex);
        }
    }

    private async void OnAnalyzeClicked(object sender, RoutedEventArgs e)
    {
        if (FileId <= 0) return;
        try
        {
            AnalyzeButton.IsEnabled = false;
            // Use the recommended VLM model id; the engine will surface
            // a friendly error if the model isn't installed (Welcome /
            // Settings -> Install).
            await ViewModels.EngineClient.Instance.DeepAnalyzeFileAsync(FileId, "qwen2_5_vl_3b");
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("Analyze failed: " + ex);
        }
        finally
        {
            AnalyzeButton.IsEnabled = true;
        }
    }

    private async Task SetFileCoreAsync(string path, string kind, long sizeBytes, double? modifiedAt)
    {
        FilePath = path;
        FileNameText.Text = Path.GetFileName(path);
        PathText.Text = path;

        var sizeDisplay = FormatSize(sizeBytes);
        var modifiedDisplay = modifiedAt.HasValue
            ? DateTimeOffset.FromUnixTimeMilliseconds((long)(modifiedAt.Value * 1000)).LocalDateTime.ToString("g")
            : "--";
        var kindDisplay = kind switch
        {
            "image" => "Image",
            "video" => "Video",
            "pdf"   => "PDF",
            "doc"   => "Document",
            "audio" => "Audio",
            _       => kind,
        };
        MetadataText.Text = $"{kindDisplay} * {sizeDisplay} * modified {modifiedDisplay}";

        // Kind-glyph fallback for non-renderable kinds.
        KindGlyphIcon.Glyph = kind switch
        {
            "image" => "", // Photo
            "video" => "", // Video
            "pdf"   => "", // Document
            "doc"   => "", // Document
            "audio" => "", // Music note
            _       => "", // Folder / generic
        };

        // Show the rendered preview for image/video/pdf via the same
        // shell-thumbnail provider chain Explorer uses (handles HEIC,
        // RAW, Office, .pages, etc). Falls back to the kind glyph for
        // audio/other/failures.
        if (kind is "image" or "video" or "pdf" or "doc")
        {
            try
            {
                var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
                using var thumb = await file.GetThumbnailAsync(
                    Windows.Storage.FileProperties.ThumbnailMode.SingleItem,
                    1024,
                    Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale);
                if (thumb != null && thumb.Size > 0)
                {
                    var bmp = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage();
                    await bmp.SetSourceAsync(thumb);
                    PreviewImage.Source = bmp;
                    PreviewImage.Visibility = Visibility.Visible;
                    PlaceholderPanel.Visibility = Visibility.Collapsed;
                    return;
                }
            }
            catch { /* fall through to placeholder */ }
        }

        PreviewImage.Visibility = Visibility.Collapsed;
        PlaceholderPanel.Visibility = Visibility.Visible;
        PlaceholderText.Text = kind switch
        {
            "audio" => Path.GetFileName(path),
            _       => "No preview available for this file type.",
        };
    }

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:0.#} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):0.#} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):0.##} GB";
    }

    private void OnRevealClicked(object sender, RoutedEventArgs e)
        => Services.DebugLog.SafeRun(nameof(OnRevealClicked), () => Services.SafeOpen.Reveal(FilePath));

    private void OnOpenClicked(object sender, RoutedEventArgs e)
        => Services.DebugLog.SafeRun(nameof(OnOpenClicked), () =>
        {
            // SEC-9: ext-gated; falls back to Reveal for non-allowlisted ext.
            if (!Services.SafeOpen.TryOpenFile(FilePath))
            {
                Services.SafeOpen.Reveal(FilePath);
            }
        });

    private void OnCopyPathClicked(object sender, RoutedEventArgs e)
        => Services.DebugLog.SafeRun(nameof(OnCopyPathClicked), () =>
        {
            var dp = new Windows.ApplicationModel.DataTransfer.DataPackage();
            dp.SetText(FilePath);
            Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(dp);
        });

    private void OnTagInputKeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter)
        {
            ApplyDraftTags();
            e.Handled = true;
        }
    }

    private void OnApplyTagsClicked(object sender, RoutedEventArgs e) => ApplyDraftTags();

    private async void ApplyDraftTags()
    {
        if (FileId <= 0)
        {
            ShowTagStatus("Reopen the preview before tagging — no file id.");
            return;
        }
        var raw = (TagInput.Text ?? string.Empty).Trim();
        if (raw.Length == 0) return;
        var tags = raw
            .Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Where(t => t.Length > 0)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .ToArray();
        if (tags.Length == 0)
        {
            ShowTagStatus("Type one or more tags separated by commas.");
            return;
        }
        try
        {
            ApplyTagsButton.IsEnabled = false;
            await ViewModels.EngineClient.Instance.ApplyTagsAsync(new long[] { FileId }, tags, mode: "add");
            ShowTagStatus($"Added {tags.Length} tag" + (tags.Length == 1 ? string.Empty : "s") + ".");
            TagInput.Text = string.Empty;
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("FilePreviewSheet.ApplyDraftTags failed: " + ex);
            ShowTagStatus("Failed: " + ex.Message);
        }
        finally
        {
            ApplyTagsButton.IsEnabled = true;
        }
    }

    private void ShowTagStatus(string msg)
    {
        TagStatusText.Text = msg;
        TagStatusText.Visibility = Visibility.Visible;
    }
}
