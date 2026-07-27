// F11: when a Similar-mode load fails (e.g. LoadSimilar's >20k image cap
// exception), the previously loaded Exact groups must not stay rendered under
// the Similar header. RefreshAsync clears the list via MergeByContentHash with
// an empty row set — verify that path really empties a populated collection.

using System.Collections.Generic;
using System.Collections.ObjectModel;
using FileID.ViewModels;
using Microsoft.Data.Sqlite;
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
    public void ExactDuplicateLoadRanksAndGroupsInOneQuery()
    {
        var dbPath = Path.Combine(Path.GetTempPath(), $"fileid-cleanup-{Guid.NewGuid():N}.sqlite");
        try
        {
            using (var connection = new SqliteConnection($"Data Source={dbPath}"))
            {
                connection.Open();
                using var create = connection.CreateCommand();
                create.CommandText = """
                    CREATE TABLE files (
                        id INTEGER PRIMARY KEY,
                        path_text TEXT NOT NULL,
                        size_bytes INTEGER NOT NULL,
                        modified_at REAL,
                        aesthetic REAL,
                        created_at REAL,
                        content_hash BLOB,
                        failed INTEGER NOT NULL DEFAULT 0
                    );
                    """;
                create.ExecuteNonQuery();
                using var insert = connection.CreateCommand();
                insert.CommandText = """
                    INSERT INTO files
                        (id, path_text, size_bytes, modified_at, aesthetic, created_at, content_hash, failed)
                    VALUES ($id, $path, $size, 1, $aesthetic, $created, $hash, 0)
                    """;
                for (var id = 1; id <= 5; id++)
                {
                    insert.Parameters.Clear();
                    insert.Parameters.AddWithValue("$id", id);
                    insert.Parameters.AddWithValue("$path", $"C:/lib/{id}.jpg");
                    insert.Parameters.AddWithValue("$size", id <= 3 ? 100 : 200);
                    insert.Parameters.AddWithValue("$aesthetic", id == 2 ? 0.9 : 0.1);
                    insert.Parameters.AddWithValue("$created", id);
                    insert.Parameters.AddWithValue("$hash", id <= 3 ? new byte[] { 0xAA } : new byte[] { 0xBB });
                    insert.ExecuteNonQuery();
                }
            }

            var groups = CleanupViewModel.LoadExactFromPath(dbPath, CancellationToken.None);
            Assert.Equal(2, groups.Count);
            Assert.Equal(3, groups[0].TotalMemberCount);
            Assert.Equal(2, groups[0].Members[0].Id);
            Assert.True(groups[0].Members[0].IsKeeper);
            Assert.Equal(2, groups[1].TotalMemberCount);
        }
        finally
        {
            SqliteConnection.ClearAllPools();
            File.Delete(dbPath);
        }
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
