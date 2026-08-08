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
using FileID.ViewModels;
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
    private int _fileCount;
    private int _tagInFlight;

    /// Most face crops rendered at once. A chained cluster can hold tens of
    /// thousands of faces (26,422 in the worst measured case); decoding that many
    /// JPEGs would wedge the UI thread. Whatever this hides is disclosed in the
    /// header rather than silently dropped.
    private const int FacePreviewCap = 200;

    public PersonDetailSheet()
    {
        InitializeComponent();
        FaceGrid.ItemsSource = _faces;
        FaceGrid.ElementPrepared += OnFaceGridElementPrepared;
        FaceGrid.ElementClearing += OnFaceGridElementClearing;
        FaceGrid.ItemTemplate = (DataTemplate)XamlReader.Load("""
            <DataTemplate xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation'
                          xmlns:x='http://schemas.microsoft.com/winfx/2006/xaml'>
              <Border CornerRadius='8'
                      Background='{ThemeResource SubtleFillColorTertiaryBrush}'
                      AutomationProperties.Name='{Binding FaceLabel}'
                      Width='104' Height='104'
                      ToolTipService.ToolTip='Right-click to remove, split, or move this face'>
                <Image Source='{Binding ImageUri}' Stretch='UniformToFill' />
              </Border>
            </DataTemplate>
            """);
    }

    private void OnFaceGridElementPrepared(ItemsRepeater sender, ItemsRepeaterElementPreparedEventArgs args)
    {
        // ItemsRepeater with x:Bind compiled templates does NOT set DataContext on
        // the realized element — DataContext is always null here. Use the index to
        // fetch the tile from the backing collection directly.
        if (args.Element is not FrameworkElement el) return;
        var tile = (args.Index >= 0 && args.Index < _faces.Count) ? _faces[args.Index] : null;
        if (tile is null) return;

        el.DataContext = tile;

        var flyout = new MenuFlyout();

        var removeItem = new MenuFlyoutItem { Text = "Remove from this person", Tag = tile.FaceId };
        removeItem.Click += async (s, e) => await DebugLog.SafeRunAsync(nameof(RemoveFaceFromPersonAsync), async () => await RemoveFaceFromPersonAsync(tile.FaceId));
        flyout.Items.Add(removeItem);

        var splitItem = new MenuFlyoutItem { Text = "Split into new person", Tag = tile.FaceId };
        splitItem.Click += async (s, e) => await DebugLog.SafeRunAsync(nameof(SplitFaceToNewPersonAsync), async () => await SplitFaceToNewPersonAsync(tile.FaceId));
        flyout.Items.Add(splitItem);

        var moveItem = new MenuFlyoutItem { Text = "Move to another person...", Tag = tile.FaceId };
        moveItem.Click += async (s, e) => await DebugLog.SafeRunAsync(nameof(MoveFaceToPersonAsync), async () => await MoveFaceToPersonAsync(tile.FaceId, s as FrameworkElement));
        flyout.Items.Add(moveItem);

        el.ContextFlyout = flyout;
    }

    private void OnFaceGridElementClearing(ItemsRepeater sender, ItemsRepeaterElementClearingEventArgs args)
    {
        if (args.Element is FrameworkElement el)
        {
            el.ContextFlyout = null;
            el.DataContext = null;
        }
    }

    private async Task RemoveFaceFromPersonAsync(long faceId)
    {
        try
        {
            var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                "reassignFace",
                () => EngineClient.Instance.ReassignFaceAsync(faceId),
                TimeSpan.FromSeconds(30));
            EnsureFaceMutationSucceeded(result, faceId);

            var tile = _faces.FirstOrDefault(f => f.FaceId == faceId);
            if (tile != null) _faces.Remove(tile);
            StatusText.Text = $"Removed Face #{faceId} from this person.";
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"RemoveFaceFromPersonAsync failed: {ex.Message}");
            StatusText.Text = $"Failed to remove face: {ex.Message}";
        }
    }

    private async Task SplitFaceToNewPersonAsync(long faceId)
    {
        try
        {
            var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                "reassignFace",
                () => EngineClient.Instance.ReassignFaceAsync(faceId, createNewPerson: true),
                TimeSpan.FromSeconds(30));
            EnsureFaceMutationSucceeded(result, faceId);

            var tile = _faces.FirstOrDefault(f => f.FaceId == faceId);
            if (tile != null) _faces.Remove(tile);
            StatusText.Text = $"Split Face #{faceId} into a new person.";
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"SplitFaceToNewPersonAsync failed: {ex.Message}");
            StatusText.Text = $"Failed to split face: {ex.Message}";
        }
    }

    private sealed class PersonPickerItem
    {
        public required long PersonId { get; init; }
        public required string DisplayName { get; init; }
        public override string ToString() => DisplayName;
    }

    private async Task MoveFaceToPersonAsync(long faceId, FrameworkElement? targetElement)
    {
        long currentPersonId = _personId;
        var personsList = await Task.Run(() =>
        {
            var list = new List<PersonPickerItem>();
            var connStr = new SqliteConnectionStringBuilder
            {
                DataSource = AppPaths.DbPath,
                Mode = SqliteOpenMode.ReadOnly,
                DefaultTimeout = 5
            }.ToString();
            using var conn = new SqliteConnection(connStr);
            conn.Open();
            using var cmd = conn.CreateCommand();
            cmd.CommandText = "SELECT p.id, COALESCE(NULLIF(TRIM(COALESCE(p.title, '') || ' ' || COALESCE(p.first_name, '') || ' ' || COALESCE(p.middle_name, '') || ' ' || COALESCE(p.last_name, '') || ' ' || COALESCE(p.suffix, '')), ''), NULLIF(TRIM(p.name), ''), 'Person ' || p.id) FROM persons p WHERE p.id != @currentId ORDER BY p.id DESC LIMIT 200";
            cmd.Parameters.AddWithValue("@currentId", currentPersonId);
            using var r = cmd.ExecuteReader();
            while (r.Read())
            {
                list.Add(new PersonPickerItem
                {
                    PersonId = r.GetInt64(0),
                    DisplayName = r.GetString(1)
                });
            }
            return list;
        });

        if (personsList.Count == 0)
        {
            StatusText.Text = "No other persons available to move face to.";
            return;
        }

        var comboBox = new ComboBox
        {
            ItemsSource = personsList,
            SelectedIndex = 0,
            HorizontalAlignment = HorizontalAlignment.Stretch
        };

        var moveBtn = new Button
        {
            Content = "Move",
            Style = (Style)Application.Current.Resources["AccentButtonStyle"],
            HorizontalAlignment = HorizontalAlignment.Right
        };

        var flyout = new Flyout
        {
            Content = new StackPanel
            {
                Width = 260,
                Spacing = 10,
                Children =
                {
                    new TextBlock { Text = $"Move Face #{faceId}", Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"] },
                    new TextBlock { Text = "Select target person:", Style = (Style)Application.Current.Resources["BodyTextBlockStyle"] },
                    comboBox,
                    moveBtn
                }
            }
        };

        moveBtn.Click += async (_, _) =>
        {
            flyout.Hide();
            if (comboBox.SelectedItem is PersonPickerItem selected)
            {
                await PerformMoveFaceAsync(faceId, selected);
            }
        };

        if (targetElement != null)
        {
            flyout.ShowAt(targetElement);
        }
        else
        {
            flyout.ShowAt(this);
        }
    }

    private async Task PerformMoveFaceAsync(long faceId, PersonPickerItem selected)
    {
        try
        {
            var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                "reassignFace",
                () => EngineClient.Instance.ReassignFaceAsync(faceId, selected.PersonId),
                TimeSpan.FromSeconds(30));
            EnsureFaceMutationSucceeded(result, faceId);

            var tile = _faces.FirstOrDefault(f => f.FaceId == faceId);
            if (tile != null) _faces.Remove(tile);
            StatusText.Text = $"Moved Face #{faceId} to {selected.DisplayName}.";
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"PerformMoveFaceAsync failed: {ex.Message}");
            StatusText.Text = $"Failed to move face: {ex.Message}";
        }
    }

    private static void EnsureFaceMutationSucceeded(BulkActionResult result, long faceId)
    {
        if (result.Failed == 0 && result.Succeeded > 0) return;
        string? detail = null;
        foreach (var message in result.Messages)
        {
            if (message is not null && !message.Ok)
            {
                detail = message.Message;
                break;
            }
        }
        detail ??= result.Messages.Count > 0 ? result.Messages[0]?.Message : null;
        detail ??= $"Face #{faceId} was not changed.";
        throw new InvalidOperationException(detail);
    }

    private sealed class LoadResult
    {
        public string Title = "";
        public string First = "";
        public string Middle = "";
        public string Last = "";
        public string Suffix = "";
        public bool IsUnknown;
        public int MemberCount;
        public int FileCount;
        public bool Found;
        public List<FaceTile> Faces = new();
        public string? Error;
    }

    public void SetPerson(long personId, string? displayName)
    {
        _personId = personId;
        _fileCount = 0;
        HeaderText.Text = string.IsNullOrEmpty(displayName) ? $"Person #{personId}" : displayName;
        TagAllPhotosStatus.Visibility = Visibility.Collapsed;
        SyncTagAllPhotosControls();
        Load();
    }

    private void OnUnknownChecked(object sender, RoutedEventArgs e)
    {
        NameFieldsPanel.Visibility = Visibility.Collapsed;
        SyncTagAllPhotosControls();
    }

    private void OnUnknownUnchecked(object sender, RoutedEventArgs e)
    {
        NameFieldsPanel.Visibility = Visibility.Visible;
        SyncTagAllPhotosControls();
    }

    private void OnNameFieldChanged(object sender, TextChangedEventArgs e)
        => SyncTagAllPhotosControls();

    private string CurrentPersonTagName() => ReadStore.FormatPersonTagName(
        TitleBox.Text,
        FirstBox.Text,
        MiddleBox.Text,
        LastBox.Text,
        SuffixBox.Text,
        null);

    private string? PreviousPersonTagIfDifferent(string currentTag)
    {
        if (_personId <= 0 || currentTag.Length == 0) return null;
        var previous = AppViewModel.Instance.Settings.LastPersonTag(_personId);
        return !string.IsNullOrWhiteSpace(previous)
               && !string.Equals(previous, currentTag, StringComparison.OrdinalIgnoreCase)
            ? previous
            : null;
    }

    private void SyncTagAllPhotosControls()
    {
        if (TagPersonPanel is null
            || TagAllPhotosButton is null
            || ReplacePersonTagButton is null)
        {
            return;
        }
        var unknown = IsUnknownCheckBox?.IsChecked == true;
        var name = CurrentPersonTagName();
        var previousTag = PreviousPersonTagIfDifferent(name);
        var busy = System.Threading.Volatile.Read(ref _tagInFlight) != 0;
        TagPersonPanel.Visibility = !unknown && _fileCount > 0
            ? Visibility.Visible
            : Visibility.Collapsed;
        TagAllPhotosButton.IsEnabled = !busy && name.Length > 0;
        TagAllPhotosButtonText.Text = name.Length == 0
            ? "Enter a name to tag these photos"
            : busy
                ? $"Tagging {_fileCount:N0} photo{(_fileCount == 1 ? "" : "s")}…"
                : $"Tag all {_fileCount:N0} photo{(_fileCount == 1 ? "" : "s")} with “{name}”";
        ReplacePersonTagButton.Visibility = previousTag is null
            ? Visibility.Collapsed
            : Visibility.Visible;
        ReplacePersonTagButton.IsEnabled = !busy && previousTag is not null;
        if (previousTag is not null)
        {
            ReplacePersonTagButtonText.Text = $"Replace “{previousTag}” with “{name}”";
            ToolTipService.SetToolTip(
                ReplacePersonTagButton,
                $"Removes only the old person tag “{previousTag}” and adds “{name}”.");
        }
        TagAllPhotosProgress.IsActive = busy;
        TagAllPhotosProgress.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
    }

    private async void OnTagAllPhotosClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnTagAllPhotosClicked), TagAllPhotosAsync);

    private async void OnReplacePersonTagClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(
            nameof(OnReplacePersonTagClicked),
            () => ApplyPersonTagAsync(replacePrevious: true));

    private Task TagAllPhotosAsync() => ApplyPersonTagAsync(replacePrevious: false);

    private async Task ApplyPersonTagAsync(bool replacePrevious)
    {
        if (System.Threading.Interlocked.CompareExchange(ref _tagInFlight, 1, 0) != 0) return;
        SyncTagAllPhotosControls();
        TagAllPhotosStatus.Visibility = Visibility.Collapsed;
        var personId = _personId;
        var tag = CurrentPersonTagName();
        var previousTag = replacePrevious ? PreviousPersonTagIfDifferent(tag) : null;
        IReadOnlyDictionary<long, List<string>>? priorTags = null;
        var confirmed = new HashSet<long>();
        var undoRegistered = false;
        try
        {
            if (tag.Length == 0 || (replacePrevious && previousTag is null)) return;
            await using var store = new ReadStore(AppPaths.DbPath);
            await store.OpenAsync();
            var fileIds = await store.PersonFileIdsAsync(personId, default);
            if (_personId != personId) return;
            if (fileIds.Count == 0)
            {
                TagAllPhotosStatus.Text = "No indexed photos currently belong to this person.";
                TagAllPhotosStatus.Visibility = Visibility.Visible;
                return;
            }

            priorTags = await TagChangeJournal.CapturePriorUserTagsAsync(fileIds);
            uint reportedFailed = 0;
            string? firstFailure = null;
            void Accumulate(BulkActionResult result, IReadOnlyList<long> expected)
            {
                reportedFailed += result.Failed;
                firstFailure ??= result.Messages
                    .FirstOrDefault(message => message is not null && !message.Ok)
                    ?.Message;
                foreach (var fileId in BulkActionResultTruth
                             .ConfirmedSuccessfulFileIds(result, expected))
                {
                    confirmed.Add(fileId);
                }
            }

            if (previousTag is null)
            {
                var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                    "applyTags",
                    () => EngineClient.Instance.ApplyTagsAsync(fileIds, new[] { tag }, "add"),
                    BulkActionTimeout.ForFileCount(fileIds.Count));
                Accumulate(result, fileIds);
            }
            else
            {
                var groups = TagChangeJournal.BuildScopedReplacementGroups(
                    fileIds,
                    priorTags,
                    previousTag,
                    tag);
                foreach (var group in groups)
                {
                    var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                        "applyTags",
                        () => EngineClient.Instance.ApplyTagsAsync(group.Ids, group.Tags, "replace"),
                        BulkActionTimeout.ForFileCount(group.Ids.Count));
                    Accumulate(result, group.Ids);
                }
            }

            var expectedCount = fileIds.Distinct().Count();
            var failed = Math.Max((long)reportedFailed, expectedCount - confirmed.Count);
            var complete = failed == 0 && confirmed.Count == expectedCount;
            var historyPersisted = true;
            if (confirmed.Count > 0)
            {
                TagChangeJournal.PushUndo(
                    TagChangeJournal.FormatLabel(
                        previousTag is null ? "add" : "replace",
                        confirmed.Count),
                    confirmed.OrderBy(id => id).ToArray(),
                    priorTags);
                undoRegistered = true;
            }
            if (complete)
            {
                var settings = AppViewModel.Instance.Settings;
                settings.RecordPersonTag(personId, tag);
                historyPersisted = await settings.SaveImmediatelyAsync();
            }
            if (_personId != personId) return;
            if (!complete)
            {
                TagAllPhotosStatus.Text =
                    $"Updated {confirmed.Count:N0}; {failed:N0} failed"
                    + (string.IsNullOrWhiteSpace(firstFailure) ? "." : $" — {firstFailure}");
            }
            else if (previousTag is null)
            {
                TagAllPhotosStatus.Text =
                    $"Tagged {confirmed.Count:N0} photo{(confirmed.Count == 1 ? "" : "s")} with “{tag}”.";
            }
            else
            {
                TagAllPhotosStatus.Text =
                    $"Replaced “{previousTag}” with “{tag}” on {confirmed.Count:N0} photo{(confirmed.Count == 1 ? "" : "s")}.";
            }
            if (!historyPersisted)
            {
                TagAllPhotosStatus.Text +=
                    " The photo tags were applied, but FileID couldn't save the rename history; check settings-folder permissions.";
            }
            TagAllPhotosStatus.Visibility = Visibility.Visible;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("TagAllPhotosAsync failed: " + ex.Message);
            TagAllPhotosStatus.Text = "Couldn't tag these photos: " + ex.Message;
            TagAllPhotosStatus.Visibility = Visibility.Visible;
        }
        finally
        {
            if (!undoRegistered && priorTags is not null && confirmed.Count > 0)
            {
                TagChangeJournal.PushUndo(
                    TagChangeJournal.FormatLabel(
                        previousTag is null ? "add" : "replace",
                        confirmed.Count),
                    confirmed.OrderBy(id => id).ToArray(),
                    priorTags);
            }
            System.Threading.Interlocked.Exchange(ref _tagInFlight, 0);
            SyncTagAllPhotosControls();
        }
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

                // Pull structured name fields + legacy name + is_unknown flag.
                using (var cmd = conn.CreateCommand())
                {
                    cmd.CommandText = "SELECT title, first_name, middle_name, last_name, suffix, COUNT(fp.id), COUNT(DISTINCT fp.file_id), COALESCE(p.is_unknown, 0), p.name " +
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
                        res.FileCount = r.GetInt32(6);
                        res.IsUnknown = r.GetInt32(7) != 0;
                        var rawName = r.IsDBNull(8) ? "" : r.GetString(8);
                        if (string.IsNullOrWhiteSpace(res.First) && !string.IsNullOrWhiteSpace(rawName) && !rawName.StartsWith("Person ", StringComparison.OrdinalIgnoreCase))
                        {
                            res.First = rawName;
                        }
                    }
                }

                // Pull every face id for this cluster + check for an on-disk JPEG.
                using (var cmd = conn.CreateCommand())
                {
                    cmd.CommandText = "SELECT id FROM face_prints WHERE person_id = @id ORDER BY COALESCE(face_quality, 0) DESC LIMIT " + FacePreviewCap.ToString(System.Globalization.CultureInfo.InvariantCulture);
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
            if (_personId != personId) return;
            if (result.Found)
            {
                TitleBox.Text = result.Title;
                FirstBox.Text = result.First;
                MiddleBox.Text = result.Middle;
                LastBox.Text = result.Last;
                SuffixBox.Text = result.Suffix;
                IsUnknownCheckBox.IsChecked = result.IsUnknown;
                NameFieldsPanel.Visibility = result.IsUnknown ? Visibility.Collapsed : Visibility.Visible;
                _fileCount = result.FileCount;
                MemberCountText.Text = result.MemberCount > result.Faces.Count
                    ? $"{result.MemberCount} faces clustered — showing the {result.Faces.Count} clearest."
                    : $"{result.MemberCount} face{(result.MemberCount == 1 ? "" : "s")} clustered.";
                SyncTagAllPhotosControls();
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
                var unkResult = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                    "markPersonsAsUnknown",
                    () => ViewModels.EngineClient.Instance.MarkPersonsAsUnknownAsync(new[] { personId }),
                    TimeSpan.FromSeconds(30));
                if (unkResult.Failed > 0 || unkResult.Succeeded == 0)
                {
                    var first = unkResult.Messages?.FirstOrDefault(m => !m.Ok)?.Message;
                    StatusText.Text = string.IsNullOrWhiteSpace(first) ? "Marking as unknown failed." : $"Marking as unknown failed: {first}";
                    return false;
                }
                try
                {
                    var s = AppViewModel.Instance.Settings;
                    s.PeopleHideUnknown = true;
                    s.Save();
                }
                catch (Exception ex)
                {
                    DebugLog.Warn("PersonDetailSheet unknown-visibility save failed: " + ex.Message);
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
