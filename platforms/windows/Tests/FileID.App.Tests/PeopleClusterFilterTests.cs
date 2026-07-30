using FileID.ViewModels;
using Microsoft.Data.Sqlite;
using Xunit;

namespace FileID.App.Tests;

/// Pins the People-grid size floor. Face clustering over-splits badly — a real
/// 135k-file library produced 3,108 clusters of which 2,271 held <=5 faces
/// (duplicate-burst fragments of one shot, not distinct people), which buried the
/// few dozen clusters actually worth naming. The grid holds small fragments back,
/// but must never hide a cluster the user has NAMED, and must disclose the count
/// it withheld rather than dropping them silently.
public class PeopleClusterFilterTests
{
    private const string Schema = """
        CREATE TABLE persons (
            id INTEGER PRIMARY KEY,
            name TEXT,
            first_name TEXT,
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
        HAVING COUNT(fp.id) >= $min_faces
           OR (p.name IS NOT NULL AND TRIM(p.name) <> '')
           OR (p.first_name IS NOT NULL AND TRIM(p.first_name) <> '')
        ORDER BY member_count DESC
        """;

    private static SqliteConnection Seed()
    {
        var connection = new SqliteConnection("Data Source=:memory:");
        connection.Open();
        using var cmd = connection.CreateCommand();
        cmd.CommandText = Schema + """
            INSERT INTO persons (id, name, first_name, is_unknown) VALUES
                (1, NULL, NULL, 0),   -- big unnamed cluster: shown
                (2, NULL, NULL, 0),   -- tiny unnamed cluster: hidden
                (3, NULL, 'Ada', 0),  -- tiny but NAMED: shown
                (4, NULL, NULL, 1),   -- tiny + is_unknown: hidden
                (5, NULL, NULL, 0);   -- tiny, all faces excluded: hidden
            """;
        cmd.ExecuteNonQuery();

        using var faces = connection.CreateCommand();
        var rows = new List<string>();
        int id = 1;
        for (int i = 0; i < 9; i++) rows.Add($"({id++}, 1, 0.4, 0)");   // 9 faces
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

    private static List<long> Visible(SqliteConnection c, bool hideUnknown)
    {
        using var cmd = c.CreateCommand();
        cmd.CommandText = VisibleSql;
        cmd.Parameters.AddWithValue("$hide_unknown", hideUnknown ? 1 : 0);
        cmd.Parameters.AddWithValue("$min_faces", PeopleViewModel.MinFacesPerCluster);
        var ids = new List<long>();
        using var r = cmd.ExecuteReader();
        while (r.Read()) ids.Add(r.GetInt64(0));
        return ids;
    }

    [Fact]
    public void SmallUnnamedClustersAreHidden_ButNamedOnesAlwaysSurvive()
    {
        using var c = Seed();
        var visible = Visible(c, hideUnknown: false);

        Assert.Contains(1L, visible);                    // 9 faces, over the floor
        Assert.Contains(3L, visible);                    // only 2 faces, but named
        Assert.DoesNotContain(2L, visible);              // 2 faces, unnamed
    }

    [Fact]
    public void ExcludedFacesDoNotCountTowardTheFloor()
    {
        using var c = Seed();
        // Person 5 has 8 face rows, but all are excluded=0-filtered out, so it is
        // below the floor and must not appear — otherwise the grid fills with
        // clusters whose faces the clusterer itself rejected.
        Assert.DoesNotContain(5L, Visible(c, hideUnknown: false));
    }

    [Fact]
    public void HideUnknownStillApplies()
    {
        using var c = Seed();
        Assert.DoesNotContain(4L, Visible(c, hideUnknown: true));
    }

    [Fact]
    public void FloorIsAboveTheMeasuredFragmentBand()
    {
        // 2,271 of 3,108 real clusters held <=5 faces. A floor of <=5 would leave
        // essentially all of them on screen and not solve anything.
        Assert.True(PeopleViewModel.MinFacesPerCluster >= 6,
            "the floor must exclude the <=5-face duplicate-burst band");
    }
}
