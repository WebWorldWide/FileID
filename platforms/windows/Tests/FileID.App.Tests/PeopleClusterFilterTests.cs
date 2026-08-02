using FileID.ViewModels;
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
        CREATE TABLE face_prints (
            id INTEGER PRIMARY KEY,
            person_id INTEGER,
            face_quality REAL,
            excluded INTEGER DEFAULT 0
        );
        """;

    /// Mirrors PeopleViewModel.LoadClusters' visibility predicate.
    private const string VisibleSql = """
        SELECT p.id, COUNT(fp.id) AS member_count
        FROM persons p
        JOIN face_prints fp ON fp.person_id = p.id
             AND COALESCE(fp.excluded, 0) = 0
        WHERE ($hide_unknown = 0 OR COALESCE(p.is_unknown, 0) = 0)
        GROUP BY p.id
        ORDER BY member_count DESC
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
        for (int i = 0; i < 15; i++) rows.Add($"({id++}, 1, 0.4, 0)");  // 15 faces
        for (int i = 0; i < 2; i++) rows.Add($"({id++}, 2, 0.4, 0)");   // 2 faces
        for (int i = 0; i < 2; i++) rows.Add($"({id++}, 3, 0.4, 0)");   // 2 faces, named
        for (int i = 0; i < 3; i++) rows.Add($"({id++}, 4, 0.4, 0)");   // 3 faces, unknown
        for (int i = 0; i < 8; i++) rows.Add($"({id++}, 5, 0.4, 1)");   // 8 EXCLUDED faces
        faces.CommandText =
            "INSERT INTO face_prints (id, person_id, face_quality, excluded) VALUES " +
            string.Join(",", rows) + ";";
        faces.ExecuteNonQuery();
        return connection;
    }

    private static List<long> Visible(
        SqliteConnection c,
        bool hideUnknown)
    {
        using var cmd = c.CreateCommand();
        cmd.CommandText = VisibleSql;
        cmd.Parameters.AddWithValue("$hide_unknown", hideUnknown ? 1 : 0);
        var ids = new List<long>();
        using var r = cmd.ExecuteReader();
        while (r.Read()) ids.Add(r.GetInt64(0));
        return ids;
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
