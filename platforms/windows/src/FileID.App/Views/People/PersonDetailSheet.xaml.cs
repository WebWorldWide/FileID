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
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Markup;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Views.People;

public sealed partial class PersonDetailSheet : UserControl
{
    public sealed class FaceTile
    {
        public required long FaceId { get; init; }
        public required string ImageUri { get; init; }
        public string FaceLabel => $"Face {FaceId}";
    }

    internal const string PersonQuery = """
        SELECT p.title, p.first_name, p.middle_name, p.last_name, p.suffix,
               COUNT(f.id), COALESCE(p.is_unknown, 0)
        FROM persons p
        LEFT JOIN face_prints fp ON fp.person_id = p.id AND COALESCE(fp.excluded, 0) = 0
        LEFT JOIN files f ON f.id = fp.file_id AND f.failed = 0
        WHERE p.id = @id GROUP BY p.id
        """;

    internal const string FacesQuery = """
        SELECT fp.id FROM face_prints fp JOIN files f ON f.id = fp.file_id
        WHERE fp.person_id = @id AND COALESCE(fp.excluded, 0) = 0 AND f.failed = 0
        ORDER BY COALESCE(fp.face_quality, 0) DESC, fp.id ASC LIMIT 200
        """;

    private long _personId;
    private bool _selectMode;
    private bool _moving;
    private readonly HashSet<long> _checked = new();
    private readonly ObservableCollection<FaceTile> _faces = new();

    public PersonDetailSheet()
    {
        InitializeComponent();
        FaceGrid.ItemsSource = _faces;
        ReviewPanel.TotalChanged += OnReviewTotalChanged;
        FaceGrid.ElementPrepared += OnTilePrepared;
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
        public bool IsUnknown;
        public bool Found;
        public List<FaceTile> Faces = new();
        public string? Error;
    }

    public void SetPerson(long personId, string? displayName)
    {
        _personId = personId;
        HeaderText.Text = string.IsNullOrEmpty(displayName) ? $"Person #{personId}" : displayName;
        Load();
        ReviewPanel.SetPerson(personId, displayName);
        _ = ReviewPanel.ReloadAsync();
    }

    private void OnReviewTotalChanged(object? sender, long total)
        => ReviewPanel.Visibility = total > 0 ? Visibility.Visible : Visibility.Collapsed;

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
                    cmd.CommandText = PersonQuery;
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
                        res.IsUnknown = r.GetInt32(6) != 0;
                    }
                }

                // Pull every face id for this cluster + check for an on-disk JPEG.
                using (var cmd = conn.CreateCommand())
                {
                    cmd.CommandText = FacesQuery;
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
                IsUnknownCheckBox.IsChecked = result.IsUnknown;
                MemberCountText.Text = $"{result.MemberCount} face{(result.MemberCount == 1 ? "" : "s")} clustered.";
            }
            else
            {
                StatusText.Text = "Person record not found or was merged into another cluster.";
            }
            _faces.Clear();
            foreach (var f in result.Faces) _faces.Add(f);
            _checked.IntersectWith(result.Faces.Select(f => f.FaceId));
            SyncMoveToolbar();
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

    internal const string MoveTargetsQuery = """
        SELECT p.id, COALESCE(p.name, p.first_name, 'Person ' || p.id), COUNT(fp.id),
               COALESCE((SELECT fp2.id FROM face_prints fp2 JOIN files f2 ON f2.id = fp2.file_id
                         WHERE fp2.person_id = p.id AND COALESCE(fp2.excluded, 0) = 0 AND f2.failed = 0
                         ORDER BY COALESCE(fp2.face_quality, 0) DESC, fp2.id ASC LIMIT 1), 0)
        FROM persons p
        JOIN face_prints fp ON fp.person_id = p.id AND COALESCE(fp.excluded, 0) = 0
        JOIN files f ON f.id = fp.file_id AND f.failed = 0
        WHERE p.id <> @id AND COALESCE(p.is_unknown, 0) = 0
        GROUP BY p.id
        ORDER BY COUNT(fp.id) DESC, p.id ASC
        """;

    private sealed record MoveTarget(long Id, string Name, int Count, long AnchorFaceId);

    private void SyncMoveToolbar()
    {
        SelectModeButton.Visibility = !_selectMode && _faces.Count >= 2 ? Visibility.Visible : Visibility.Collapsed;
        var selecting = _selectMode ? Visibility.Visible : Visibility.Collapsed;
        SelectedCountText.Visibility = selecting;
        MoveButton.Visibility = selecting;
        CancelSelectButton.Visibility = selecting;
        SelectedCountText.Text = $"{_checked.Count} selected";
        MoveButton.IsEnabled = _checked.Count > 0 && !_moving;
    }

    private void OnTilePrepared(ItemsRepeater sender, ItemsRepeaterElementPreparedEventArgs args)
    {
        if (args.Element is not Border tile || args.Index < 0 || args.Index >= _faces.Count) return;
        tile.Tag = _faces[args.Index].FaceId;
        tile.Tapped -= OnTileTapped;
        tile.Tapped += OnTileTapped;
        StyleTile(tile);
    }

    private void StyleTile(Border tile)
    {
        var on = tile.Tag is long id && _checked.Contains(id);
        tile.BorderThickness = new Thickness(on ? 2 : 0);
        tile.BorderBrush = on ? ThemeHelper.GetBrushSafe("GoldBrush") : null;
        tile.Opacity = _selectMode && !on ? 0.75 : 1;
    }

    private void RestyleTiles()
    {
        for (var i = 0; i < _faces.Count; i++)
        {
            if (FaceGrid.TryGetElement(i) is Border tile) StyleTile(tile);
        }
    }

    private void OnTileTapped(object sender, TappedRoutedEventArgs e)
    {
        if (!_selectMode || _moving || sender is not Border { Tag: long id } tile) return;
        if (!_checked.Add(id)) _checked.Remove(id);
        StyleTile(tile);
        SyncMoveToolbar();
    }

    private void OnSelectModeClicked(object sender, RoutedEventArgs e)
    {
        _selectMode = true;
        _checked.Clear();
        StatusText.Text = "";
        SyncMoveToolbar();
        RestyleTiles();
    }

    private void OnCancelSelectClicked(object sender, RoutedEventArgs e) => EndSelectMode();

    private void EndSelectMode()
    {
        _selectMode = false;
        _checked.Clear();
        SyncMoveToolbar();
        RestyleTiles();
    }

    private async void OnMoveClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnMoveClicked), async () =>
    {
        if (_checked.Count == 0 || _moving) return;
        var sourceId = _personId;
        List<MoveTarget> targets;
        try
        {
            targets = await Task.Run(() => LoadMoveTargets(sourceId)).ConfigureAwait(true);
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Couldn't load people: {ex.Message}";
            return;
        }
        if (targets.Count == 0)
        {
            StatusText.Text = "There's nobody else to move these photos to yet.";
            return;
        }

        var count = _checked.Count;
        var list = new ListView { SelectionMode = ListViewSelectionMode.None, IsItemClickEnabled = true, MaxHeight = 360, MinWidth = 300 };
        foreach (var target in targets) list.Items.Add(BuildTargetRow(target));
        var flyout = new Flyout
        {
            Placement = Microsoft.UI.Xaml.Controls.Primitives.FlyoutPlacementMode.Bottom,
            Content = new StackPanel
            {
                Spacing = 8,
                Children =
                {
                    new TextBlock { Text = $"Move {count} photo{(count == 1 ? "" : "s")} to…", Style = ThemeHelper.GetStyleSafe("BodyStrongTextBlockStyle") },
                    new TextBlock
                    {
                        Text = "Pick the person these photos actually show. Only the selected photos move; the rest of this person's group stays put.",
                        Style = ThemeHelper.GetStyleSafe("CaptionTextBlockStyle"),
                        TextWrapping = TextWrapping.Wrap,
                        MaxWidth = 300,
                    },
                    list,
                },
            },
        };
        list.ItemClick += async (_, args) =>
        {
            flyout.Hide();
            if (args.ClickedItem is FrameworkElement { Tag: MoveTarget chosen })
            {
                await DebugLog.SafeRunAsync("MoveFacesTo", () => MoveFacesAsync(sourceId, chosen));
            }
        };
        flyout.ShowAt(MoveButton);
    });

    private static List<MoveTarget> LoadMoveTargets(long excludePersonId)
    {
        var rows = new List<MoveTarget>();
        if (!File.Exists(AppPaths.DbPath)) return rows;
        var connStr = new SqliteConnectionStringBuilder { DataSource = AppPaths.DbPath, Mode = SqliteOpenMode.ReadOnly }.ToString();
        using var conn = new SqliteConnection(connStr);
        conn.Open();
        using var cmd = conn.CreateCommand();
        cmd.CommandText = MoveTargetsQuery;
        cmd.Parameters.AddWithValue("@id", excludePersonId);
        using var r = cmd.ExecuteReader();
        while (r.Read())
        {
            rows.Add(new MoveTarget(r.GetInt64(0), r.IsDBNull(1) ? $"Person {r.GetInt64(0)}" : r.GetString(1), r.GetInt32(2), r.GetInt64(3)));
        }
        return rows;
    }

    private static FrameworkElement BuildTargetRow(MoveTarget target)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10, Padding = new Thickness(0, 4, 0, 4), Tag = target };
        var crop = Path.Combine(AppPaths.Root, "face_crops", $"{target.AnchorFaceId}.jpg");
        var photo = new Border { Width = 40, Height = 40, CornerRadius = new CornerRadius(20), Background = ThemeHelper.GetBrushSafe("SubtleFillColorTertiaryBrush") };
        if (File.Exists(crop))
        {
            photo.Child = new Image { Source = new BitmapImage { UriSource = new Uri(crop), DecodePixelWidth = 80 }, Stretch = Stretch.UniformToFill };
        }
        row.Children.Add(photo);
        var text = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        text.Children.Add(new TextBlock { Text = target.Name, TextTrimming = TextTrimming.CharacterEllipsis, MaxWidth = 200 });
        text.Children.Add(new TextBlock
        {
            Text = $"{target.Count} photo{(target.Count == 1 ? "" : "s")}",
            Style = ThemeHelper.GetStyleSafe("CaptionTextBlockStyle"),
            Foreground = ThemeHelper.GetBrushSafe("Text3Brush"),
        });
        row.Children.Add(text);
        AutomationProperties.SetName(row, $"Move to {target.Name}, {target.Count} photo{(target.Count == 1 ? "" : "s")}");
        return row;
    }

    private async Task MoveFacesAsync(long sourceId, MoveTarget target)
    {
        if (_moving || sourceId != _personId) return;
        var faceIds = _checked.OrderBy(id => id).ToArray();
        _moving = true;
        SyncMoveToolbar();
        StatusText.Text = $"Moving {faceIds.Length} photo{(faceIds.Length == 1 ? "" : "s")}…";
        var moved = 0;
        string? problem = null;
        try
        {
            foreach (var faceId in faceIds)
            {
                var result = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                    "reassignFace",
                    () => ViewModels.EngineClient.Instance.ReassignFaceAsync(faceId, target.Id),
                    TimeSpan.FromSeconds(30));
                if (result.Succeeded == 1) moved++;
                else if (result.Messages?.FirstOrDefault(m => !m.Ok)?.Message is { Length: > 0 } message) problem = message;
            }
        }
        catch (Exception ex)
        {
            problem = ex is TimeoutException ? "The move didn't confirm. Try again." : ex.Message;
            Services.DebugLog.Warn("PersonDetailSheet.MoveFaces failed: " + ex.Message);
        }
        finally
        {
            _moving = false;
        }
        StatusText.Text = moved > 0
            ? $"Moved {moved} photo{(moved == 1 ? "" : "s")} to {target.Name}." + (problem is null ? "" : $" {problem}")
            : problem ?? "Nothing moved.";
        EndSelectMode();
        Load();
    }

    // Read-only existence probe for the rename pre-write guard. persons.id is
    // INTEGER PRIMARY KEY AUTOINCREMENT, so an id is never reused — a present
    // row is proof it's still the same person we loaded. Throws on a genuine
    // read error so the caller can distinguish "definitely gone" from
    // "couldn't check" and avoid blocking a legit rename on a transient fault.
    internal static bool PersonExists(string dbPath, long personId)
    {
        if (!File.Exists(dbPath)) return false;
        var connStr = new SqliteConnectionStringBuilder
        {
            DataSource = dbPath,
            Mode = SqliteOpenMode.ReadOnly,
        }.ToString();
        using var conn = new SqliteConnection(connStr);
        conn.Open();
        using var cmd = conn.CreateCommand();
        cmd.CommandText = "SELECT 1 FROM persons WHERE id = @id LIMIT 1";
        cmd.Parameters.AddWithValue("@id", personId);
        using var r = cmd.ExecuteReader();
        return r.Read();
    }

    public async Task<bool> CommitAsync()
    {
        // Capture the target person id at commit start and write by that id. A
        // background re-cluster can merge this person away while the sheet is
        // open; because persons.id is AUTOINCREMENT the id is never reassigned,
        // so a missing row means the person is genuinely gone (not a different
        // person). The engine's renamePerson reports succeeded=1 even on a
        // 0-row UPDATE, so without this pre-write existence check the dialog
        // would close on a phantom save.
        long personId = _personId;
        StatusText.Text = "Saving...";
        try
        {
            bool gone;
            try
            {
                gone = !await Task.Run(() => PersonExists(AppPaths.DbPath, personId)).ConfigureAwait(true);
            }
            catch (Exception ex)
            {
                // Couldn't verify (transient read error) — don't block the rename
                // on a check we couldn't run; fall through to the IPC.
                Services.DebugLog.Warn("PersonDetailSheet existence check failed: " + ex.Message);
                gone = false;
            }
            if (gone)
            {
                StatusText.Text = "This person no longer exists — it may have been merged. Reopen People and try again.";
                return false;
            }

            if (IsUnknownCheckBox.IsChecked == true)
            {
                var unknownResult = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                    "markPersonsAsUnknown",
                    () => ViewModels.EngineClient.Instance.MarkPersonsAsUnknownAsync(new[] { personId }),
                    TimeSpan.FromSeconds(30));
                if (unknownResult.Failed > 0 || unknownResult.Succeeded == 0)
                {
                    var first = unknownResult.Messages?.FirstOrDefault(m => !m.Ok)?.Message;
                    StatusText.Text = string.IsNullOrWhiteSpace(first) ? "Save failed." : $"Save failed: {first}";
                    return false;
                }
                return true;
            }

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
                    personId,
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
