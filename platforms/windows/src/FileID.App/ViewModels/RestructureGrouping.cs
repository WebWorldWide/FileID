// Single source of truth for mapping an engine RestructureMove.Tier to a
// recommendation outcome. Shared by RestructureView (card grouping) and
// DrillDownSheet (the "see all" filter) so the two can't drift apart.

namespace FileID.ViewModels;

internal static class RestructureGrouping
{
    /// <summary>Mixed -> Tidy, Anchor -> Keep, Junk/null/unknown -> Reorganize.
    /// Mirrors macOS RestructureView.outcomeFor.</summary>
    public static RestructureOutcome OutcomeForTier(string? tier) => tier switch
    {
        "Mixed" => RestructureOutcome.Tidy,
        "Anchor" => RestructureOutcome.Keep,
        _ => RestructureOutcome.Reorganize,
    };
}
