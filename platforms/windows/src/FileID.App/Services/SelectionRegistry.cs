// Selection shared across views: the app builds a fresh LibraryViewModel per tab navigation, so LibraryView publishes its selection here and other tabs (Deep Analyze) read it.

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
