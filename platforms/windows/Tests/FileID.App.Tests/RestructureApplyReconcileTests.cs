// R6-05 regression: leaving the Restructure tab mid-apply unsubscribes the view,
// dropping the apply-completion event; the static single-flight guard would then
// stay engaged for the session (Apply buttons stuck disabled). OnLoaded replays
// the handlers, which gate on IsUnhandledCompletion: a NEW completion reference
// (arrived while unloaded) must surface; the SAME reference (already surfaced, or
// apply still in flight with an unchanged slot) must be a no-op.

using FileID.IpcSchema;
using FileID.Views.Restructure;
using Xunit;

namespace FileID.App.Tests;

public class RestructureApplyReconcileTests
{
    [Fact]
    public void NullCompletion_IsNotUnhandled()
        => Assert.False(RestructureView.IsUnhandledCompletion<RestructureApplyResult>(null, null));

    [Fact]
    public void FirstCompletion_NoPriorSurfaced_IsUnhandled()
        => Assert.True(RestructureView.IsUnhandledCompletion(new RestructureApplyResult(5, 0), null));

    [Fact]
    public void SameReference_AlreadySurfaced_IsNoOp()
    {
        var r = new RestructureApplyResult(0, 3);
        Assert.False(RestructureView.IsUnhandledCompletion(r, r));
    }

    [Fact] // value-equal but a NEW instance must still surface (Set swapped the ref)
    public void NewReference_ValueEqual_ArrivedWhileUnloaded_IsUnhandled()
    {
        var surfaced = new RestructureApplyResult(0, 3);
        var arrived = new RestructureApplyResult(0, 3);
        Assert.True(RestructureView.IsUnhandledCompletion(arrived, surfaced));
    }

    [Fact]
    public void Error_NewReference_IsUnhandled()
    {
        var prev = new EngineError("apply_restructure", "x", null);
        var fresh = new EngineError("apply_restructure", "x", null);
        Assert.True(RestructureView.IsUnhandledCompletion(fresh, prev));
    }
}
