using System.Text.Json;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.Data.Sqlite;
using Xunit;

namespace FileID.App.Tests;

public sealed class LibraryScopeTests
{
    private static LibraryRoot Root(long id, string path, long count = 1, bool online = true) =>
        new(id, path, System.IO.Path.GetFileName(path.TrimEnd('/', '\\')), "external", true, online, count, 1);

    private static readonly LibraryRoot[] Roots =
    {
        Root(1, "/m/Camera/"), Root(2, "/m/Camera2"), Root(3, "/m/Camera/Synced"), Root(4, "/elsewhere"),
    };

    [Fact]
    public void AScopeKeepsItsOwnFilesNotLookalikesOrDeeperLocations()
    {
        var scope = LibraryScope.For(1, Roots)!;
        foreach (var inside in new[] { "/m/Camera", "/m/Camera/a.jpg", "/m/Camera/2020/b.jpg", "/m/Camera/Synced2/c.jpg" })
            Assert.True(scope.Contains(inside), inside);
        foreach (var outside in new[] { "/m/Camera2/a.jpg", "/m/Cameras/a.jpg", "/m/Camera/Synced", "/m/Camera/Synced/d.jpg", "/m/Cam", "/elsewhere/e.jpg", null })
            Assert.False(scope.Contains(outside), outside);
        Assert.True(LibraryScope.For(3, Roots)!.Contains("/m/Camera/Synced/d.jpg"));
        Assert.Null(LibraryScope.For(9, Roots));
        Assert.Null(LibraryScope.For(null, Roots));
        var windows = LibraryScope.For(5, new[] { Root(5, @"D:\Photos\"), Root(6, @"D:\Photos\Old") })!;
        Assert.True(windows.Contains(@"D:\Photos\a.jpg"));
        Assert.False(windows.Contains(@"D:\Photos2\a.jpg"));
        Assert.False(windows.Contains(@"D:\Photos\Old\a.jpg"));
    }

    [Fact]
    public void TheChoiceSurvivesLoadingAndFallsBackWhenItsLocationIsGone()
    {
        Assert.Equal(2, LibraryScope.Reconciled(2, Array.Empty<LibraryRoot>()));
        Assert.Equal(2, LibraryScope.Reconciled(2, Roots));
        Assert.Null(LibraryScope.Reconciled(9, Roots));
        Assert.Null(LibraryScope.Reconciled(null, Roots));
    }

    [Fact]
    public void TheTitleBarNamesTheLocation()
    {
        Assert.Equal("Camera2", LibraryScope.Title(LibraryScope.For(2, Roots), Roots, "Pictures"));
        Assert.Equal("All locations", LibraryScope.Title(null, Roots, "Pictures"));
        Assert.Equal("Pictures", LibraryScope.Title(null, Array.Empty<LibraryRoot>(), "Pictures"));
        Assert.Equal("Choose a folder", LibraryScope.Title(null, Array.Empty<LibraryRoot>(), null));
        Assert.Equal($"Card, {1204:N0} files, offline", LibraryScope.Spoken(Root(7, "/Volumes/Card", 1204, online: false)));
        Assert.Equal("One, 1 file", LibraryScope.Spoken(Root(8, "/Users/a/One")));
        Assert.Equal("1 location", LibraryScope.LocationCountText(1));
    }

    [Fact]
    public void TheSqlClauseAgreesWithContains()
    {
        using var conn = new SqliteConnection("Data Source=:memory:");
        conn.Open();
        using (var create = conn.CreateCommand())
        {
            create.CommandText = "CREATE TABLE files(id INTEGER PRIMARY KEY, path_text TEXT NOT NULL); INSERT INTO files VALUES(1,'/m/Camera/a.jpg'),(2,'/m/Camera/x/b.jpg'),(3,'/m/Camera2/c.jpg'),(4,'/m/Cameras/d.jpg'),(5,'/m/Camera/Synced/e.jpg'),(6,'/m/Camera');";
            create.ExecuteNonQuery();
        }
        var paths = new[] { "/m/Camera/a.jpg", "/m/Camera/x/b.jpg", "/m/Camera2/c.jpg", "/m/Cameras/d.jpg", "/m/Camera/Synced/e.jpg", "/m/Camera" };
        foreach (var id in new long[] { 1, 2, 3 })
        {
            var scope = LibraryScope.For(id, Roots)!;
            using var cmd = conn.CreateCommand();
            cmd.CommandText = "SELECT id FROM files f WHERE " + LibraryScope.Sql("f.path_text", "$scope") + " ORDER BY id";
            cmd.Parameters.AddWithValue("$scope", scope.Param);
            var kept = new List<long>();
            using (var reader = cmd.ExecuteReader()) while (reader.Read()) kept.Add(reader.GetInt64(0));
            var expected = paths.Select((path, index) => (path, id: index + 1L)).Where(p => scope.Contains(p.path)).Select(p => p.id).ToList();
            Assert.Equal(expected, kept);
        }
        Assert.Equal(new long[] { 1, 2, 6 }, LibraryScopeIds(conn, LibraryScope.For(1, Roots)!));
    }

    private static long[] LibraryScopeIds(SqliteConnection conn, LibraryScope scope)
    {
        using var cmd = conn.CreateCommand();
        cmd.CommandText = "SELECT id FROM files f WHERE " + LibraryScope.Sql("f.path_text", "$scope") + " ORDER BY id";
        cmd.Parameters.AddWithValue("$scope", scope.Param);
        var ids = new List<long>();
        using var reader = cmd.ExecuteReader();
        while (reader.Read()) ids.Add(reader.GetInt64(0));
        return ids.ToArray();
    }

    [Fact]
    public void ChangingTheLocationRunsTheCurrentMomentsSearchAgain()
    {
        Assert.Equal(MomentsScopePolicy.Step.Search, MomentsScopePolicy.OnScopeChanged(null, 2, "birthday", busy: false));
        Assert.Equal(MomentsScopePolicy.Step.Search, MomentsScopePolicy.OnScopeChanged(2, null, "birthday", busy: false));
        Assert.Equal(MomentsScopePolicy.Step.AfterCurrent, MomentsScopePolicy.OnScopeChanged(1, 2, "birthday", busy: true));
        Assert.Equal(MomentsScopePolicy.Step.Nothing, MomentsScopePolicy.OnScopeChanged(2, 2, "birthday", busy: false));
        Assert.Equal(MomentsScopePolicy.Step.Nothing, MomentsScopePolicy.OnScopeChanged(null, 2, "  ", busy: false));
        Assert.Equal(MomentsScopePolicy.Step.Nothing, MomentsScopePolicy.OnScopeChanged(null, 2, null, busy: false));
        Assert.True(MomentsScopePolicy.RunPending(true, "birthday", busy: false));
        Assert.False(MomentsScopePolicy.RunPending(true, "birthday", busy: true));
        Assert.False(MomentsScopePolicy.RunPending(false, "birthday", busy: false));
        Assert.False(MomentsScopePolicy.RunPending(true, "", busy: false));
    }

    [Fact]
    public void SearchAndChatSendTheChosenLocation()
    {
        var search = JsonSerializer.Serialize(CatalogEmbeddingSpace.SearchRequest("beach", "keyword", null, null, null, 3), IpcCoder.Options);
        Assert.Contains("\"rootID\":3", search);
        Assert.DoesNotContain("rootID", JsonSerializer.Serialize(CatalogEmbeddingSpace.SearchRequest("beach", "keyword"), IpcCoder.Options));
        var chat = JsonSerializer.Serialize(ChatActionPolicy.Context(null, "/m/Camera", null, 1), IpcCoder.Options);
        Assert.Contains("\"rootID\":1", chat);
        Assert.Null(ChatActionPolicy.Context(null, "/m/Camera").RootID);
    }
}

public sealed class ReadStoreScopeTests : IDisposable
{
    private readonly string _dbPath = System.IO.Path.Combine(System.IO.Path.GetTempPath(), $"fileid-scope-test-{Guid.NewGuid():N}.sqlite");

    public void Dispose()
    {
        SqliteConnection.ClearAllPools();
        try { if (File.Exists(_dbPath)) File.Delete(_dbPath); } catch { /* best effort */ }
    }

    [Fact]
    public async Task TheLibraryListingCountsAndPagesKeepToTheScope()
    {
        using (var conn = new SqliteConnection(new SqliteConnectionStringBuilder { DataSource = _dbPath }.ToString()))
        {
            conn.Open();
            using var cmd = conn.CreateCommand();
            cmd.CommandText = """
                CREATE TABLE files (id INTEGER PRIMARY KEY, path_text TEXT NOT NULL, kind TEXT NOT NULL, size_bytes INTEGER NOT NULL DEFAULT 1,
                    modified_at REAL, has_faces INTEGER NOT NULL DEFAULT 0, has_text INTEGER NOT NULL DEFAULT 0, failed INTEGER NOT NULL DEFAULT 0,
                    vlm_proposed_name TEXT, scanned_at REAL NOT NULL DEFAULT 0);
                CREATE TABLE clip_embeddings (file_id INTEGER NOT NULL, embedding BLOB NOT NULL);
                CREATE TABLE tags (file_id INTEGER NOT NULL, tag TEXT NOT NULL, source TEXT NOT NULL, score REAL);
                INSERT INTO files(id,path_text,kind,scanned_at) VALUES
                    (1,'/m/Camera/a.jpg','image',1),(2,'/m/Camera/2020/b.mov','video',2),(3,'/m/Camera2/c.jpg','image',3),
                    (4,'/m/Camera/Synced/d.jpg','image',4),(5,'/m/Cameras/e.jpg','image',5);
                """;
            cmd.ExecuteNonQuery();
        }
        var roots = new LibraryRoot[]
        {
            new(1, "/m/Camera", "Camera", "external", true, true, 2, 2), new(2, "/m/Camera2", "Camera2", "external", true, true, 1, 1),
            new(3, "/m/Camera/Synced", "Synced", "external", true, true, 1, 1),
        };
        using var store = new ReadStore(_dbPath);
        await store.OpenAsync(CancellationToken.None);
        Assert.Equal(new long[] { 5, 4, 3, 2, 1 }, (await store.RecentAsync(10, CancellationToken.None)).Select(r => r.Id).ToArray());
        store.Scope = LibraryScope.For(1, roots);
        Assert.Equal(new long[] { 2, 1 }, (await store.RecentAsync(10, CancellationToken.None)).Select(r => r.Id).ToArray());
        Assert.Equal(new long[] { 1 }, (await store.RecentAsync(1, CancellationToken.None, offset: 1)).Select(r => r.Id).ToArray());
        Assert.Equal(new long[] { 1 }, (await store.RecentAsync(10, CancellationToken.None, "image")).Select(r => r.Id).ToArray());
        var counts = await store.KindCountsAsync(CancellationToken.None);
        Assert.Equal(1, counts["image"]);
        Assert.Equal(1, counts["video"]);
        store.Scope = LibraryScope.For(3, roots);
        Assert.Equal(new long[] { 4 }, (await store.RecentAsync(10, CancellationToken.None)).Select(r => r.Id).ToArray());
        store.Scope = null;
        Assert.Equal(5, (await store.RecentAsync(10, CancellationToken.None)).Count);
    }
}
