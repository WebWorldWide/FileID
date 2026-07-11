// F11: when a Similar-mode load fails (e.g. LoadSimilar's >20k image cap
// exception), the previously loaded Exact groups must not stay rendered under
// the Similar header. RefreshAsync clears the list via MergeByContentHash with
// an empty row set — verify that path really empties a populated collection.

using System.Collections.Generic;
using System.Collections.ObjectModel;
using FileID.ViewModels;
using Xunit;

namespace FileID.App.Tests;

public class CleanupModeSwitchTests
{
    private static DuplicateGroup Group(string hash, params long[] memberIds)
    {
        var members = new List<DuplicateMember>();
        foreach (var id in memberIds)
        {
            members.Add(new DuplicateMember
            {
                Id = id,
                Path = $"C:/lib/{hash}/{id}.jpg",
                FileName = $"{id}.jpg",
                SizeBytes = 4,
                GroupKey = hash,
            });
        }
        return new DuplicateGroup { ContentHash = hash, Members = members };
    }

    [Fact]
    public void MergeWithEmptyRows_ClearsStaleGroups()
    {
        var groups = new ObservableCollection<DuplicateGroup>
        {
            Group("dup-AA:4", 1, 2),
            Group("dup-BB:4", 3, 4),
        };

        CleanupViewModel.MergeByContentHash(groups, new List<DuplicateGroup>());

        Assert.Empty(groups);
    }

    [Fact]
    public void MergeAfterClear_RepopulatesFreshMode()
    {
        var groups = new ObservableCollection<DuplicateGroup>();
        CleanupViewModel.MergeByContentHash(groups, new List<DuplicateGroup>());
        Assert.Empty(groups);

        var similar = new List<DuplicateGroup> { Group("sim-7", 7, 8) };
        CleanupViewModel.MergeByContentHash(groups, similar);

        Assert.Single(groups);
        Assert.Equal("sim-7", groups[0].ContentHash);
    }
}
