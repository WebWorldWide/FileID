namespace FileID.ViewModels;

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
