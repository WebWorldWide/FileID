// Tests for the bulk-tag "Replace existing" undo grouping (F-C5-004). A replace
// wipes user tags, so BulkTagSheet snapshots each file's prior user tags and
// journals an undo that restores them via one applyTags(replace) per distinct
// tag-set. GroupByTagSet is the pure batching behind that undo — exercised here
// headlessly (a static method on a XAML control type, same shape as the
// TagChip.FormatTag tests; the rest of the flow is UI-runtime / CI-build only).

using System.Collections.Generic;
using System.Linq;
using FileID.Views.Library;
using Xunit;

namespace FileID.App.Tests;

public class BulkTagReplaceUndoGroupingTests
{
    private static readonly long[] s_ids123 = [1, 2, 3];
    private static readonly long[] s_ids12 = [1, 2];
    private static readonly string[] s_tagsAB = ["a", "b"];
    private static readonly string[] s_tagsBA = ["b", "a"];
    private static readonly string[] s_tagA = ["a"];
    private static readonly string[] s_tagB = ["b"];
    private static readonly string[] s_tagsAbC = ["ab", "c"];
    private static readonly string[] s_tagsABc = ["a", "bc"];

    private static Dictionary<long, List<string>> Prior(params (long Id, string[] Tags)[] entries)
    {
        var map = new Dictionary<long, List<string>>();
        foreach (var (id, tags) in entries) map[id] = tags.ToList();
        return map;
    }

    [Fact]
    public void GroupByTagSet_FilesWithSameSet_ShareOneBatch()
    {
        var prior = Prior((1, s_tagsAB), (2, s_tagsBA), (3, s_tagsAB));
        var groups = BulkTagSheet.GroupByTagSet(s_ids123, prior);
        Assert.Single(groups);
        Assert.Equal(s_ids123, groups[0].Ids.OrderBy(x => x).ToArray());
        Assert.Equal(s_tagsAB, groups[0].Tags.OrderBy(x => x).ToArray());
    }

    [Fact]
    public void GroupByTagSet_DistinctSets_ProduceSeparateBatches()
    {
        var prior = Prior((1, s_tagA), (2, s_tagB));
        var groups = BulkTagSheet.GroupByTagSet(s_ids12, prior);
        Assert.Equal(2, groups.Count);
    }

    [Fact]
    public void GroupByTagSet_MissingFile_RestoresToEmptySet()
    {
        // A file with no prior user tags is absent from the snapshot; its undo
        // must replace-with-empty (clearing the tags the apply just added).
        var prior = Prior((1, s_tagA)); // id 2 has no prior user tags
        var groups = BulkTagSheet.GroupByTagSet(s_ids12, prior);
        var emptyBatch = groups.Single(g => g.Ids.Contains(2));
        Assert.Empty(emptyBatch.Tags);
    }

    [Fact]
    public void GroupByTagSet_DistinctSetsThatConcatAlike_DoNotCollide()
    {
        // The key delimiter must keep ["ab","c"] distinct from ["a","bc"].
        var prior = Prior((1, s_tagsAbC), (2, s_tagsABc));
        var groups = BulkTagSheet.GroupByTagSet(s_ids12, prior);
        Assert.Equal(2, groups.Count);
    }

    [Fact]
    public void GroupByTagSet_EmptySelection_ReturnsNoGroups()
    {
        var groups = BulkTagSheet.GroupByTagSet(
            System.Array.Empty<long>(), new Dictionary<long, List<string>>());
        Assert.Empty(groups);
    }
}
