using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.ViewModels;

namespace FileID.Services;

internal static class TagChangeJournal
{
    internal static string FormatLabel(string mode, int fileCount)
    {
        var action = mode switch
        {
            "add" => "add tags to",
            "remove" => "remove tags from",
            _ => "replace tags on",
        };
        return $"{action} {fileCount} file{(fileCount == 1 ? string.Empty : "s")}";
    }

    internal static async Task<IReadOnlyDictionary<long, List<string>>> CapturePriorUserTagsAsync(
        IReadOnlyList<long> fileIds)
    {
        var distinctIds = fileIds.Distinct().ToArray();
        var map = new Dictionary<long, List<string>>();
        if (distinctIds.Length == 0) return map;

        await Task.Run(() =>
        {
            if (!System.IO.File.Exists(AppPaths.DbPath))
            {
                throw new InvalidOperationException(
                    "The library database is unavailable, so existing tags cannot be saved for undo.");
            }

            using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                {
                    DataSource = AppPaths.DbPath,
                    Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                }.ToString());
            conn.Open();
            foreach (var chunk in distinctIds.Chunk(500))
            {
                using var cmd = conn.CreateCommand();
                cmd.CommandText =
                    $"SELECT file_id, tag FROM tags WHERE source = 'user' AND file_id IN ({string.Join(",", chunk)}) ORDER BY file_id, rowid";
                using var reader = cmd.ExecuteReader();
                while (reader.Read())
                {
                    var fileId = reader.GetInt64(0);
                    if (!map.TryGetValue(fileId, out var tags))
                    {
                        tags = [];
                        map[fileId] = tags;
                    }
                    tags.Add(reader.GetString(1));
                }
            }
        }).ConfigureAwait(false);
        return map;
    }

    internal static List<(List<long> Ids, List<string> Tags)> GroupByTagSet(
        IReadOnlyList<long> fileIds,
        IReadOnlyDictionary<long, List<string>> priorTags)
    {
        var groups = new Dictionary<string, (List<long> Ids, List<string> Tags)>(
            StringComparer.Ordinal);
        foreach (var fileId in fileIds.Distinct())
        {
            var tags = priorTags.TryGetValue(fileId, out var prior)
                ? prior.OrderBy(tag => tag, StringComparer.Ordinal).ToList()
                : [];
            var key = string.Concat(tags.Select(tag => $"{tag.Length}:{tag}"));
            if (!groups.TryGetValue(key, out var group))
            {
                group = ([], tags);
                groups[key] = group;
            }
            group.Ids.Add(fileId);
        }
        return groups.Values.ToList();
    }

    internal static List<(List<long> Ids, List<string> Tags)> BuildScopedReplacementGroups(
        IReadOnlyList<long> fileIds,
        IReadOnlyDictionary<long, List<string>> priorTags,
        string oldTag,
        string newTag)
    {
        ArgumentException.ThrowIfNullOrWhiteSpace(oldTag);
        ArgumentException.ThrowIfNullOrWhiteSpace(newTag);
        var replacement = newTag.Trim();
        var desired = new Dictionary<long, List<string>>();
        foreach (var fileId in fileIds.Distinct())
        {
            var tags = priorTags.TryGetValue(fileId, out var prior)
                ? prior.Where(tag => !string.Equals(tag, oldTag, StringComparison.OrdinalIgnoreCase))
                    .ToList()
                : [];
            if (!tags.Contains(replacement, StringComparer.OrdinalIgnoreCase))
            {
                tags.Add(replacement);
            }
            desired[fileId] = tags;
        }
        return GroupByTagSet(fileIds, desired);
    }

    internal static void PushUndo(
        string label,
        IReadOnlyList<long> confirmedFileIds,
        IReadOnlyDictionary<long, List<string>> priorUserTags)
    {
        var groups = GroupByTagSet(confirmedFileIds, priorUserTags);
        if (groups.Count == 0) return;

        UndoStack.Instance.Push(label, ChangeKind.Tags, async () =>
        {
            var confirmed = await RestoreGroupsConfirmedAsync(
                groups,
                (ids, tags) => EngineClient.Instance.WaitForBulkActionResultAsync(
                    "applyTags",
                    () => EngineClient.Instance.ApplyTagsAsync(ids, tags, "replace"),
                    BulkActionTimeout.ForFileCount(ids.Count))).ConfigureAwait(false);
            if (!confirmed)
            {
                throw new InvalidOperationException(
                    "The engine did not confirm every restored tag group.");
            }
            return true;
        });
    }

    internal static async Task<bool> RestoreGroupsConfirmedAsync(
        IReadOnlyList<(List<long> Ids, List<string> Tags)> groups,
        Func<IReadOnlyList<long>, IReadOnlyList<string>, Task<BulkActionResult>> reverse)
    {
        if (groups.Count == 0) return false;
        foreach (var (ids, tags) in groups)
        {
            var result = await reverse(ids, tags).ConfigureAwait(false);
            if (!BulkActionResultTruth.ConfirmsExactSuccess(result, ids))
            {
                return false;
            }
        }
        return true;
    }
}
