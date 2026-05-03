// FilePreviewSheet code-behind. Hosted inside a ContentDialog (so Esc /
// click-outside dismiss come for free). The actual visual preview body
// is wired in Phase 2.6 alongside the shell::thumbnail real renderer +
// pdfium-render PDF + Media Foundation video keyframe.

using System;
using System.IO;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Library;

public sealed partial class FilePreviewSheet : UserControl
{
    public string FilePath { get; private set; } = string.Empty;

    public FilePreviewSheet()
    {
        InitializeComponent();
    }

    public void SetFile(string path, string kind, long sizeBytes, double? modifiedAt)
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

        // Pick a kind-glyph for the placeholder. Phase 2.6 swaps this
        // out for the real preview Image when shell::thumbnail returns
        // RGBA bytes we can wrap as a SoftwareBitmapSource. Glyphs use
        // \uXXXX escapes so the bytes survive any source-control pass.
        KindGlyphIcon.Glyph = kind switch
        {
            "image" => "", // Photo
            "video" => "", // Video
            "pdf"   => "", // PDF / document
            "doc"   => "", // Document
            "audio" => "", // Music note
            _       => "", // Folder / generic
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
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo
            {
                FileName = "explorer.exe",
                Arguments = $"/select,\"{FilePath}\"",
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
