using FileID.ViewModels;
using FileID.Views.People;
using Microsoft.Data.Sqlite;
using Xunit;

namespace FileID.App.Tests;

/// Pins the People-grid presentation contract: every active face cluster is
/// visible regardless of size, while the explicit Unknown filter still applies.
public class PeopleClusterFilterTests
{
    private const string Schema = """
        CREATE TABLE persons (
            id INTEGER PRIMARY KEY,
            name TEXT,
            title TEXT,
            first_name TEXT,
            middle_name TEXT,
            last_name TEXT,
            suffix TEXT,
            representative_face_id INTEGER,
            is_unknown INTEGER
        );
        CREATE TABLE files (id INTEGER PRIMARY KEY, failed INTEGER NOT NULL DEFAULT 0);
        CREATE TABLE face_prints (
            id INTEGER PRIMARY KEY,
            file_id INTEGER NOT NULL,
            person_id INTEGER,
            face_quality REAL,
            excluded INTEGER DEFAULT 0, bbox TEXT, embedding_model TEXT,
            processing_version TEXT, source_revision TEXT
        );
        """;

    private static SqliteConnection Seed()
    {
        var connection = new SqliteConnection("Data Source=:memory:");
        connection.Open();
        using var cmd = connection.CreateCommand();
        cmd.CommandText = Schema + """
            INSERT INTO persons (id, name, title, first_name, middle_name, last_name, suffix, is_unknown) VALUES
                (1, NULL, NULL, NULL, NULL, NULL, NULL, 0),   -- big unnamed cluster: shown
                (2, NULL, NULL, NULL, NULL, NULL, NULL, 0),   -- tiny unnamed cluster: hidden
                (3, NULL, 'Dr', NULL, NULL, NULL, NULL, 0),   -- tiny but structured-named: shown
                (4, NULL, NULL, NULL, NULL, NULL, NULL, 1),   -- tiny + is_unknown: explicit toggle wins
                (5, NULL, NULL, NULL, NULL, NULL, NULL, 0);   -- tiny, all faces excluded: hidden
            """;
        cmd.ExecuteNonQuery();

        using var faces = connection.CreateCommand();
        var rows = new List<string>();
        int id = 1;
        for (int i = 0; i < 15; i++) rows.Add($"({id}, {id++}, 1, 0.4, 0)");  // 15 faces
        for (int i = 0; i < 2; i++) rows.Add($"({id}, {id++}, 2, 0.4, 0)");   // 2 faces
        for (int i = 0; i < 2; i++) rows.Add($"({id}, {id++}, 3, 0.4, 0)");   // 2 faces, named
        for (int i = 0; i < 3; i++) rows.Add($"({id}, {id++}, 4, 0.4, 0)");   // 3 faces, unknown
        for (int i = 0; i < 8; i++) rows.Add($"({id}, {id++}, 5, 0.4, 1)");   // 8 EXCLUDED faces
        faces.CommandText =
            "INSERT INTO files(id) VALUES " + string.Join(",", Enumerable.Range(1, id - 1).Select(fileId => $"({fileId})")) + ";" +
            "INSERT INTO face_prints (id, file_id, person_id, face_quality, excluded) VALUES " +
            string.Join(",", rows) + ";";
        faces.ExecuteNonQuery();
        return connection;
    }

    private static List<long> Visible(
        SqliteConnection c,
        bool hideUnknown)
    {
        using var cmd = c.CreateCommand();
        cmd.CommandText = PeopleViewModel.ClusterQuery(hideUnknown);
        var ids = new List<long>();
        using var r = cmd.ExecuteReader();
        while (r.Read()) ids.Add(r.GetInt64(0));
        return ids;
    }

    [Fact]
    public void RefreshedRepresentativeProvenanceKeepsIdentityAndRejectedAnchorFallsBack()
    {
        using var connection = Seed();
        using var update = connection.CreateCommand();
        update.CommandText = "UPDATE persons SET representative_face_id=16 WHERE id=1;";
        update.ExecuteNonQuery();
        using var query = connection.CreateCommand();
        query.CommandText = PeopleViewModel.ClusterQuery(false);
        string previous;
        using (var reader = query.ExecuteReader())
        {
            Assert.True(reader.Read());
            Assert.Equal(1L, reader.GetInt64(0));
            Assert.Equal(1L, reader.GetInt64(3));
            previous = reader.GetString(4);
        }
        foreach (var sql in new[]
        {
            "UPDATE face_prints SET processing_version='v3' WHERE id=1",
            "UPDATE face_prints SET embedding_model='model-b' WHERE id=1",
            "UPDATE face_prints SET source_revision='source-b' WHERE id=1",
            "UPDATE face_prints SET bbox='new bounds' WHERE id=1"
        })
        {
            update.CommandText = sql;
            update.ExecuteNonQuery();
            using var reader = query.ExecuteReader();
            Assert.True(reader.Read());
            Assert.Equal(1L, reader.GetInt64(0));
            Assert.Equal(1L, reader.GetInt64(3));
            Assert.NotEqual(previous, reader.GetString(4));
            previous = reader.GetString(4);
        }
        update.CommandText = "UPDATE persons SET representative_face_id=1 WHERE id=1; UPDATE face_prints SET excluded=1 WHERE id=1;";
        update.ExecuteNonQuery();
        using var final = query.ExecuteReader();
        Assert.True(final.Read());
        Assert.Equal(1L, final.GetInt64(0));
        Assert.Equal(14L, final.GetInt64(2));
        Assert.Equal(2L, final.GetInt64(3));
    }

    [Fact]
    public void FailedFacesDoNotCountOrAnchorActiveCards()
    {
        using var connection = Seed();
        using var update = connection.CreateCommand();
        update.CommandText = "UPDATE files SET failed=1 WHERE id IN (1,16,17); UPDATE persons SET representative_face_id=1 WHERE id=1; UPDATE face_prints SET excluded=1 WHERE id=2;";
        update.ExecuteNonQuery();
        Assert.DoesNotContain(2L, Visible(connection, hideUnknown: false));
        using var query = connection.CreateCommand();
        query.CommandText = PeopleViewModel.ClusterQuery(false);
        using var reader = query.ExecuteReader();
        Assert.True(reader.Read());
        Assert.Equal(1L, reader.GetInt64(0));
        Assert.Equal(13L, reader.GetInt64(2));
        Assert.Equal(3L, reader.GetInt64(3));
    }

    [Fact]
    public void DetailCountsAndThumbnailsUseActiveFacesWithoutRemovingIdentityRecords()
    {
        using var connection = Seed();
        using var update = connection.CreateCommand();
        update.CommandText = "UPDATE files SET failed=1 WHERE id IN (1,16,17); UPDATE face_prints SET excluded=1 WHERE id=2; UPDATE persons SET first_name='Ada' WHERE id=1;";
        update.ExecuteNonQuery();
        foreach (var (personId, expectedCount) in new[] { (1, 13), (2, 0), (5, 0) })
        {
            using var person = connection.CreateCommand();
            person.CommandText = PersonDetailSheet.PersonQuery;
            person.Parameters.AddWithValue("@id", personId);
            using (var reader = person.ExecuteReader())
            {
                Assert.True(reader.Read());
                Assert.Equal(expectedCount, reader.GetInt32(5));
                if (personId == 1) Assert.Equal("Ada", reader.GetString(1));
            }
            using var faces = connection.CreateCommand();
            faces.CommandText = PersonDetailSheet.FacesQuery;
            faces.Parameters.AddWithValue("@id", personId);
            var ids = new List<long>();
            using var tiles = faces.ExecuteReader();
            while (tiles.Read()) ids.Add(tiles.GetInt64(0));
            Assert.Equal(expectedCount, ids.Count);
            Assert.DoesNotContain(1L, ids);
            Assert.DoesNotContain(2L, ids);
        }
        update.CommandText = "SELECT COUNT(*) FROM face_prints";
        Assert.Equal(30L, (long)update.ExecuteScalar()!);
    }

    [Fact]
    public void DeferredCoverageStatusPreservesUnknownLegacyCoverage()
    {
        Assert.Equal(string.Empty, PeopleViewModel.DeferredFaceStatus(null));
        Assert.Equal(string.Empty, PeopleViewModel.DeferredFaceStatus(0));
        Assert.Equal("1 face awaits analysis; existing assignments were kept.", PeopleViewModel.DeferredFaceStatus(1));
        Assert.Equal("3 faces await analysis; existing assignments were kept.", PeopleViewModel.DeferredFaceStatus(3));
    }

    [Fact]
    public void SmallUnnamedClustersAreVisible()
    {
        using var c = Seed();
        var visible = Visible(c, hideUnknown: false);

        Assert.Contains(1L, visible);
        Assert.Contains(2L, visible);
        Assert.Contains(3L, visible);
    }

    [Fact]
    public void PeopleWithOnlyExcludedFacesAreNotVisible()
    {
        using var c = Seed();
        // Person 5 has only excluded faces, so it has no active cluster card.
        Assert.DoesNotContain(5L, Visible(c, hideUnknown: false));
    }

    [Fact]
    public void HideUnknownStillApplies()
    {
        using var c = Seed();
        Assert.Contains(4L, Visible(c, hideUnknown: false));
        Assert.DoesNotContain(4L, Visible(c, hideUnknown: true));
    }

}
