// Tiny VM record consumed by RestructureView's per-category list.

namespace FileID.ViewModels;

internal sealed class RestructureCategoryRow
{
    public required string Category { get; init; }
    public required uint Count { get; init; }

    public string CategoryDisplay => Category switch
    {
        "photo" => "Photos",
        "video" => "Videos",
        "document" => "Documents",
        "audio" => "Audio",
        "misc" => "Misc",
        _ => Category,
    };

    public string CountDisplay => Count == 1 ? "1 file" : $"{Count:N0} files";
}
