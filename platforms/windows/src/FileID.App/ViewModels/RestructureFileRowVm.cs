// Per-file row inside a Restructure recommendation card's expand-in-place list
// (and the "see all" drill-down). Mirrors macOS RestructureView.proposalRow:
// a checkbox + file glyph + filename + "from <folder>" subtitle. IsSelected
// flows back to RestructureView via SelectionChanged so the apply count and the
// actual applied move set never diverge.

using System;
using System.ComponentModel;
using System.IO;
using FileID.IpcSchema;

namespace FileID.ViewModels;

internal sealed class RestructureFileRowVm : INotifyPropertyChanged
{
    public required RestructureMove Move { get; init; }

    /// <summary>Raised whenever IsSelected flips so the owning view recomputes
    /// the apply count + button state. Set once at construction.</summary>
    public Action? SelectionChanged { get; init; }

    public long FileId => Move.FileId;
    public string FileName => Path.GetFileName(Move.Source);
    public string FromToTooltip => $"{Move.Source}  ->  {Move.Destination}";

    public string FromLabel
    {
        get
        {
            var dir = Path.GetDirectoryName(Move.Source);
            var leaf = string.IsNullOrEmpty(dir) ? null : Path.GetFileName(dir.TrimEnd('\\', '/'));
            return "from " + (string.IsNullOrEmpty(leaf) ? "root" : leaf);
        }
    }

    public string FileGlyph => GlyphFor(FileName);

    private bool _isSelected = true;
    public bool IsSelected
    {
        get => _isSelected;
        set
        {
            if (_isSelected == value) return;
            _isSelected = value;
            OnChanged(nameof(IsSelected));
            SelectionChanged?.Invoke();
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnChanged(string name) => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));

    // Segoe MDL2 Assets glyph code points, mirroring macOS fileIcon(forFilename:).
    // Built from an int so the source stays pure ASCII (no embedded PUA chars).
    private static string GlyphFor(string name)
    {
        var ext = Path.GetExtension(name).TrimStart('.').ToLowerInvariant();
        int cp = ext switch
        {
            "jpg" or "jpeg" or "png" or "heic" or "heif" or "gif" or "tiff" or "tif"
                or "bmp" or "webp" or "raw" or "dng" => 0xE91B, // Photo
            "mp4" or "mov" or "m4v" or "avi" or "mkv" or "webm" => 0xE714, // Video
            "pdf" => 0xEA90,                                     // PDF
            "doc" or "docx" or "rtf" or "txt" or "md" or "pages" => 0xE8A5, // Document
            "mp3" or "m4a" or "wav" or "flac" or "aac" or "ogg" => 0xE8D6,  // Audio
            _ => 0xE7C3,                                         // Page
        };
        return ((char)cp).ToString();
    }
}
