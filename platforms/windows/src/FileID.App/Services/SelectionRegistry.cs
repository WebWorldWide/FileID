// Cross-view selection registry.
//
// macOS Library selects files in LibraryView and the DeepAnalyzeView reads
// those same selected file IDs to expose "Analyze selected". The Windows
// app builds a fresh LibraryViewModel per tab navigation, so a static
// global registry is the simplest equivalent — LibraryView publishes its
// selection here whenever the user changes it; DeepAnalyzeView (and any
// other tab) reads.
//
// PreviewedFileId is the corollary for "Analyze current" — set when
// FilePreviewSheet opens, cleared when it closes.

using System;
using System.Collections.Generic;
using System.ComponentModel;

namespace FileID.Services;

internal sealed class SelectionRegistry : INotifyPropertyChanged
{
    public static SelectionRegistry Instance { get; } = new();

    private IReadOnlyList<long> _librarySelection = Array.Empty<long>();
    public IReadOnlyList<long> LibrarySelection
    {
        get => _librarySelection;
        set
        {
            // ReferenceEquals + Count short-circuit lets the binding stay
            // quiet on no-op publishes (the LibraryView's PropertyChanged
            // listener fires on every IsSelected toggle).
            if (ReferenceEquals(_librarySelection, value)) return;
            _librarySelection = value ?? Array.Empty<long>();
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LibrarySelection)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasLibrarySelection)));
        }
    }

    public bool HasLibrarySelection => _librarySelection.Count > 0;

    private long? _previewedFileId;
    public long? PreviewedFileId
    {
        get => _previewedFileId;
        set
        {
            if (_previewedFileId == value) return;
            _previewedFileId = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(PreviewedFileId)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasPreviewedFile)));
        }
    }

    public bool HasPreviewedFile => _previewedFileId.HasValue;

    public event PropertyChangedEventHandler? PropertyChanged;
}
