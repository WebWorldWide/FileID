// PersonDetailSheet code-behind. Loads every face for a cluster + its
// JPEG crop, populates the structured-name editor, and on commit fires
// a renamePerson IPC (DB write only — sidecar tags inherit from the
// per-file scan).
//
// structured-name editor + face grid; renamePerson IPC is
// added as part of this sheet's wiring (engine handler + DTO).

using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.IO;
using System.Linq;
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
        public string FaceLabel => $"Face {FaceId}";
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
                      AutomationProperties.Name='{x:Bind FaceLabel}'
                      Width='100' Height='100'>
                <Image Source='{x:Bind ImageUri, Mode=OneTime}' Stretch='UniformToFill' />
              </Border>
            </DataTemplate>
            """);
    }

    private sealed class LoadResult
    {
        public string Title = "";
        public string First = "";
        public string Middle = "";
        public string Last = "";
        public string Suffix = "";
        public int MemberCount;
        public bool Found;
        public List<FaceTile> Faces = new();
        public string? Error;
    }

    public void SetPerson(long personId, string? displayName)
    {
        _personId = personId;
        HeaderText.Text = string.IsNullOrEmpty(displayName) ? $"Person #{personId}" : displayName;
        Load();
    }

    private async void Load()
        => await DebugLog.SafeRunAsync(nameof(Load), async () =>
    {
        _faces.Clear();
        var dispatcher = DispatcherQueue;
        long personId = _personId;

        // The SqliteConnection open + structured-name read + up to 200
        // File.Exists probes used to run synchronously on the UI thread before
        // the dialog showed, stalling its open on a cold disk. Do all of it on
        // a worker thread and marshal the UI writes back.
        LoadResult result = await Task.Run(() =>
        {
            var res = new LoadResult();
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
                    cmd.Parameters.AddWithValue("@id", personId);
                    using var r = cmd.ExecuteReader();
                    if (r.Read())
                    {
                        res.Found = true;
                        res.Title = r.IsDBNull(0) ? "" : r.GetString(0);
                        res.First = r.IsDBNull(1) ? "" : r.GetString(1);
                        res.Middle = r.IsDBNull(2) ? "" : r.GetString(2);
                        res.Last = r.IsDBNull(3) ? "" : r.GetString(3);
                        res.Suffix = r.IsDBNull(4) ? "" : r.GetString(4);
                        res.MemberCount = r.GetInt32(5);
                    }
                }

                // Pull every face id for this cluster + check for an on-disk JPEG.
                using (var cmd = conn.CreateCommand())
                {
                    cmd.CommandText = "SELECT id FROM face_prints WHERE person_id = @id ORDER BY COALESCE(face_quality, 0) DESC LIMIT 200";
                    cmd.Parameters.AddWithValue("@id", personId);
                    using var r = cmd.ExecuteReader();
                    while (r.Read())
                    {
                        var faceId = r.GetInt64(0);
                        var path = Path.Combine(AppPaths.Root, "face_crops", $"{faceId}.jpg");
                        if (File.Exists(path))
                        {
                            res.Faces.Add(new FaceTile { FaceId = faceId, ImageUri = new Uri(path).AbsoluteUri });
                        }
                    }
                }
            }
            catch (Exception ex)
            {
                res.Error = ex.Message;
            }
            return res;
        }).ConfigureAwait(true);

        void Apply()
        {
            if (result.Error is not null)
            {
                StatusText.Text = $"Couldn't load: {result.Error}";
                return;
            }
            // Guard against a stale load landing after the sheet was re-pointed at
            // a different person — skip ALL UI writes (name boxes + face grid), not
            // just the grid, so a slow prior load can't overwrite the new person.
            if (_personId != personId) return;
            if (result.Found)
            {
                TitleBox.Text = result.Title;
                FirstBox.Text = result.First;
                MiddleBox.Text = result.Middle;
                LastBox.Text = result.Last;
                SuffixBox.Text = result.Suffix;
                MemberCountText.Text = $"{result.MemberCount} face{(result.MemberCount == 1 ? "" : "s")} clustered.";
            }
            _faces.Clear();
            foreach (var f in result.Faces) _faces.Add(f);
        }

        // ConfigureAwait(true) resumes on the captured UI context; the
        // DispatcherQueue post is belt-and-suspenders in case the continuation
        // resumes on a worker thread (no captured SyncContext).
        if (dispatcher is null || dispatcher.HasThreadAccess)
        {
            Apply();
        }
        else
        {
            dispatcher.TryEnqueue(Apply);
        }
    });

    public async Task<bool> CommitAsync()
    {
        try
        {
            // Await the engine's BulkActionResult instead of fire-and-forget:
            // renamePerson reports failure in the result (e.g. the row update
            // didn't take), not as a thrown exception, so declaring success on
            // the IPC send alone left the dialog closing on a failed save (the
            // silent-failure class). Route through the engine's single-writer
            // connection so we don't contend SQLite locks with the engine
            // writer or a sibling sheet in another window.
            var result = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                "renamePerson",
                () => ViewModels.EngineClient.Instance.RenamePersonAsync(
                    _personId,
                    TitleBox.Text,
                    FirstBox.Text,
                    MiddleBox.Text,
                    LastBox.Text,
                    SuffixBox.Text),
                TimeSpan.FromSeconds(30));
            if (result.Failed > 0 || result.Succeeded == 0)
            {
                var first = result.Messages?.FirstOrDefault(m => !m.Ok)?.Message;
                StatusText.Text = string.IsNullOrWhiteSpace(first) ? "Save failed." : $"Save failed: {first}";
                return false;
            }
            return true;
        }
        catch (TimeoutException ex)
        {
            StatusText.Text = "Save didn't confirm — try again.";
            Services.DebugLog.Warn("PersonDetailSheet.CommitAsync timed out: " + ex.Message);
            return false;
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Save failed: {ex.Message}";
            return false;
        }
    }
}
