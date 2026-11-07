using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class ChatSelectionPolicyTests
{
    private static CatalogHit Hit(long id, string verdict, string path = "") =>
        new(id, path.Length == 0 ? $"C:\\f\\{id}.jpg" : path, "selection", "reason", null, null, null, verdict);

    [Fact]
    public void ScopeIsDeduplicatedCappedAndLabelledByKind()
    {
        var scope = ChatSelectionPolicy.Selected(new long[] { 3, 3, 0, 4 }, new[] { "image", "image" })!;
        Assert.Equal(new long[] { 3, 4 }, scope.FileIDs);
        Assert.Equal("2 selected photos", scope.Label);
        Assert.Equal("1 selected video", ChatSelectionPolicy.Selected(new long[] { 5 }, new[] { "video" })!.Label);
        Assert.Equal("2 selected files", ChatSelectionPolicy.Selected(new long[] { 5, 6 }, new[] { "video", "image" })!.Label);
        Assert.Null(ChatSelectionPolicy.Selected(Array.Empty<long>(), Array.Empty<string>()));
        Assert.Equal(ChatSelectionPolicy.MaxFiles, ChatSelectionPolicy.Selected(Enumerable.Range(1, 2500).Select(i => (long)i), Array.Empty<string>())!.FileIDs.Count);
        Assert.Equal("video", ChatSelectionPolicy.KindOf("C:\\clips\\a.MOV"));
        Assert.Equal("image", ChatSelectionPolicy.KindOf("C:\\a.heic"));
    }

    [Fact]
    public void OnlyMatchesNarrowTheNextQuestion()
    {
        var narrowed = ChatSelectionPolicy.Narrowed(new[] { Hit(1, "unsure"), Hit(2, "yes") })!;
        Assert.Equal(new long[] { 2 }, narrowed.FileIDs);
        Assert.Equal("1 match from the last answer", narrowed.Label);
        Assert.Null(ChatSelectionPolicy.Narrowed(new[] { Hit(1, "unsure") }));
    }

    [Fact]
    public void StripTargetsChosenTilesThenMatchesThenEverything()
    {
        var hits = new[] { Hit(1, "yes"), Hit(2, "unsure"), Hit(3, "yes") };
        Assert.Equal(new long[] { 1, 3 }, ChatSelectionPolicy.Target(hits, new HashSet<long>()));
        Assert.Equal(new long[] { 2, 3 }, ChatSelectionPolicy.Target(hits, new HashSet<long> { 3, 2 }));
        Assert.Equal(new long[] { 4 }, ChatSelectionPolicy.Target(new[] { Hit(4, "unsure") }, new HashSet<long>()));
        Assert.Equal("Ask about the 2 matches", ChatSelectionPolicy.AskLabel(hits, 0));
        Assert.Equal("Ask about 1 selected", ChatSelectionPolicy.AskLabel(hits, 1));
        Assert.True(ChatSelectionPolicy.HasResults(hits));
        Assert.False(ChatSelectionPolicy.HasResults(new[] { Hit(5, "yes") with { Match = null } }));
        Assert.Equal("Match: 1.jpg. reason", ChatSelectionPolicy.MatchSpoken(Hit(1, "yes", "C:\\f\\1.jpg")));
    }

    [Fact]
    public void BestTakesAttachesKeptTriesOnlyWhenSomeAreKept()
    {
        var primaries = new long[] { 10, 11, 12 };
        Assert.Equal((new long[] { 10, 11, 12 }, "Ask about all 3"), Unpack(ChatSelectionPolicy.TakeTargets(primaries, new HashSet<long>())));
        Assert.Equal((new long[] { 11 }, "Ask about 1 kept"), Unpack(ChatSelectionPolicy.TakeTargets(primaries, new HashSet<long> { 11 })));
        Assert.Equal((new long[] { 10, 11, 12 }, "Ask about all 3"), Unpack(ChatSelectionPolicy.TakeTargets(primaries, new HashSet<long> { 10, 11, 12 })));
    }

    [Fact]
    public void ScopedRequestsSerializeTheSelectionAndMatchRoundTrips()
    {
        var request = new ChatRequest("r", "c", "send", "the one where he catches it", UseModel: true, ScopeFileIDs: new long[] { 4, 5 });
        var json = System.Text.Json.JsonSerializer.Serialize(request, IpcCoder.Options);
        Assert.Contains("\"scopeFileIDs\":[4,5]", json, StringComparison.Ordinal);
        var hit = System.Text.Json.JsonSerializer.Deserialize<CatalogHit>("{\"fileID\":4,\"path\":\"p\",\"kind\":\"selection\",\"text\":\"Both hands on the ball.\",\"match\":\"yes\"}", IpcCoder.Options)!;
        Assert.Equal("yes", hit.Match);
        Assert.DoesNotContain("match", System.Text.Json.JsonSerializer.Serialize(hit with { Match = null }, IpcCoder.Options), StringComparison.Ordinal);
    }

    private static (long[], string) Unpack((IReadOnlyList<long> Ids, string Label) value) => (value.Ids.ToArray(), value.Label);
}
