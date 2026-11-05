using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class TakeGroupsPolicyTests
{
    private static readonly long[] OneAndTwo = { 1, 2 };
    private static readonly long[] OneTwoThree = { 1, 2, 3 };
    private static readonly long[] TwoAndOne = { 2, 1 };

    private static CatalogHit Hit(long id, string path) => new(id, path, "passage", "", null, null, null);

    private static CatalogTake Take(long id, double? score, string? version, string? path = null) =>
        new("game", id, path ?? $"C:\\clips\\try{id}.mp4", score, 0.42, null, null, null, version, false, false);

    private static CatalogEvent Event(string goal, params long[] ids) => new("game", "Baseball", goal, ids, true);

    private static CatalogJob Job(string id, string state, params long[] ids) => new(id, "timelineSample", ids, state, 0, null, 0, 0);

    [Fact]
    public void ThePoolKeepsCandidatesFirstAndSearchesAddOnlyNewVideos()
    {
        var candidates = new[] { Hit(1, "C:\\a.mp4"), Hit(1, "C:\\a.mp4"), Hit(2, "C:\\b.jpg") };
        var found = TakeGroupsPolicy.FoundVideos(new[] { Hit(2, "C:\\b.mp4"), Hit(3, "C:\\c.MOV"), Hit(4, "C:\\d.pdf"), Hit(3, "C:\\c.MOV") }, candidates);
        Assert.Equal("3", string.Join(",", found.Select(hit => hit.FileID)));
        var pool = TakeGroupsPolicy.Pool(candidates, found);
        Assert.Equal("1,2,3", string.Join(",", pool.Select(hit => hit.FileID)));
        Assert.True(TakeGroupsPolicy.CanOpenFromSearch(candidates));
        Assert.False(TakeGroupsPolicy.CanOpenFromSearch(candidates.Take(2)));
        Assert.Equal("Find at least two clips first", TakeGroupsPolicy.SearchTooltip(candidates.Take(1)));
        Assert.Equal("Search for at least two videos first", TakeGroupsPolicy.FindRelatedTooltip(1));
    }

    [Fact]
    public void VideoPathsFollowTheEngineExtensionList()
    {
        Assert.True(TakeGroupsPolicy.IsVideoPath("C:\\clips\\swing.MTS"));
        Assert.True(TakeGroupsPolicy.IsVideoPath("/clips/swing.webm"));
        Assert.False(TakeGroupsPolicy.IsVideoPath("C:\\clips.mp4\\still.jpg"));
        Assert.False(TakeGroupsPolicy.IsVideoPath("C:\\clips\\mp4"));
        Assert.False(TakeGroupsPolicy.IsVideoPath("C:\\clips\\swing."));
    }

    [Fact]
    public void SavingNeedsANameAGoalAndTwoClipsAndKeepsTheOpenID()
    {
        Assert.True(TakeGroupsPolicy.CanSave("Baseball", "Alex gets a hit", 2));
        Assert.False(TakeGroupsPolicy.CanSave("  ", "Alex gets a hit", 2));
        Assert.False(TakeGroupsPolicy.CanSave("Baseball", "", 3));
        Assert.False(TakeGroupsPolicy.CanSave("Baseball", "goal", 1));
        Assert.Equal("Save (Ctrl+S)", TakeGroupsPolicy.SaveTooltip(true));
        Assert.Equal("Needs a name, a goal and at least two clips", TakeGroupsPolicy.SaveTooltip(false));
        var fresh = TakeGroupsPolicy.EventToSave(null, "new-id", "T", "G", new long[] { 5, 2, 5 });
        Assert.Equal("new-id", fresh.Id);
        Assert.Equal("2,5", string.Join(",", fresh.FileIDs));
        Assert.Equal("game", TakeGroupsPolicy.EventToSave(Event("G", 2), "x", "T", "G", OneAndTwo).Id);
    }

    [Fact]
    public void ChooseAllFlipsBetweenEverythingAndNothing()
    {
        var pool = new[] { Hit(1, "C:\\a.mp4"), Hit(2, "C:\\b.mp4") };
        var some = new HashSet<long> { 1 };
        Assert.Equal("Choose all", TakeGroupsPolicy.ChooseLabel(some, pool));
        var all = TakeGroupsPolicy.ToggleAll(some, pool);
        Assert.Equal(2, all.Count);
        Assert.Equal("Choose none", TakeGroupsPolicy.ChooseLabel(all, pool));
        Assert.Empty(TakeGroupsPolicy.ToggleAll(all, pool));
        Assert.Equal("CLIPS · 1 OF 2 CHOSEN", TakeGroupsPolicy.ClipsHeader(1, 2));
        Assert.Equal("Choose all", TakeGroupsPolicy.ChooseLabel(new HashSet<long>(), Array.Empty<CatalogHit>()));
        Assert.Equal("Search on the left to add clips.", TakeGroupsPolicy.EditorNote(true));
    }

    [Fact]
    public void ScoringWaitsForASavedUnchangedGroupOfVideos()
    {
        var group = Event("Alex gets a hit", 1, 2);
        var chosen = new HashSet<long>(OneAndTwo);
        var takes = new[] { Take(1, null, null), Take(2, null, null) };
        Assert.True(TakeGroupsPolicy.CanScore(group, "Alex gets a hit ", chosen, takes, false, null));
        Assert.False(TakeGroupsPolicy.CanScore(group, "Someone else", chosen, takes, false, null));
        Assert.False(TakeGroupsPolicy.CanScore(group, "Alex gets a hit", new HashSet<long> { 1 }, takes, false, null));
        Assert.False(TakeGroupsPolicy.CanScore(group, "Alex gets a hit", chosen, takes[..1], false, null));
        Assert.False(TakeGroupsPolicy.CanScore(group, "Alex gets a hit", chosen, takes, true, null));
        Assert.False(TakeGroupsPolicy.CanScore(group, "Alex gets a hit", chosen, takes, false, Job("j", "running", 1, 2)));
        Assert.True(TakeGroupsPolicy.CanScore(group, "Alex gets a hit", chosen, takes, false, Job("j", "completed", 1, 2)));
        var photo = new[] { Take(1, null, null, "C:\\clips\\still.jpg"), takes[1] };
        Assert.False(TakeGroupsPolicy.CanScore(group, "Alex gets a hit", chosen, photo, false, null));
    }

    [Fact]
    public void TheGoalJobIsTheNewSamplingJobOverExactlyTheGroup()
    {
        var jobs = new[] { Job("old", "running", OneAndTwo), Job("other", "queued", OneTwoThree), Job("new", "queued", TwoAndOne) };
        var previous = new HashSet<string> { "old" };
        var requested = new HashSet<long>(OneAndTwo);
        Assert.Equal("new", TakeGroupsPolicy.NewGoalJob(jobs, requested, previous)?.Id);
        Assert.Null(TakeGroupsPolicy.NewGoalJob(jobs[..2], requested, previous));
        Assert.Equal("2 videos · finding moments", TakeGroupsPolicy.JobTitle(jobs[2]));
        Assert.Equal(("Queued", TakeGroupsPolicy.Tone.Muted), TakeGroupsPolicy.JobState(jobs[2]));
        Assert.True(TakeGroupsPolicy.JobActive(jobs[2]));
        Assert.False(TakeGroupsPolicy.JobActive(null));
    }

    [Fact]
    public void CardHeadlinesFollowPreferenceStalenessAndWhoScored()
    {
        var draft = Take(1, 0.8, "take-outcome-sequence-v1/abc");
        Assert.Equal(("Draft · Goal met", TakeGroupsPolicy.Tone.Done), TakeGroupsPolicy.Headline(draft));
        Assert.Equal("Model draft", TakeGroupsPolicy.ReviewDot(draft).Spoken);
        var missed = draft with { ModelVersion = "user", OutcomeScore = 0.0 };
        Assert.Equal(("Goal missed", TakeGroupsPolicy.Tone.Danger), TakeGroupsPolicy.Headline(missed));
        Assert.Equal(TakeGroupsPolicy.Outcome.Missed, TakeGroupsPolicy.SelectedOutcome(missed));
        var unclear = missed with { OutcomeScore = null };
        Assert.Equal("Unclear", TakeGroupsPolicy.Headline(unclear).Text);
        Assert.Equal(TakeGroupsPolicy.Outcome.Unclear, TakeGroupsPolicy.SelectedOutcome(unclear));
        Assert.Equal(TakeGroupsPolicy.Tone.Warning, TakeGroupsPolicy.Headline(unclear with { Stale = true }).Tone);
        Assert.Equal(("Preferred by you", TakeGroupsPolicy.Tone.Gold), TakeGroupsPolicy.Headline(unclear with { Stale = true, Preferred = true }));
        var unscored = Take(2, null, null);
        Assert.Equal("Not reviewed", TakeGroupsPolicy.Headline(unscored).Text);
        Assert.Null(TakeGroupsPolicy.SelectedOutcome(unscored));
        Assert.Equal("No score yet", TakeGroupsPolicy.ReviewDot(unscored).Tooltip);
    }

    [Fact]
    public void CardButtonsSendTheMatchingFeedback()
    {
        var take = Take(3, 0.7, "user");
        var met = TakeGroupsPolicy.OutcomeFeedback("game", take, TakeGroupsPolicy.Outcome.Met);
        Assert.Equal<double?>(1.0, met.OutcomeScore);
        Assert.False(met.Preferred);
        Assert.Equal(3L, met.FileID);
        Assert.Null(TakeGroupsPolicy.OutcomeFeedback("game", take, TakeGroupsPolicy.Outcome.Unclear).OutcomeScore);
        var prefer = TakeGroupsPolicy.PreferFeedback("game", take);
        Assert.Equal<double?>(0.7, prefer.OutcomeScore);
        Assert.True(prefer.Preferred);
        Assert.False(TakeGroupsPolicy.PreferFeedback("game", take with { Preferred = true }).Preferred);
        Assert.Equal("Goal met", TakeGroupsPolicy.Spoken(TakeGroupsPolicy.Outcome.Met));
        Assert.Equal("Take 2, try3.mp4, recommended", TakeGroupsPolicy.CardSpoken(2, take, true));
        Assert.Equal(43, TakeGroupsPolicy.Percent(0.426));
    }

    [Fact]
    public void RowsAndRecommendationsReadLikeTheMac()
    {
        var group = Event("Alex gets a hit", 1, 2, 3);
        Assert.Equal("3 clips · Alex gets a hit", TakeGroupsPolicy.EventMeta(group));
        Assert.Equal("Baseball, 3 clips, goal: Alex gets a hit", TakeGroupsPolicy.EventSpoken(group));
        var suggestion = new CatalogTakeGroupSuggestion(
            new[] { new CatalogTakeGroupMember(1, "C:\\c\\a.mp4"), new CatalogTakeGroupMember(2, "C:\\c\\b.mp4") },
            0.874,
            "Filmed close together");
        Assert.Equal("2 clips · 87% alike", TakeGroupsPolicy.SuggestionMeta(suggestion));
        Assert.Equal("a.mp4, b.mp4", string.Join(", ", TakeGroupsPolicy.SuggestionNames(suggestion)));
        Assert.Equal(2L, TakeGroupsPolicy.SuggestionHits(suggestion)[1].FileID);
        var recommendation = new CatalogTakeRecommendation("game", "tie", new long[] { 2 }, "Two takes meet the goal");
        Assert.Equal(TakeGroupsPolicy.Tone.Ai, TakeGroupsPolicy.RecommendationTone(recommendation));
        Assert.Equal("RECOMMENDATION · TIE", TakeGroupsPolicy.RecommendationHeader(recommendation));
        Assert.True(TakeGroupsPolicy.IsRecommended(recommendation, Take(2, null, null)));
        Assert.False(TakeGroupsPolicy.IsRecommended(null, Take(2, null, null)));
        Assert.True(TakeGroupsPolicy.RepliesWithOpenGroup("setTakeFeedback"));
        Assert.False(TakeGroupsPolicy.RepliesWithOpenGroup("listEvents"));
        Assert.True(TakeGroupsPolicy.Relists("deleteEvent"));
        Assert.False(TakeGroupsPolicy.Relists("takeGroup"));
    }

    [Fact]
    public void OpenGroupClipsJoinThePoolOnce()
    {
        var pool = new[] { Hit(1, "C:\\a.mp4") };
        var missing = TakeGroupsPolicy.MissingTakes(new[] { Take(1, null, null), Take(4, null, null), Take(4, null, null) }, pool);
        Assert.Equal("4", string.Join(",", missing.Select(hit => hit.FileID)));
        Assert.Equal("C:\\clips\\try4.mp4", missing[0].Path);
    }

    [Fact]
    public void CardGridFitsColumnsOfTwoHundredSixtyToFourHundredTwenty()
    {
        Assert.Equal((1, 250d), TakeGroupsPolicy.CardColumns(250));
        Assert.Equal((2, 266d), TakeGroupsPolicy.CardColumns(544));
        Assert.Equal((5, 260d), TakeGroupsPolicy.CardColumns(1348));
        Assert.Equal((1, 420d), TakeGroupsPolicy.CardColumns(500));
        Assert.Equal((1, 420d), TakeGroupsPolicy.CardColumns(double.PositiveInfinity));
    }
}
