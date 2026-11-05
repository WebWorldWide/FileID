using FileID.Services;
using Microsoft.Data.Sqlite;
using Xunit;

namespace FileID.App.Tests;

public sealed class ReadStoreTimelineTracksTests : IDisposable
{
    private readonly string _dbPath = Path.Combine(Path.GetTempPath(), $"fileid-timeline-test-{Guid.NewGuid():N}.sqlite");

    public void Dispose()
    {
        SqliteConnection.ClearAllPools();
        try { if (File.Exists(_dbPath)) File.Delete(_dbPath); }
        catch (IOException) { }
    }

    private void Build(string sql)
    {
        using var connection = new SqliteConnection(new SqliteConnectionStringBuilder { DataSource = _dbPath }.ToString());
        connection.Open();
        using var command = connection.CreateCommand();
        command.CommandText = sql;
        command.ExecuteNonQuery();
    }

    private async Task<TimelineTracks> ReadAsync(long fileId)
    {
        await using var store = new ReadStore(_dbPath);
        await store.OpenAsync();
        return await store.TimelineTracksAsync(fileId, CancellationToken.None);
    }

    [Fact]
    public async Task ExaminedAndSpeechLanesFollowTheGtkFilters()
    {
        Build("""
            CREATE TABLE files (id INTEGER PRIMARY KEY);
            CREATE TABLE catalog_coverage (id TEXT PRIMARY KEY, file_id INTEGER, start_seconds REAL, end_seconds REAL, status TEXT);
            CREATE TABLE catalog_passages (id TEXT PRIMARY KEY, file_id INTEGER, start_seconds REAL, end_seconds REAL, stale INTEGER NOT NULL DEFAULT 0);
            INSERT INTO catalog_coverage VALUES
                ('frame:2', 7, 20, 20, 'sampled'),
                ('frame:1', 7, 10, 14, 'verified'),
                ('gap', 7, 0, 60, 'incomplete'),
                ('speech:1', 7, 30, 31, 'sampled'),
                ('other', 8, 1, 2, 'sampled');
            INSERT INTO catalog_passages VALUES
                ('speech:b', 7, 40, NULL, 0),
                ('speech:a', 7, 33, 36, 0),
                ('speech:old', 7, 5, 6, 1),
                ('speech:none', 7, NULL, NULL, 0),
                ('chapter:x', 7, 50, 52, 0);
            """);
        var tracks = await ReadAsync(7);
        Assert.Equal("10-14 20-20", string.Join(" ", tracks.Examined.Select(span => FormattableString.Invariant($"{span.Start:0}-{span.End:0}"))));
        Assert.Equal("33-36 40-40", string.Join(" ", tracks.Speech.Select(span => FormattableString.Invariant($"{span.Start:0}-{span.End:0}"))));
    }

    [Fact]
    public async Task AnOlderCatalogWithoutTheTablesGivesEmptyLanes()
    {
        Build("CREATE TABLE files (id INTEGER PRIMARY KEY);");
        var tracks = await ReadAsync(7);
        Assert.Empty(tracks.Examined);
        Assert.Empty(tracks.Speech);
    }

    [Fact]
    public async Task AMissingCatalogFileGivesEmptyLanes()
    {
        var tracks = await ReadAsync(7);
        Assert.Same(TimelineTracks.Empty, tracks);
    }
}
