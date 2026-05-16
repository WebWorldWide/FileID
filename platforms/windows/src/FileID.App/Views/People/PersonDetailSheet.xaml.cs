// PersonDetailSheet code-behind. Loads every face for a cluster + its
// JPEG crop, populates the structured-name editor, and on commit fires
// a renamePerson IPC (DB write only — sidecar tags inherit from the
// per-file scan).
//
// V14.4 cut: structured-name editor + face grid; renamePerson IPC is
// added as part of this sheet's wiring (engine handler + DTO).

using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.IO;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.Services;
using Microsoft.Data.Sqlite;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Markup;

namespace FileID.Views.People;

public sealed partial class PersonDetailSheet : UserControl
{
    public sealed class FaceTile
    {
        public required long FaceId { get; init; }
        public required string ImageUri { get; init; }
    }

    private long _personId;
    private readonly ObservableCollection<FaceTile> _faces = new();

    public PersonDetailSheet()
    {
        InitializeComponent();
        FaceGrid.ItemsSource = _faces;
        FaceGrid.ItemTemplate = (DataTemplate)XamlReader.Load("""
            <DataTemplate xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation'
                          xmlns:x='http://schemas.microsoft.com/winfx/2006/xaml'
                          xmlns:p='using:FileID.Views.People'
                          x:DataType='p:PersonDetailSheet+FaceTile'>
              <Border CornerRadius='8'
                      Background='{ThemeResource SubtleFillColorTertiaryBrush}'
                      Width='100' Height='100'>
                <Image Source='{x:Bind ImageUri, Mode=OneTime}' Stretch='UniformToFill' />
              </Border>
            </DataTemplate>
            """);
    }

    public void SetPerson(long personId, string? displayName)
    {
        _personId = personId;
        HeaderText.Text = string.IsNullOrEmpty(displayName) ? $"Person #{personId}" : displayName;
        Load();
    }

    private void Load()
    {
        _faces.Clear();
        try
        {
            var connStr = new SqliteConnectionStringBuilder
            {
                DataSource = AppPaths.DbPath,
                Mode = SqliteOpenMode.ReadOnly,
            }.ToString();
            using var conn = new SqliteConnection(connStr);
            conn.Open();

            // Pull structured name fields.
            using (var cmd = conn.CreateCommand())
            {
                cmd.CommandText = "SELECT title, first_name, middle_name, last_name, suffix, COUNT(fp.id) " +
                                  "FROM persons p LEFT JOIN face_prints fp ON fp.person_id = p.id " +
                                  "WHERE p.id = @id GROUP BY p.id";
                cmd.Parameters.AddWithValue("@id", _personId);
                using var r = cmd.ExecuteReader();
                if (r.Read())
                {
                    TitleBox.Text = r.IsDBNull(0) ? "" : r.GetString(0);
                    FirstBox.Text = r.IsDBNull(1) ? "" : r.GetString(1);
                    MiddleBox.Text = r.IsDBNull(2) ? "" : r.GetString(2);
                    LastBox.Text = r.IsDBNull(3) ? "" : r.GetString(3);
                    SuffixBox.Text = r.IsDBNull(4) ? "" : r.GetString(4);
                    var memberCount = r.GetInt32(5);
                    MemberCountText.Text = $"{memberCount} face{(memberCount == 1 ? "" : "s")} clustered.";
                }
            }

            // Pull every face id for this cluster + check for an on-disk JPEG.
            using (var cmd = conn.CreateCommand())
            {
                cmd.CommandText = "SELECT id FROM face_prints WHERE person_id = @id ORDER BY COALESCE(face_quality, 0) DESC LIMIT 200";
                cmd.Parameters.AddWithValue("@id", _personId);
                using var r = cmd.ExecuteReader();
                while (r.Read())
                {
                    var faceId = r.GetInt64(0);
                    var path = Path.Combine(AppPaths.Root, "face_crops", $"{faceId}.jpg");
                    if (File.Exists(path))
                    {
                        _faces.Add(new FaceTile { FaceId = faceId, ImageUri = new Uri(path).AbsoluteUri });
                    }
                }
            }
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Couldn't load: {ex.Message}";
        }
    }

    public async Task<bool> CommitAsync()
    {
        try
        {
            // Route through the engine's single-writer connection so we
            // don't contend SQLite locks with the engine writer or with a
            // sibling PersonDetailSheet open in another window.
            await ViewModels.EngineClient.Instance.RenamePersonAsync(
                _personId,
                TitleBox.Text,
                FirstBox.Text,
                MiddleBox.Text,
                LastBox.Text,
                SuffixBox.Text);
            return true;
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Save failed: {ex.Message}";
            return false;
        }
    }
}
