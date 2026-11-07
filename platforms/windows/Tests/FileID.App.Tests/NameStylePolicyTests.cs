using System.Text.Json;
using FileID.IpcSchema;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public sealed class NameStylePolicyTests
{
    private static CatalogHit Hit(long id, string name, string text, string reason) =>
        new(id, "C:\\photos\\" + name, "proposedName", text, reason, null, null);

    [Theory]
    [InlineData("chapter", "From your chapter")]
    [InlineData("proposedName", "From the AI name")]
    [InlineData("description", "From the description")]
    [InlineData("tags", "From tags")]
    [InlineData("event", "From your event")]
    [InlineData("unchanged", "Already well named")]
    [InlineData("budget", "Too long for the length limit")]
    [InlineData("reserved", "Not allowed as a file name")]
    [InlineData("collision", "No free name in this folder")]
    [InlineData("noEvidence", "Nothing to name it from yet")]
    public void ReasonLabels(string reason, string label) => Assert.Equal(label, NameStylePolicy.ReasonLabel(reason));

    [Fact]
    public void EveryCorpusReasonHasALabel()
    {
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null && !File.Exists(Path.Combine(dir.FullName, "shared", "test-corpus", "naming", "style-cases.json"))) dir = dir.Parent;
        if (dir is null) return;
        using var doc = JsonDocument.Parse(File.ReadAllText(Path.Combine(dir.FullName, "shared", "test-corpus", "naming", "style-cases.json")));
        foreach (var item in doc.RootElement.EnumerateArray())
        {
            var reason = item.GetProperty("reason").GetString();
            Assert.True(reason == "noEvidence" || NameStylePolicy.ReasonLabel(reason) != NameStylePolicy.ReasonLabel("noEvidence"), reason);
        }
    }

    [Fact]
    public void DefaultsAndRequest()
    {
        Assert.Equal("short", NameStylePolicy.DefaultStyle);
        Assert.Equal(40, NameStylePolicy.DefaultMaxLength);
        Assert.True(NameStylePolicy.DefaultIncludePeople && NameStylePolicy.DefaultIncludeEvent && !NameStylePolicy.DefaultIncludeDate);
        var naming = NameStylePolicy.Naming("descriptive", 500, true, false, true);
        Assert.Equal(new CatalogNaming("descriptive", 120, true, false, true), naming);
        Assert.Equal(new CatalogNaming("short", 4), NameStylePolicy.Naming("loud", 1, false, false, false));
        var request = NameStylePolicy.Request("r1", new long[] { 3, 4 }, naming);
        Assert.Equal("proposeNames", request.Action);
        Assert.Equal(new long[] { 3, 4 }, request.FileIDs);
        Assert.Equal(naming, request.Naming);
        Assert.Equal(TimeSpan.FromMilliseconds(300), NameStylePolicy.PreviewDelay);
    }

    [Fact]
    public void SanitizeAndClamp()
    {
        Assert.Equal("dateFirst", NameStylePolicy.SanitizeStyle("dateFirst"));
        Assert.Equal("short", NameStylePolicy.SanitizeStyle("DATEFIRST"));
        Assert.Equal("short", NameStylePolicy.SanitizeStyle(null));
        Assert.Equal(4, NameStylePolicy.ClampLength(-3));
        Assert.Equal(120, NameStylePolicy.ClampLength(999));
        Assert.Equal(40, NameStylePolicy.ClampLength(double.NaN));
        Assert.Equal(57, NameStylePolicy.ClampLength(56.6));
    }

    [Fact]
    public void DateFirstDisablesAddDate()
    {
        Assert.False(NameStylePolicy.DateToggleEnabled("dateFirst"));
        Assert.True(NameStylePolicy.DateToggleEnabled("short"));
        Assert.True(NameStylePolicy.DateToggleEnabled("descriptive"));
        Assert.False(NameStylePolicy.Naming("dateFirst", 40, true, true, true).IncludeDate);
    }

    [Fact]
    public void FileIdsPreferTheSelectionAndCapAtOneThousand()
    {
        Assert.Equal(new long[] { 5, 6 }, NameStylePolicy.FileIds(new long[] { 5, 5, 0, 6 }, new long[] { 9 }));
        Assert.Equal(new long[] { 9, 8 }, NameStylePolicy.FileIds(Array.Empty<long>(), new long[] { 9, 8 }));
        Assert.Equal(1000, NameStylePolicy.FileIds(Enumerable.Range(1, 1500).Select(i => (long)i).ToArray(), Array.Empty<long>()).Count);
    }

    [Fact]
    public void OnlyRowsWithTextCanBeCheckedAndUntickedRowsStayUnticked()
    {
        var hits = new[]
        {
            Hit(1, "IMG_0001.jpg", "sam-baseball-hit.jpg", "proposedName"),
            Hit(2, "Beach.jpg", "", "unchanged"),
            new CatalogHit(3, "C:\\x", "keyword", "nope", null, null, null),
        };
        var rows = NameStylePolicy.Rows(hits, Array.Empty<NameProposalRow>());
        Assert.Equal(new long[] { 1, 2 }, rows.Select(r => r.FileId));
        Assert.True(rows[0].CanCheck && rows[0].Checked);
        Assert.False(rows[1].CanCheck || rows[1].Checked);
        rows[0].Checked = false;
        Assert.False(NameStylePolicy.Rows(hits, rows)[0].Checked);
    }

    [Fact]
    public void RenamesSendTheEditedTextForCheckedRowsOnly()
    {
        var rows = NameStylePolicy.Rows(new[]
        {
            Hit(1, "IMG_0001.jpg", "sam-baseball-hit.jpg", "proposedName"),
            Hit(2, "IMG_0002.jpg", "beach.jpg", "tags"),
            Hit(3, "IMG_0003.jpg", "", "budget"),
            Hit(4, "IMG_0004.jpg", "cake.jpg", "description"),
            Hit(5, "IMG_0005.jpg", "dog.jpg", "tags"),
        }, Array.Empty<NameProposalRow>());
        rows[0].Edited = "  sam-home-run.jpg ";
        rows[1].Checked = false;
        rows[2].Checked = true;
        rows[3].Edited = "bad\\name.jpg";
        rows[4].Edited = "IMG_0005.jpg";
        var renames = NameStylePolicy.Renames(rows);
        Assert.Equal(new[] { new RenameEntry(1, "sam-home-run.jpg") }, renames);
        Assert.Equal(new[] { new RenameEntry(1, "IMG_0001.jpg") }, NameStylePolicy.UndoEntries(rows, renames, new long[] { 1, 99 }));
        Assert.Empty(NameStylePolicy.UndoEntries(rows, renames, Array.Empty<long>()));
    }

    [Fact]
    public void AccessibleNameReadsNameTargetAndReason()
    {
        var rows = NameStylePolicy.Rows(new[]
        {
            Hit(1, "IMG_0001.jpg", "sam-baseball-hit.jpg", "proposedName"),
            Hit(2, "Beach.jpg", "", "unchanged"),
        }, Array.Empty<NameProposalRow>());
        Assert.Equal("IMG_0001.jpg, rename to sam-baseball-hit.jpg, from the AI name", NameStylePolicy.AccessibleName(rows[0]));
        Assert.Equal("Beach.jpg, keeps its name, already well named", NameStylePolicy.AccessibleName(rows[1]));
    }

    private static readonly long[] PairOrder = { 11, 10, 12, 14 };
    private static readonly string[] PairSuffixes = { ".JPG", ".ARW", ".ARW.xmp", "" };
    private static readonly bool[] GroupUnticked = { false, false, false, true };
    private static readonly string[] EditedStems = { " sunset-walk.JPG ", "sunset-walk.ARW", "sunset-walk.ARW.xmp", "lone-hill.ARW" };

    private static CatalogHit[] Pair() => new[]
    {
        Hit(11, "DSC_1.JPG", "beach-sunset_2.JPG", "proposedName|group:11"),
        Hit(10, "DSC_1.ARW", "beach-sunset_2.ARW", "proposedName|group:11"),
        Hit(12, "DSC_1.ARW.xmp", "beach-sunset_2.ARW.xmp", "proposedName|group:11"),
        Hit(14, "DSC_3.ARW", "lone-hill.ARW", "proposedName"),
    };

    [Fact]
    public void CompanionsArriveTogetherLeaderFirst()
    {
        var rows = NameStylePolicy.Rows(Pair(), Array.Empty<NameProposalRow>());
        Assert.Equal(PairOrder, rows.Select(r => r.FileId));
        Assert.All(rows, r => Assert.Equal("proposedName", r.Reason));
        Assert.True(rows[0].IsLeader && rows[1].IsFollower && rows[2].IsFollower && rows[3].Group is null);
        Assert.Equal(PairSuffixes, rows.Select(r => r.Suffix));
        Assert.Equal("+ ARW, XMP", NameStylePolicy.CompanionChip(rows[0], rows));
        Assert.Null(NameStylePolicy.CompanionChip(rows[1], rows));
        Assert.Equal("Its partner file isn't scanned yet", NameStylePolicy.ReasonLabel("unscannedCompanion"));
        Assert.Equal("DSC_1.ARW, rename to beach-sunset_2.ARW, from the AI name, together with DSC_1.JPG, DSC_1.ARW.xmp",
            NameStylePolicy.AccessibleName(rows[1], rows));
        Assert.Equal(("collision", (long?)30), NameStylePolicy.ParseReason("collision|group:30"));
        Assert.Equal(("tags", (long?)null), NameStylePolicy.ParseReason("tags"));
        Assert.Equal(("noEvidence", (long?)null), NameStylePolicy.ParseReason(null));
    }

    [Fact]
    public void TickingTheLeaderTicksTheWholeGroupAndStaysUnticked()
    {
        var rows = NameStylePolicy.Rows(Pair(), Array.Empty<NameProposalRow>());
        Assert.Equal(3, NameStylePolicy.SetChecked(rows, 11, false).Count);
        Assert.Equal(GroupUnticked, rows.Select(r => r.Checked));
        Assert.Equal(GroupUnticked, NameStylePolicy.Rows(Pair(), rows).Select(r => r.Checked));
        NameStylePolicy.SetChecked(rows, 11, true);
        Assert.All(rows, r => Assert.True(r.Checked));
    }

    [Fact]
    public void EditingTheLeaderGivesEveryCompanionTheSameStem()
    {
        var rows = NameStylePolicy.Rows(Pair(), Array.Empty<NameProposalRow>());
        Assert.Equal(2, NameStylePolicy.Edit(rows, 11, " sunset-walk.JPG ").Count);
        Assert.Equal(EditedStems, rows.Select(r => r.Edited));
        Assert.Empty(NameStylePolicy.Edit(rows, 10, "rogue.ARW"));
        Assert.Equal("sunset-walk.ARW", rows[1].Edited);
        Assert.Equal(new[]
        {
            new RenameEntry(11, "sunset-walk.JPG"), new RenameEntry(10, "sunset-walk.ARW"),
            new RenameEntry(12, "sunset-walk.ARW.xmp"), new RenameEntry(14, "lone-hill.ARW"),
        }, NameStylePolicy.Renames(rows));
        NameStylePolicy.Edit(rows, 11, "bad/name.JPG");
        Assert.Equal(new[] { new RenameEntry(14, "lone-hill.ARW") }, NameStylePolicy.Renames(rows));
    }

    [Fact]
    public void LearnedReasonsChipsAndEdits()
    {
        var hits = new[]
        {
            Hit(1, "IMG_1.jpg", "Kids_Soccer.jpg", "proposedName|learned:separator,case"),
            Hit(3, "DSC_1.JPG", "Red_Barn.JPG", "proposedName|learned:case|group:3"),
            Hit(2, "DSC_1.ARW", "Red_Barn.ARW", "proposedName|learned:case|group:3"),
            Hit(4, "IMG_4.jpg", "beach.jpg", "tags"),
        };
        var rows = NameStylePolicy.Rows(hits, Array.Empty<NameProposalRow>());
        Assert.Equal(new[] { "proposedName", "proposedName", "proposedName", "tags" }, rows.Select(r => r.Reason));
        Assert.Equal(new[] { "separator", "case" }, rows[0].Learned);
        Assert.Equal((3L, ".ARW"), (rows[2].Group!.Value, rows[2].Suffix));
        Assert.Equal("From the AI name · your style", NameStylePolicy.ReasonChip(rows[0]));
        Assert.Equal("From tags", NameStylePolicy.ReasonChip(rows[3]));
        Assert.Equal("Learned from your renames: uses your word separator, uses your capitals", NameStylePolicy.LearnedHelp(rows[0].Learned));
        Assert.Null(NameStylePolicy.LearnedHelp(Array.Empty<string>()));
        NameStylePolicy.Edit(rows, 1, "Kids Soccer Final.jpg");
        var edits = NameStylePolicy.Edits(rows, new long[] { 1, 2, 3 });
        Assert.Equal(new[] { new CatalogNameEdit(1, "Kids_Soccer.jpg", "Kids Soccer Final.jpg"), new CatalogNameEdit(3, "Red_Barn.JPG", "Red_Barn.JPG") }, edits);
        var naming = NameStylePolicy.Naming("short", 40, false, false, false, learn: true);
        Assert.True(naming.Learn);
        var request = NameStylePolicy.LearnRequest("l", edits, naming);
        Assert.Equal("learnNames", request.Action);
        var wire = JsonSerializer.Serialize(request, new JsonSerializerOptions { PropertyNamingPolicy = JsonNamingPolicy.CamelCase });
        Assert.Contains("\"learn\":true", wire);
        Assert.Contains("\"nameEdits\":[{\"fileID\":1", wire);
        Assert.DoesNotContain("\"learn\"", JsonSerializer.Serialize(NameStylePolicy.Naming("short", 40, false, false, false), new JsonSerializerOptions { PropertyNamingPolicy = JsonNamingPolicy.CamelCase }));
        Assert.Equal("resetNamePreferences", NameStylePolicy.PreferencesRequest("p", true).Action);
    }

    [Fact]
    public void LearnedSummaryReadsPreferences()
    {
        static CatalogHit Pref(string key, string text) => new(0, "", "namePreference", text, key, null, null);
        var count = new CatalogHit(0, "", "nameSamples", "12", null, null, null);
        var hits = new[] { Pref("separator", "_"), Pref("leadingDate", "end"), Pref("dateFormat", "compact"), Pref("words", "2"), Pref("dropWords", "outdoor"), count };
        var (text, samples) = NameStylePolicy.LearnedSummary(hits, true);
        Assert.Equal(12, samples);
        Assert.Equal("From 12 renames: underscores between words, the date at the end, dates like 20230704, at most 2 words, without \u201Coutdoor\u201D.", text);
        Assert.Equal("No clear preference yet from 2 renames.", NameStylePolicy.LearnedSummary(new[] { count with { Text = "2" } }, true).Text);
        Assert.Equal("Names follow the style above only.", NameStylePolicy.LearnedSummary(hits, false).Text);
        Assert.Equal(("Edit names before you rename them and FileID will follow your style.", 0L), NameStylePolicy.LearnedSummary(Array.Empty<CatalogHit>(), true));
    }
}

