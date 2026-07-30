using FileID.Views.Restructure;
using Microsoft.Data.Sqlite;
using Xunit;

namespace FileID.App.Tests;

public class RestructureQualityQueryTests
{
    [Fact]
    public void QualityCounts_AreRootScopedKindAwareAndExcludeFailedRows()
    {
        using var connection = new SqliteConnection("Data Source=:memory:");
        connection.Open();
        Execute(
            connection,
            """
            CREATE TABLE files (
                id INTEGER PRIMARY KEY,
                path_text TEXT NOT NULL,
                kind TEXT NOT NULL,
                failed INTEGER NOT NULL,
                vlm_description TEXT,
                vlm_full_model TEXT
            );
            CREATE TABLE clip_embeddings (file_id INTEGER PRIMARY KEY);
            CREATE TABLE text_embeddings (file_id INTEGER PRIMARY KEY);
            CREATE TABLE doc_text (file_id INTEGER PRIMARY KEY, text TEXT NOT NULL);
            INSERT INTO files VALUES
                (1, 'C:\Library\photo.jpg', 'image', 0, 'A beach', 'qwen'),
                (2, 'C:\Library\Docs\report.pdf', 'pdf', 0, NULL, NULL),
                (3, 'C:\Library\readme.txt', 'other', 0, NULL, NULL),
                (4, 'C:\Library2\outside.jpg', 'image', 0, NULL, NULL),
                (5, 'C:\Library\failed.jpg', 'image', 1, NULL, NULL),
                (6, 'D:\Elsewhere\outside.pdf', 'pdf', 0, NULL, NULL),
                (7, 'C:\Library\Docs\letter.docx', 'doc', 0, NULL, NULL),
                (8, 'C:\Library\Models\part.stl', 'model', 0, NULL, NULL),
                (9, 'C:\Library\Models\scene.obj', 'model', 0, NULL, NULL),
                (10, 'C:\Library\Audio\untagged.mp3', 'audio', 0, NULL, 'qwen'),
                (11, 'C:\Library\Docs\legacy.rtf', 'doc', 0, NULL, NULL),
                (12, 'C:\Library\Code\main.rs', 'doc', 0, NULL, NULL);
            INSERT INTO clip_embeddings VALUES (1), (4), (5), (8), (9);
            INSERT INTO text_embeddings VALUES (2), (6), (7);
            INSERT INTO doc_text VALUES
                (2, 'PDF text'),
                (7, 'Document text'),
                (12, 'fn main() {}');
            """);

        var stats = RestructureView.QueryRestructureQuality(
            connection,
            @"c:\LIBRARY\");

        Assert.True(stats.Available);
        Assert.Equal(4, stats.Total);
        Assert.Equal(2, stats.Captioned);
        Assert.Equal(5, stats.ContentEligible);
        Assert.Equal(2, stats.ClipEmbeddings);
        Assert.Equal(2, stats.TextEmbeddings);
    }

    private static void Execute(SqliteConnection connection, string sql)
    {
        using var command = connection.CreateCommand();
        command.CommandText = sql;
        command.ExecuteNonQuery();
    }
}
