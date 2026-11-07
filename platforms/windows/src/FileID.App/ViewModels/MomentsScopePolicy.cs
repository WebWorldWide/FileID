namespace FileID.ViewModels;

/// <summary>When the title-bar location changes, Search &amp; Moments runs its current search again
/// in the new location (macOS CatalogWorkbench). A search that is still running finishes first and
/// the re-run follows it.</summary>
internal static class MomentsScopePolicy
{
    public enum Step { Nothing, Search, AfterCurrent }

    public static Step OnScopeChanged(long? shown, long? chosen, string? query, bool busy)
    {
        if (shown == chosen || string.IsNullOrWhiteSpace(query)) return Step.Nothing;
        return busy ? Step.AfterCurrent : Step.Search;
    }

    public static bool RunPending(bool pending, string? query, bool busy) =>
        pending && !busy && !string.IsNullOrWhiteSpace(query);
}
