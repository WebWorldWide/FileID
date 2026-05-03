// FilePreviewSheet code-behind. Hosted inside a ContentDialog (so Esc /
// click-outside dismiss come for free). Real preview body uses the same
// shell-thumbnail provider chain Explorer uses (handles HEIC, RAW, Office,
// .pages, etc) — no special pdfium / Media Foundation roundtrips needed
// for the visual preview at this fidelity.

using System;
using System.IO;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Library;

public sealed partial class FilePreviewSheet : UserControl
{
    public string FilePath { get; private set; } = string.Empty;
    public long FileId { get; private set; }

    public FilePreviewSheet()
    {
        InitializeComponent();
    }

    public async void SetFile(string path, string kind, long sizeBytes, double? modifiedAt, long fileId = 0)
    {
        FileId = fileId;
        // Async-void → must wrap the entire body. Any unhandled exception
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
            // Settings → Install).
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
            : "—";
        var kindDisplay = kind switch
        {
            "image" => "Image",
            "video" => "Video",
            "pdf"   => "PDF",
            "doc"   => "Document",
            "audio" => "Audio",
            _       => kind,
        };
        MetadataText.Text = $"{kindDisplay} · {sizeDisplay} · modified {modifiedDisplay}";

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
    {
        if (!File.Exists(FilePath)) return;
        try
        {
            // Escape any embedded double-quotes in the path so a filename
            // like `evil"file.jpg` can't break out of the argument.
            var quoted = FilePath.Replace("\"", "\\\"");
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = "explorer.exe",
                Arguments = $"/select,\"{quoted}\"",
                UseShellExecute = true,
            });
        }
        catch { /* swallow */ }
    }

    private void OnOpenClicked(object sender, RoutedEventArgs e)
    {
        if (!File.Exists(FilePath)) return;
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = FilePath,
                UseShellExecute = true,
            });
        }
        catch { /* swallow */ }
    }

    private void OnCopyPathClicked(object sender, RoutedEventArgs e)
    {
        var dp = new Windows.ApplicationModel.DataTransfer.DataPackage();
        dp.SetText(FilePath);
        Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(dp);
    }
}
