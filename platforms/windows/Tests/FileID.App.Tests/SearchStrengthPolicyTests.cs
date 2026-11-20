using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class SearchStrengthPolicyTests
{
    [Theory]
    [InlineData("1 match (1 strong visual).")]
    [InlineData("3 matches (3 strong visual).")]
    [InlineData("1 match (1 strong visual), 2 weaker.")]
    [InlineData("4 matches (4 strong visual), 1 weaker.")]
    [InlineData("No good matches, 5 weaker.")]
    [InlineData("No matches.")]
    public void ShowsTheHelpersSentence(string message)
    {
        Assert.Equal(message, SearchStrengthPolicy.Label("sunset", message));
    }

    [Fact]
    public void TrimsTheMessage()
    {
        Assert.Equal("3 matches (3 strong visual).", SearchStrengthPolicy.Label("  dog  ", " 3 matches (3 strong visual).\n"));
    }

    [Theory]
    [InlineData("ab")]
    [InlineData("  a ")]
    [InlineData("")]
    [InlineData(null)]
    public void ShortSearchesKeepTheCount(string? query)
    {
        Assert.Null(SearchStrengthPolicy.Label(query, "3 matches (3 strong visual)."));
    }

    [Fact]
    public void ThreeCharactersIsEnough()
    {
        Assert.NotNull(SearchStrengthPolicy.Label(" dog ", "3 matches (3 strong visual)."));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("Visual similarity results.")]
    [InlineData("Matches by name, text and what's in the picture.")]
    [InlineData("Keyword results only. Preparing the local visual search index.")]
    [InlineData("3 matches (3 strong visual)")]
    [InlineData("3 matches (3 strong visual), 1 weaker. Extra")]
    [InlineData("-1 matches (-1 strong visual).")]
    public void NoMessageOrAnotherNoteKeepsTheCount(string? message)
    {
        Assert.Null(SearchStrengthPolicy.Label("sunset", message));
    }

    [Fact]
    public void SummaryReplacesTheCountOnlyWhenPresent()
    {
        Assert.Equal("12 results", SearchStrengthPolicy.CountText("12 results", null));
        Assert.Equal("3 matches (3 strong visual).", SearchStrengthPolicy.CountText("12 results", "3 matches (3 strong visual)."));
    }

    [Fact]
    public void ScreenReaderNameIncludesTheSummary()
    {
        Assert.Equal("Search results. 3 matches (3 strong visual), 1 weaker.", SearchStrengthPolicy.AccessibleName("3 matches (3 strong visual), 1 weaker.", "3 matches (3 strong visual), 1 weaker."));
        Assert.Equal("12 results", SearchStrengthPolicy.AccessibleName("12 results", null));
        Assert.Equal(string.Empty, SearchStrengthPolicy.AccessibleName(string.Empty, null));
    }

    [Theory]
    [InlineData("3 matches (3 strong visual), 1 weaker.", 3, 1)]
    [InlineData("1 match (1 strong visual).", 1, 0)]
    [InlineData("No good matches, 60 weaker.", 0, 60)]
    [InlineData("No matches.", 0, 0)]
    public void CountsReadTheHelpersSentences(string message, int good, int weaker)
    {
        Assert.Equal((good, weaker), SearchStrengthPolicy.Counts(message));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("Matches by name, text and what's in the picture.")]
    [InlineData("Visual similarity results.")]
    [InlineData("3 matches (3 strong visual)")]
    public void CountsIgnoreAnyOtherNote(string? message)
    {
        Assert.Null(SearchStrengthPolicy.Counts(message));
    }

    [Fact]
    public void GoodMatchesShowTheHelpersListAndSummary()
    {
        var d = SearchStrengthPolicy.Decide("sunset", "4 matches (4 strong visual), 20 weaker.", 2, showWeaker: false);
        Assert.False(d.KeywordOnly);
        Assert.Equal("4 matches (4 strong visual), 20 weaker.", d.Label);
        Assert.Equal(0, d.HiddenWeaker);
    }

    [Fact]
    public void KeywordMatchesOnlyKeepWeakerBehindTheButton()
    {
        var d = SearchStrengthPolicy.Decide("invoice", "No good matches, 60 weaker.", 3, showWeaker: false);
        Assert.True(d.KeywordOnly);
        Assert.Equal("3 by name, tag or text", d.Label);
        Assert.Equal(60, d.HiddenWeaker);
    }

    [Fact]
    public void WeakerOnlyShowsNothingUntilAsked()
    {
        var d = SearchStrengthPolicy.Decide("zzzqqq", "No good matches, 60 weaker.", 0, showWeaker: false);
        Assert.True(d.KeywordOnly);
        Assert.Equal("No good matches", d.Label);
        Assert.Equal(60, d.HiddenWeaker);
        var shown = SearchStrengthPolicy.Decide("zzzqqq", "No good matches, 60 weaker.", 0, showWeaker: true);
        Assert.False(shown.KeywordOnly);
        Assert.Equal(0, shown.HiddenWeaker);
        Assert.Equal("No good matches, 60 weaker.", shown.Label);
    }

    [Fact]
    public void NothingAtAll()
    {
        var d = SearchStrengthPolicy.Decide("zzzqqq", "No matches.", 0, showWeaker: false);
        Assert.True(d.KeywordOnly);
        Assert.Equal("No matches", d.Label);
        Assert.Equal(0, d.HiddenWeaker);
    }

    [Theory]
    [InlineData("sunset", null)]
    [InlineData("sunset", "Matches by name, text and what's in the picture.")]
    [InlineData("zz", "No matches.")]
    public void NoSummaryOrAShortSearchKeepsTodaysBehaviour(string query, string? message)
    {
        var d = SearchStrengthPolicy.Decide(query, message, 0, showWeaker: false);
        Assert.False(d.KeywordOnly);
        Assert.Equal(SearchStrengthPolicy.Label(query, message), d.Label);
        Assert.Equal(0, d.HiddenWeaker);
    }

    [Fact]
    public void EmptyStateAndButtonUseTheMacsWords()
    {
        Assert.Equal("Show 60 weaker matches", SearchStrengthPolicy.WeakerButtonText(60));
        Assert.Equal(
            "No matches for \u201Czzzqqq\u201D. Nothing has that in its name, tags or text, and no picture matches it well. 60 weaker look-alikes were set aside.",
            SearchStrengthPolicy.NoMatchesText("zzzqqq", 60));
        Assert.Equal(
            "No matches for \u201Czzzqqq\u201D. Nothing has that in its name, tags or text, and no picture matches it. Try other words.",
            SearchStrengthPolicy.NoMatchesText("zzzqqq", 0));
    }

    [Fact]
    public void KeywordRowsFollowTheHelpersRowsWithoutRepeats()
    {
        var combined = SearchStrengthPolicy.AppendKeywordRows(new long[] { 5, 2 }, new long[] { 2, 9, 5, 7 }, id => id);
        Assert.Equal(new long[] { 5, 2, 9, 7 }, combined);
        Assert.Equal(new long[] { 3, 1 }, SearchStrengthPolicy.AppendKeywordRows(System.Array.Empty<long>(), new long[] { 3, 1 }, id => id));
        Assert.Equal(new long[] { 4 }, SearchStrengthPolicy.AppendKeywordRows(new long[] { 4 }, System.Array.Empty<long>(), id => id));
    }

    [Fact]
    public void KeywordRowsAreNotCapped()
    {
        var keyword = new long[300];
        for (var i = 0; i < keyword.Length; i++) keyword[i] = 100 + i;
        Assert.Equal(302, SearchStrengthPolicy.AppendKeywordRows(new long[] { 1, 2 }, keyword, id => id).Count);
    }

    [Theory]
    [InlineData("4 matches (4 strong visual), 20 weaker.", 2, false, true)]
    [InlineData("No good matches, 60 weaker.", 0, true, true)]
    [InlineData("Matches by name, text and what's in the picture.", 0, false, true)]
    [InlineData(null, 0, false, true)]
    [InlineData("No good matches, 60 weaker.", 3, false, false)]
    [InlineData("No matches.", 0, false, false)]
    public void OnlyTheHelpersListDecisionsAddKeywordRows(string? message, int keywordCount, bool showWeaker, bool expected)
    {
        Assert.Equal(expected, SearchStrengthPolicy.Decide("sunset", message, keywordCount, showWeaker).AddKeywordRows);
    }
}
