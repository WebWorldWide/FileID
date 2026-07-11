// F9/F12 (paged plans): the engine's plan path emits plan_restructure_failed /
// plan_restructure_db / plan_restructure_store and returns WITHOUT a plan event,
// so every kind must unfreeze "Computing plan..."; and a fresh plan arriving
// while an apply is still in flight must NOT release the apply single-flight
// guard (a second concurrent apply truncates the first run's undo journal).

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
}
