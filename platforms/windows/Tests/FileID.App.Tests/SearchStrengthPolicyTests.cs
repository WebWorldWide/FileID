using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class SearchStrengthPolicyTests
{
    [Theory]
    [InlineData("1 good match.")]
    [InlineData("3 good matches.")]
    [InlineData("1 good match, 2 weaker.")]
    [InlineData("4 good matches, 1 weaker.")]
    [InlineData("No good matches, 5 weaker.")]
    [InlineData("No matches.")]
    public void ShowsTheHelpersSentence(string message)
    {
        Assert.Equal(message, SearchStrengthPolicy.Label("sunset", message));
    }

    [Fact]
    public void TrimsTheMessage()
    {
        Assert.Equal("3 good matches.", SearchStrengthPolicy.Label("  dog  ", " 3 good matches.\n"));
    }

    [Theory]
    [InlineData("ab")]
    [InlineData("  a ")]
    [InlineData("")]
    [InlineData(null)]
    public void ShortSearchesKeepTheCount(string? query)
    {
        Assert.Null(SearchStrengthPolicy.Label(query, "3 good matches."));
    }

    [Fact]
    public void ThreeCharactersIsEnough()
    {
        Assert.NotNull(SearchStrengthPolicy.Label(" dog ", "3 good matches."));
    }

    [Theory]
    [InlineData(null)]
    [InlineData("")]
    [InlineData("   ")]
    [InlineData("Visual similarity results.")]
    [InlineData("Matches by name, text and what's in the picture.")]
    [InlineData("Keyword results only. Preparing the local visual search index.")]
    [InlineData("3 good matches")]
    [InlineData("3 good matches, 1 weaker. Extra")]
    [InlineData("-1 good matches.")]
    public void NoMessageOrAnotherNoteKeepsTheCount(string? message)
    {
        Assert.Null(SearchStrengthPolicy.Label("sunset", message));
    }

    [Fact]
    public void SummaryReplacesTheCountOnlyWhenPresent()
    {
        Assert.Equal("12 results", SearchStrengthPolicy.CountText("12 results", null));
        Assert.Equal("3 good matches.", SearchStrengthPolicy.CountText("12 results", "3 good matches."));
    }

    [Fact]
    public void ScreenReaderNameIncludesTheSummary()
    {
        Assert.Equal("Search results. 3 good matches, 1 weaker.", SearchStrengthPolicy.AccessibleName("3 good matches, 1 weaker.", "3 good matches, 1 weaker."));
        Assert.Equal("12 results", SearchStrengthPolicy.AccessibleName("12 results", null));
        Assert.Equal(string.Empty, SearchStrengthPolicy.AccessibleName(string.Empty, null));
    }
}
