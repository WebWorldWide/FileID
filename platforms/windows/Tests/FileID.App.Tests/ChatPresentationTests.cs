using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ChatPresentationTests
{
    private static CatalogHit Hit(string kind, double? start = null) =>
        new(7, "C:\\Videos\\Birthday\\party.mov", kind, "", null, start, null);

    private static ChatMessage Message(string id, string role) => new(id, role, "text", 0);

    [Fact]
    public void ChatChipsSitUnderTheLatestAnswerAndRepliesExtendTheShownMessages()
    {
        var messages = new[] { Message("1", "user"), Message("2", "assistant"), Message("3", "user") };
        Assert.Equal("2", ChatPresentation.LastAnswerID(messages));
        Assert.Null(ChatPresentation.LastAnswerID(messages[..1]));
        Assert.Equal("You", ChatPresentation.RoleLabel(messages[0]));
        Assert.Equal("FileID", ChatPresentation.RoleLabel(messages[1]));
        var shown = new List<string> { "1", "2" };
        Assert.True(ChatPresentation.ExtendsShown(shown, messages));
        Assert.False(ChatPresentation.ExtendsShown(shown, messages[..1]));
        Assert.False(ChatPresentation.ExtendsShown(shown, new[] { Message("1", "user"), Message("9", "assistant") }));
        Assert.True(ChatPresentation.ExtendsShown(new List<string>(), messages));
        var hits = Enumerable.Range(0, 12).Select(i => Hit("chapter", i)).ToArray();
        Assert.Equal(ChatPresentation.MaxChips, ChatPresentation.ChipHits(hits).Count);
        Assert.Equal("None yet", ChatPresentation.SourcesCount(0));
        Assert.Equal("· 0:42", ChatPresentation.ChipMeta(Hit("chapter", 42)));
        Assert.Equal("", ChatPresentation.ChipMeta(Hit("ocrText")));
        Assert.Equal("3", ChatPresentation.SourcesCount(3));
    }
}
