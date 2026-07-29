using System.Collections.Generic;
using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public class RestructurePlanPresentationTests
{
    [Fact]
    public void TotalMoves_UsesEngineAuthoritativeStoredPlanCount()
    {
        var plan = new RestructurePlan(
            @"C:\Library",
            [],
            [],
            PlanId: "plan-1",
            TotalMoves: 4_500_000_000,
            Truncated: true);

        Assert.Equal(4_500_000_000UL, RestructurePlanPresentation.TotalMoves(plan));
    }

    [Fact]
    public void CompleteConfidenceCounts_MustCoverTheAuthoritativeFullPlan()
    {
        var plan = new RestructurePlan(
            @"C:\Library",
            [],
            [],
            PlanId: "plan-1",
            TotalMoves: 100,
            Truncated: true,
            ConfidenceCounts: new RestructureConfidenceCounts(
                Auto: 60,
                Review: 25,
                Ask: 10,
                Unknown: 5));

        Assert.True(
            RestructurePlanPresentation.TryGetCompleteConfidenceCounts(
                plan,
                out var counts));
        Assert.Equal(60UL, counts.Auto);
    }

    [Fact]
    public void CompleteConfidenceCounts_RejectsMissingMismatchedAndOverflowingTotals()
    {
        var missing = StoredPlan(null);
        var mismatched = StoredPlan(new RestructureConfidenceCounts(60, 25, 10, 4));
        var overflowing = StoredPlan(
            new RestructureConfidenceCounts(ulong.MaxValue, 1, 0, 0));

        Assert.False(
            RestructurePlanPresentation.TryGetCompleteConfidenceCounts(
                missing,
                out _));
        Assert.False(
            RestructurePlanPresentation.TryGetCompleteConfidenceCounts(
                mismatched,
                out _));
        Assert.False(
            RestructurePlanPresentation.TryGetCompleteConfidenceCounts(
                overflowing,
                out _));
    }

    [Theory]
    [InlineData(1, true, true)]
    [InlineData(0, true, false)]
    [InlineData(1, false, false)]
    public void StoredPlanApply_RequiresAuthoritativeAutoWorkAndSafeVisibleSample(
        ulong auto,
        bool visibleSampleIsSafe,
        bool expected)
    {
        var plan = StoredPlan(
            new RestructureConfidenceCounts(
                Auto: auto,
                Review: 99 - auto,
                Ask: 0,
                Unknown: 1));

        Assert.Equal(
            expected,
            RestructurePlanPresentation.CanApplyStoredPlan(
                plan,
                visibleSampleIsSafe));
    }

    [Theory]
    [InlineData(10, 0, 0, true)]
    [InlineData(10, 1, 0, true)]
    [InlineData(10, 8, 0, false)]
    [InlineData(10, 0, 8, false)]
    [InlineData(0, 0, 0, false)]
    public void MissingContentSignals_RequiresEightyPercentEligibleCoverage(
        int total,
        int clip,
        int text,
        bool expected)
        => Assert.Equal(
            expected,
            RestructurePlanPresentation.HasMissingContentSignals(total, clip, text));

    [Theory]
    [InlineData(@"F:\", true)]
    [InlineData(@"F:", true)]
    [InlineData(@"F:\Adlon Drive", false)]
    [InlineData("", false)]
    public void DriveRootScope_IsDetected(string path, bool expected)
        => Assert.Equal(expected, RestructurePlanPresentation.IsDriveRoot(path));

    [Fact]
    public void TopCategories_IsBoundedMergedAndDeterministic()
    {
        IReadOnlyList<RestructureCategoryCount> categories =
        [
            new("Photos", 7),
            new("documents", 4),
            new("Documents", 6),
            new(" ", 5),
            new("Travel", 5),
            new("Empty", 0),
        ];

        var result = RestructurePlanPresentation.TopCategories(categories, cap: 3);

        Assert.Collection(
            result,
            item =>
            {
                Assert.Equal("documents", item.Category);
                Assert.Equal(10UL, item.Count);
            },
            item =>
            {
                Assert.Equal("Photos", item.Category);
                Assert.Equal(7UL, item.Count);
            },
            item =>
            {
                Assert.Equal("Travel", item.Category);
                Assert.Equal(5UL, item.Count);
            });
    }

    [Fact]
    public void CategoryCount_IgnoresEmptyBucketsAndCaseDuplicates()
    {
        IReadOnlyList<RestructureCategoryCount> categories =
        [
            new("Documents", 3),
            new("documents", 2),
            new("", 1),
            new("Unsorted", 4),
            new("Empty", 0),
        ];

        Assert.Equal(2, RestructurePlanPresentation.CategoryCount(categories));
    }

    [Fact]
    public void InspectPreview_BlocksDuplicateDestinationsAndBoundaryEscapes()
    {
        var plan = new RestructurePlan(
            @"C:\Library",
            [
                Move(1, @"C:\Library\A.txt", @"C:\Library\Sorted\File.txt"),
                Move(2, @"C:\Library\B.txt", @"c:\library\sorted\file.txt"),
                Move(3, @"C:\Other\C.txt", @"C:\Library\Sorted\C.txt"),
            ],
            []);

        var result = RestructurePlanPresentation.InspectPreview(plan);

        Assert.False(result.IsSafe);
        Assert.Equal(1, result.DuplicateDestinations);
        Assert.Equal(1, result.OutsideRootMoves);
        Assert.Equal(0, result.InvalidPaths);
    }

    [Fact]
    public void InspectPreview_AcceptsDistinctMovesWithinLibrary()
    {
        var plan = new RestructurePlan(
            @"C:\Library",
            [
                Move(1, @"C:\Library\A.txt", @"C:\Library\Sorted\A.txt"),
                Move(2, @"C:\Library\B.txt", @"C:\Library\Sorted\B.txt"),
            ],
            []);

        Assert.True(RestructurePlanPresentation.InspectPreview(plan).IsSafe);
    }

    private static RestructureMove Move(long id, string source, string destination)
        => new(id, source, destination, "Documents", "Mixed", "auto");

    private static RestructurePlan StoredPlan(
        RestructureConfidenceCounts? confidenceCounts)
        => new(
            @"C:\Library",
            [],
            [],
            PlanId: "plan-1",
            TotalMoves: 100,
            Truncated: true,
            ConfidenceCounts: confidenceCounts);
}
