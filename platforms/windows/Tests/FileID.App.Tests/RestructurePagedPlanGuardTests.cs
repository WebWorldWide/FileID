// F9/F12 (paged plans): the engine's plan path emits plan_restructure_failed /
// plan_restructure_db / plan_restructure_store and returns WITHOUT a plan event,
// so every kind must unfreeze "Computing plan..."; and a fresh plan arriving
// while an apply is still in flight must NOT release the apply single-flight
// guard (a second concurrent apply truncates the first run's undo journal).

using FileID.IpcSchema;
using FileID.ViewModels;
using FileID.Views.Restructure;
using Xunit;

namespace FileID.App.Tests;

public class RestructurePagedPlanGuardTests
{
    [Theory]
    [InlineData("plan_restructure_failed")]
    [InlineData("plan_restructure_db")]
    [InlineData("plan_restructure_store")]
    public void EnginePlanErrorKinds_AreRecognized(string kind)
        => Assert.True(RestructureView.IsPlanRestructureErrorKind(kind));

    [Theory]
    [InlineData("apply_restructure")]
    [InlineData("undo_restructure")]
    [InlineData("scan")]
    public void NonPlanKinds_AreNotPlanErrors(string kind)
        => Assert.False(RestructureView.IsPlanRestructureErrorKind(kind));

    [Fact] // F12 core: fresh plan mid-apply keeps the guard engaged
    public void FreshPlanWhileApplyInFlight_DoesNotReleaseGuard()
    {
        var applying = new object();
        var fresh = new object();
        Assert.False(RestructureView.ShouldReleaseApplyGuardOnPlanArrival(true, fresh, applying));
    }

    [Fact] // post-apply re-plan (result already arrived) releases
    public void FreshPlanAfterApplyCompleted_Releases()
    {
        var applying = new object();
        var fresh = new object();
        Assert.True(RestructureView.ShouldReleaseApplyGuardOnPlanArrival(false, fresh, applying));
    }

    [Fact] // R6-04: a returning view re-rendering the SAME cached pre-apply plan
    public void SameCachedPlanReference_NeverReleases()
    {
        var applying = new object();
        Assert.False(RestructureView.ShouldReleaseApplyGuardOnPlanArrival(false, applying, applying));
        Assert.False(RestructureView.ShouldReleaseApplyGuardOnPlanArrival(true, applying, applying));
    }

    // R6-06: engine-lifecycle release rule — the engine process that owned the
    // in-flight apply died (spawn generation moved), so its result/error can
    // never arrive and the guard must release; a live engine (generation
    // unchanged) or a disengaged guard must not.

    [Fact]
    public void EngineDiedMidApply_ReleasesGuard()
        => Assert.True(RestructureView.ShouldReleaseApplyGuardOnEngineChange(
            guardEngaged: true, applyingGeneration: 3, currentGeneration: 4));

    [Fact]
    public void SameEngineProcess_KeepsGuard()
        => Assert.False(RestructureView.ShouldReleaseApplyGuardOnEngineChange(
            guardEngaged: true, applyingGeneration: 3, currentGeneration: 3));

    [Fact]
    public void GuardNotEngaged_NeverReleases()
        => Assert.False(RestructureView.ShouldReleaseApplyGuardOnEngineChange(
            guardEngaged: false, applyingGeneration: 0, currentGeneration: 7));

    [Fact]
    public void FrozenPlan_MustStillBeBothLiveAndRendered()
    {
        var frozen = new object();
        var replacement = new object();

        Assert.True(RestructureView.IsFrozenPlanCurrent(frozen, frozen, frozen));
        Assert.False(RestructureView.IsFrozenPlanCurrent(frozen, replacement, frozen));
        Assert.False(RestructureView.IsFrozenPlanCurrent(frozen, frozen, replacement));
        Assert.False(RestructureView.IsFrozenPlanCurrent(null, null, null));
    }

    [Fact]
    public void DetailCard_MustBeTheExactRenderedRecommendation()
    {
        var rendered = Recommendation();
        var replacement = Recommendation();

        Assert.True(RestructureView.IsCurrentRecommendation(rendered, rendered));
        Assert.False(RestructureView.IsCurrentRecommendation(rendered, replacement));
        Assert.False(RestructureView.IsCurrentRecommendation(null, rendered));
    }

    [Fact]
    public void SelectableDrillDown_PreservesEverySharedRowWithoutACap()
    {
        var rows = Enumerable.Range(0, 250)
            .Select(index => new RestructureFileRowVm
            {
                Move = new RestructureMove(
                    index,
                    $@"C:\Library\Source\{index}.jpg",
                    $@"C:\Library\Photos\{index}.jpg",
                    "Photos",
                    "Mixed",
                    "review"),
            })
            .ToArray();

        var prepared = DrillDownSheet.PrepareSelectableRows(rows);

        Assert.Same(rows, prepared);
        Assert.Equal(250, prepared.Count);
        Assert.Same(rows[249], prepared[249]);
    }

    [Fact]
    public void RepeaterItemResolution_UsesTheAuthoritativeRealizedIndex()
    {
        var first = new RestructureFileRowVm
        {
            Move = new RestructureMove(
                1,
                @"C:\Library\Source\1.jpg",
                @"C:\Library\Photos\1.jpg",
                "Photos",
                "Mixed",
                "review"),
        };
        var second = new RestructureFileRowVm
        {
            Move = new RestructureMove(
                2,
                @"C:\Library\Source\2.jpg",
                @"C:\Library\Photos\2.jpg",
                "Photos",
                "Mixed",
                "review"),
        };
        var rows = new System.Collections.ObjectModel.ObservableCollection<RestructureFileRowVm>
        {
            first,
            second,
        };

        Assert.Same(
            second,
            RestructureView.ResolveRepeaterItem<RestructureFileRowVm>(rows, 1));
        Assert.Null(RestructureView.ResolveRepeaterItem<RestructureFileRowVm>(rows, -1));
        Assert.Null(RestructureView.ResolveRepeaterItem<RestructureFileRowVm>(rows, rows.Count));
    }

    private static RestructureRecommendationVm Recommendation()
        => new()
        {
            Outcome = RestructureOutcome.Tidy,
            Headline = "Tidy",
            BodyText = "Review files",
            FileCount = 1,
            FolderCount = 1,
        };
}
