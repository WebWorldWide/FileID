// One recommendation card in the Restructure surface — Keep / Tidy / Reorganize.
// Mirrors macOS RestructureRecommendationRow: plain-language headline + body, a
// file/folder count badge, approve/skip + review-files toggles, and an
// expand-in-place file list. The card tint + glyph come from the outcome so a
// single DataTemplate renders all three (no per-outcome template).
//
// Brushes are built lazily in getters, which x:Bind evaluates on the UI thread
// during layout — the same DispatcherObject-in-a-VM pattern MergeSuggestionVm
// uses for its face BitmapImages.

using System.Collections.ObjectModel;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.ViewModels;

internal sealed class RestructureRecommendationVm : INotifyPropertyChanged
{
    public required RestructureOutcome Outcome { get; init; }
    public required string Headline { get; init; }
    public required string BodyText { get; init; }
    public required int FileCount { get; init; }
    public required int FolderCount { get; init; }

    /// <summary>Keep is informational — no files move, no toggles, no count.</summary>
    public bool IsInformational { get; init; }

    /// <summary>Total files in this outcome (Files is capped for the inline list).</summary>
    public int MatchedCount { get; init; }

    public ObservableCollection<RestructureFileRowVm> Files { get; } = new();

    public string CountText => FileCount.ToString("N0");
    public string FolderText => FolderCount == 1 ? "1 folder" : $"{FolderCount:N0} folders";
    public Visibility ActionsVisibility => IsInformational ? Visibility.Collapsed : Visibility.Visible;
    public Visibility CountVisibility => IsInformational ? Visibility.Collapsed : Visibility.Visible;

    public string Glyph => Ch(Outcome switch
    {
        RestructureOutcome.Keep => 0xE72E,        // Lock
        RestructureOutcome.Tidy => 0xE8DE,        // MoveToFolder
        _ => 0xE8CB,                              // Sort (Reorganize)
    });

    private SolidColorBrush? _tintBrush;
    public Brush TintBrush => _tintBrush ??= new SolidColorBrush(TintColor(Outcome));

    private SolidColorBrush? _tintSoftBrush;
    public Brush TintSoftBrush
    {
        get
        {
            if (_tintSoftBrush is null)
            {
                var c = TintColor(Outcome);
                c.A = 0x2E;
                _tintSoftBrush = new SolidColorBrush(c);
            }
            return _tintSoftBrush;
        }
    }

    private bool _isApproved = true;
    public bool IsApproved
    {
        get => _isApproved;
        set
        {
            if (_isApproved == value) return;
            _isApproved = value;
            OnChanged(nameof(IsApproved));
            OnChanged(nameof(RowOpacity));
            OnChanged(nameof(ApproveLabel));
            OnChanged(nameof(ApproveGlyph));
        }
    }
    public double RowOpacity => (_isApproved || IsInformational) ? 1.0 : 0.55;
    public string ApproveLabel => _isApproved ? "Skip these" : "Approve";
    public string ApproveGlyph => Ch(_isApproved ? 0xE711 : 0xE73E); // Cancel : CheckMark

    private bool _isExpanded;
    public bool IsExpanded
    {
        get => _isExpanded;
        set
        {
            if (_isExpanded == value) return;
            _isExpanded = value;
            OnChanged(nameof(IsExpanded));
            OnChanged(nameof(ExpandedVisibility));
            OnChanged(nameof(ReviewLabel));
            OnChanged(nameof(ReviewGlyph));
        }
    }
    public Visibility ExpandedVisibility => _isExpanded ? Visibility.Visible : Visibility.Collapsed;
    public string ReviewLabel => _isExpanded ? "Hide files" : "Review files";
    public string ReviewGlyph => Ch(_isExpanded ? 0xE70D : 0xE76C); // ChevronDown : ChevronRight

    public Visibility SeeAllVisibility => MatchedCount > Files.Count ? Visibility.Visible : Visibility.Collapsed;
    public string SeeAllText => $"See all {MatchedCount:N0} files";

    private bool _isHighlighted;
    public bool IsHighlighted
    {
        get => _isHighlighted;
        set
        {
            if (_isHighlighted == value) return;
            _isHighlighted = value;
            OnChanged(nameof(IsHighlighted));
            OnChanged(nameof(HighlightVisibility));
        }
    }
    public Visibility HighlightVisibility => _isHighlighted ? Visibility.Visible : Visibility.Collapsed;

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnChanged(string name) => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));

    private static string Ch(int codePoint) => ((char)codePoint).ToString();

    private static Color TintColor(RestructureOutcome o) => o switch
    {
        RestructureOutcome.Keep => Color.FromArgb(0xFF, 0x6C, 0xC2, 0x4A),  // green
        RestructureOutcome.Tidy => Color.FromArgb(0xFF, 0xFF, 0x9F, 0x45),  // orange
        _ => Color.FromArgb(0xFF, 0xFF, 0xCC, 0x00),                         // gold
    };
}
