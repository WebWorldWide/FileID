using FileID.Services;
using Microsoft.Data.Sqlite;
using Xunit;

namespace FileID.App.Tests;

public sealed class PersonTagReadStoreTests : IDisposable
{
    private readonly string _dbPath = Path.Combine(
        Path.GetTempPath(), $"fileid-person-tags-{Guid.NewGuid():N}.sqlite");

    [Fact]
    public async Task NamedPeopleUseEveryNameFieldAndDeduplicateFiles()
    {
        BuildDatabase();
        await using var store = new ReadStore(_dbPath);
        await store.OpenAsync();

        var people = await store.NamedPersonFileIdsAsync(default);
        var fileIds = await store.PersonFileIdsAsync(7, default);

        Assert.Collection(people.Keys, key => Assert.Equal("Dr. Ada M. Lovelace PhD", key));
        Assert.Collection(
            people["Dr. Ada M. Lovelace PhD"],
            fileId => Assert.Equal(1, fileId));
        Assert.Collection(fileIds, fileId => Assert.Equal(1, fileId));
    }

    [Fact]
    public void PersonTagNameFallsBackToLegacyOnlyWhenStructuredNameIsEmpty()
    {
        Assert.Equal(
            "Grandma Ada Lovelace Jr.",
            ReadStore.FormatPersonTagName(" Grandma ", "Ada", null, "Lovelace", "Jr.", "ignored"));
        Assert.Equal(
            "Legacy Name",
            ReadStore.FormatPersonTagName(null, " ", null, null, null, " Legacy Name "));
    }

    private void BuildDatabase()
    {
        using var connection = new SqliteConnection($"Data Source={_dbPath}");
        connection.Open();
        connection.ExecuteNonQuery("""
            CREATE TABLE files (id INTEGER PRIMARY KEY, failed INTEGER NOT NULL);
            CREATE TABLE persons (
                id INTEGER PRIMARY KEY,
                title TEXT,
                first_name TEXT,
                middle_name TEXT,
                last_name TEXT,
                suffix TEXT,
                name TEXT,
                is_unknown INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE face_prints (id INTEGER PRIMARY KEY, file_id INTEGER, person_id INTEGER);
            INSERT INTO files VALUES (1, 0), (2, 1);
            INSERT INTO persons VALUES (7, 'Dr.', 'Ada', 'M.', 'Lovelace', 'PhD', 'legacy', 0);
            INSERT INTO face_prints VALUES (10, 1, 7), (11, 1, 7), (12, 2, 7);
            """);
    }

    public void Dispose()
    {
        SqliteConnection.ClearAllPools();
        try { File.Delete(_dbPath); } catch { }
    }
}

internal static class PersonTagSqliteExtensions
{
    internal static void ExecuteNonQuery(this SqliteConnection connection, string sql)
    {
        using var command = connection.CreateCommand();
        command.CommandText = sql;
        command.ExecuteNonQuery();
    }
}
