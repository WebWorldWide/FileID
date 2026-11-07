using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class TakeProposalPolicyTests
{
    private static readonly string[] SessionAndCameras = { "captureSession", "simultaneousCameras" };
    private static readonly string[] MixedEvidence = { "captureSession", "visualSimilarity", "future", "sameCamera" };

    private static CatalogJob Job(string state, double updated = 1) => new("take-grouping", "takeGrouping", Array.Empty<long>(), state, 0.5, null, 0, updated);

    private static CatalogTakeAutoGroup Group(string key, string status = "open", string? relation = "alternateViews") => new(
        key, "photos", status, 0, 0.9,
        new[] { new CatalogTakeShot(4, new long[] { 4, 5 }, "/p/a.JPG"), new CatalogTakeShot(6, new long[] { 6 }, "/p/b.JPG") },
        relation, SessionAndCameras);

    [Fact]
    public void RelationChipsAndEvidenceText()
    {
        Assert.Equal("Repeated tries", TakeProposalPolicy.RelationLabel("repeatedAttempts"));
        Assert.Equal("Other angles", TakeProposalPolicy.RelationLabel("alternateViews"));
        Assert.Equal("Not sure", TakeProposalPolicy.RelationLabel("uncertain"));
        Assert.Null(TakeProposalPolicy.RelationLabel(null));
        Assert.Null(TakeProposalPolicy.RelationLabel("other"));
        Assert.Equal(TakeProposalPolicy.RelationTone.Gold, TakeProposalPolicy.Tone("repeatedAttempts"));
        Assert.Equal(TakeProposalPolicy.RelationTone.Info, TakeProposalPolicy.Tone("alternateViews"));
        Assert.Equal(TakeProposalPolicy.RelationTone.Ai, TakeProposalPolicy.Tone("uncertain"));
        foreach (var relation in new[] { "repeatedAttempts", "alternateViews", "uncertain" }) Assert.NotNull(TakeProposalPolicy.RelationHelp(relation));
        foreach (var code in new[] { "captureSession", "visualSimilarity", "sharedSubject", "sameCamera", "simultaneousCameras", "mixedCameras", "cameraDetailsMissing" })
            Assert.NotNull(TakeProposalPolicy.EvidenceSentence(code));
        Assert.Equal("Taken close together. They look alike. One camera took them all.",
            TakeProposalPolicy.EvidenceText(MixedEvidence));
        Assert.Equal("", TakeProposalPolicy.EvidenceText(null));
        Assert.StartsWith("Tries of the same shot", TakeProposalPolicy.Summary(Group("k", relation: null) with { Evidence = null }), StringComparison.Ordinal);
        Assert.StartsWith("Taken close together. Different cameras took them at the same moment. Different cameras caught", TakeProposalPolicy.Summary(Group("k")), StringComparison.Ordinal);
    }

    [Fact]
    public void JobFindingPushesAndReload()
    {
        var timeline = new CatalogJob("t", "timelineSample", new long[] { 1 }, "running", 0, null, 0, 0);
        Assert.Equal("paused", TakeProposalPolicy.Job(new[] { timeline, Job("paused") })?.State);
        Assert.Null(TakeProposalPolicy.Job(null));
        Assert.True(TakeProposalPolicy.CanFind(null) && TakeProposalPolicy.CanFind(Job("paused")) && TakeProposalPolicy.CanFind(Job("completed")));
        Assert.False(TakeProposalPolicy.CanFind(Job("queued")) || TakeProposalPolicy.CanFind(Job("running")));
        Assert.True(TakeProposalPolicy.Finished(Job("running"), Job("completed")));
        Assert.True(TakeProposalPolicy.Finished(null, Job("completed")));
        Assert.True(TakeProposalPolicy.Finished(Job("completed", 1), Job("completed", 2)));
        Assert.False(TakeProposalPolicy.Finished(Job("completed"), Job("completed")));
        Assert.False(TakeProposalPolicy.Finished(Job("queued"), Job("running")));
        Assert.False(TakeProposalPolicy.ShowJob(null) || TakeProposalPolicy.ShowJob(Job("completed")));
        Assert.True(TakeProposalPolicy.ShowJob(Job("paused")));
        var push = new CatalogResponse("take-grouping", "ok", null, Array.Empty<CatalogHit>(), Array.Empty<CatalogChapter>(), new[] { Job("running") });
        Assert.True(TakeProposalPolicy.IsJobPush(push));
        Assert.False(TakeProposalPolicy.IsJobPush(push with { RequestID = "other" }));
        Assert.False(TakeProposalPolicy.IsJobPush(push with { Jobs = new[] { timeline } }));
        Assert.False(TakeProposalPolicy.IsJobPush(null));
        Assert.Equal("enqueueTakeGrouping", TakeProposalPolicy.EnqueueRequest("e").Action);
        var read = TakeProposalPolicy.ProposalsRequest("p");
        Assert.Equal(("takeGroupProposals", 100), (read.Action, read.Limit));
        Assert.Equal("Not looked yet. Find related tries to search the library.", TakeProposalPolicy.EmptyText(null));
    }

    [Fact]
    public void LabelsSavedMembersAndNextProposal()
    {
        Assert.Equal("2 photos, Other angles, Sat 4 Jul, skipped. Taken close together. Different cameras took them at the same moment.",
            TakeProposalPolicy.Spoken(Group("k", "skipped"), "Sat 4 Jul"));
        Assert.Equal("RELATED TRIES · 1", TakeProposalPolicy.Header(new[] { Group("a"), Group("b", "skipped") }));
        var hits = TakeProposalPolicy.SaveHits(Group("k"));
        Assert.Equal(new long[] { 4, 6 }, hits.Select(h => h.FileID));
        Assert.All(hits, h => Assert.Equal("image", h.Kind));
        var before = new[] { Group("a"), Group("b"), Group("c") };
        Assert.Equal(1, TakeProposalPolicy.NextAfter(before, new[] { Group("a"), Group("c") }, "b"));
        Assert.Equal(0, TakeProposalPolicy.NextAfter(before, new[] { Group("a"), Group("b", "skipped") }, "c"));
        Assert.Null(TakeProposalPolicy.NextAfter(before, new[] { Group("b") }, "b"));
    }
}
