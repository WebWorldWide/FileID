// Tier -> outcome mapping guard. RestructureView and DrillDownSheet both group
// engine moves through RestructureGrouping.OutcomeForTier; a silent change here
// would mis-sort files between the Keep / Tidy / Reorganize cards.

using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public class RestructureGroupingTests
{
    // expected is passed as a string (not the internal RestructureOutcome enum)
    // so the public xUnit test signature stays accessibility-consistent.
    [Theory]
    [InlineData("Mixed", "Tidy")]
    [InlineData("Junk", "Reorganize")]
    [InlineData("Anchor", "Keep")]
    [InlineData(null, "Reorganize")]
    [InlineData("", "Reorganize")]
    [InlineData("future-tier", "Reorganize")]
    public void OutcomeForTier_MapsEngineTierToOutcome(string? tier, string expected)
    {
        Assert.Equal(expected, RestructureGrouping.OutcomeForTier(tier).ToString());
    }
}
