// FilePreviewSheet code-behind. Kind-dispatched preview matching the macOS
// LibraryView.swift:902-1112 preview sheet:
//   image  → BitmapImage at 1024 px (shell IThumbnailProvider chain).
//   video  → MediaPlayerElement with transport controls (in-app playback;
//            macOS opens default app, we do better).
//   audio  → MediaPlayerElement audio mode (in-app playback).
//   pdf    → 1024 px shell thumbnail (page 1, matches macOS PDFKit).
//   doc    → 1024 px shell thumbnail (Office providers render natively).
//   other  → kind glyph + "Open in default app" affordance.
//
// Lifecycle: SetFile is called once on open and again on every prev/next
// sibling navigation. CloseInternal is invoked by the toolbar X button,
// Esc key, or when the host dialog hides — it pauses the media player so
// audio doesn't keep playing after the sheet closes.

using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Views.Library;

public sealed partial class FilePreviewSheet : UserControl
{
    public string FilePath { get; private set; } = string.Empty;
    public long FileId { get; private set; }

    private IReadOnlyList<FileID.ViewModels.FileTile>? _siblings;
    private int _siblingIndex;

    /// <summary>Raised by the toolbar X button + Esc. Host (LibraryView)
    /// subscribes and calls <c>ContentDialog.Hide()</c>.</summary>
    public event EventHandler? RequestClose;

    public FilePreviewSheet()
    {
        InitializeComponent();
        // PreviewKeyDown (tunneling) so arrow-key navigation fires BEFORE a
        // focused video player / button consumes the key — and skips when the
        // tag TextBox is focused so typing still moves the cursor.
        PreviewKeyDown += OnPreviewKeyDown;
        Loaded += OnSheetLoaded;
        Unloaded += (_, _) =>
        {
            // Stop playback + fully dispose the MediaPlayer so audio can't keep
            // playing and the file handle is released after the dialog dismisses.
            StopAndClearMedia();
            DisposeMediaPlayer();
            // Clear the cross-tab "currently previewed" hint so the Deep
            // Analyze tab's "Analyze current" button disables when the
            // user closes the sheet.
            FileID.Services.SelectionRegistry.Instance.PreviewedFileId = null;
        };
        IsTabStop = true;
    }

    private void OnSheetLoaded(object sender, RoutedEventArgs e)
    {
        // The host ContentDialog has no default button, so focus would otherwise
        // sit on the dialog chrome and the tunneling PreviewKeyDown would never
        // fire. Grab focus into the sheet so arrow keys navigate immediately.
        try { Focus(FocusState.Programmatic); } catch { /* best-effort */ }
    }

    internal void SetSiblings(IReadOnlyList<FileID.ViewModels.FileTile> siblings, int currentIndex)
    {
        _siblings = siblings;
        _siblingIndex = currentIndex;
        UpdateNavButtons();
    }

    private void UpdateNavButtons()
    {
        if (PrevButton == null || NextButton == null) return;
        var haveSiblings = _siblings is { Count: > 0 };
        PrevButton.IsEnabled = haveSiblings && _siblingIndex > 0;
        NextButton.IsEnabled = haveSiblings && _siblingIndex < (_siblings?.Count ?? 0) - 1;
        if (haveSiblings && _siblings!.Count > 1)
        {
            SiblingCountText.Visibility = Visibility.Visible;
            SiblingCountText.Text = $"{_siblingIndex + 1} of {_siblings.Count}";
        }
        else
        {
            SiblingCountText.Visibility = Visibility.Collapsed;
        }
    }

    private void OnPreviewKeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
        => HandleKeyDown(e);

    /// <summary>Arrow-key sibling nav + Space play/pause + Esc close. Public
    /// because the host wires it on the ContentDialog via
    /// AddHandler(PreviewKeyDownEvent, …, handledEventsToo:true): once the
    /// dialog is shown IT owns keyboard focus (not this UserControl), so the
    /// sheet's own PreviewKeyDown never fires. The dialog is an ancestor of the
    /// focused element, so its tunneling Preview pass reaches us BEFORE a focused
    /// Button consumes Space — letting us intercept it for play/pause.</summary>
    internal void HandleKeyDown(Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Handled) return;
        // While typing in the tag box, let Left/Right/Space act as text input.
        var focused = XamlRoot is null
            ? null
            : Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement(XamlRoot);
        bool typing = focused is TextBox;
        switch (e.Key)
        {
            case Windows.System.VirtualKey.Left:
                if (typing) return;
                NavigateSibling(-1);
                e.Handled = true;
                break;
            case Windows.System.VirtualKey.Right:
                if (typing) return;
                NavigateSibling(+1);
                e.Handled = true;
                break;
            case Windows.System.VirtualKey.Space:
                // Space starts/pauses video+audio (the file loads paused —
                // AutoPlay=False). Only when a media surface is up; otherwise let
                // Space fall through (e.g. activating a focused button).
                if (typing) return;
                if (TryTogglePlayback()) e.Handled = true;
                break;
            case Windows.System.VirtualKey.Escape:
                RaiseClose();
                e.Handled = true;
                break;
        }
    }

    /// <summary>Toggle the MediaPlayerElement between play and pause. Returns
    /// false when no media surface is active (caller lets the key pass through).
    /// The MediaPlayer is created lazily when Source is set, so it exists by the
    /// time a video/audio preview is visible.</summary>
    private bool TryTogglePlayback()
    {
        if (PreviewMedia.Visibility != Visibility.Visible) return false;
        var mp = PreviewMedia.MediaPlayer;
        if (mp is null) return false;
        if (mp.PlaybackSession.PlaybackState == Windows.Media.Playback.MediaPlaybackState.Playing)
            mp.Pause();
        else
            mp.Play();
        return true;
    }

    private void OnPrevClicked(object sender, RoutedEventArgs e) => NavigateSibling(-1);
    private void OnNextClicked(object sender, RoutedEventArgs e) => NavigateSibling(+1);
    private void OnCloseClicked(object sender, RoutedEventArgs e) => RaiseClose();

    private void RaiseClose()
    {
        StopAndClearMedia();
        RequestClose?.Invoke(this, EventArgs.Empty);
    }

    private void NavigateSibling(int delta)
    {
        if (_siblings is null || _siblings.Count == 0) return;
        var next = _siblingIndex + delta;
        if (next < 0 || next >= _siblings.Count) return;
        _siblingIndex = next;
        var t = _siblings[next];
        SetFile(t.Path, t.Kind, t.SizeBytes, t.ModifiedAt, t.Id, t.HasFaces, t.HasText);
        UpdateNavButtons();
    }

    public async void SetFile(string path, string kind, long sizeBytes, double? modifiedAt, long fileId = 0,
                               bool hasFaces = false, bool hasText = false)
    {
        FileId = fileId;
        // Publish the active file so the Deep Analyze tab's "Analyze
        // current" button can target it without LibraryView wiring.
        FileID.Services.SelectionRegistry.Instance.PreviewedFileId =
            fileId > 0 ? fileId : null;
        TextBadge.Visibility = hasText ? Visibility.Visible : Visibility.Collapsed;

        TagInput.Text = string.Empty;
        TagStatusText.Visibility = Visibility.Collapsed;
        // Refresh the proposed-rename card from the DB (separate query
        // because the sheet may be opened for a file the LibraryView
        // hasn't loaded yet).
        _ = LoadProposedNameAsync(fileId);
        // Refresh the Vision Tags card (auto + user tags for this file).
        _ = LoadVisionTagsAsync(fileId);

        // Hide every preview surface up front; the kind dispatcher below
        // re-shows exactly one. Without this a navigation from video →
        // image would leave both visible.
        StopAndClearMedia();
        PreviewImage.Visibility = Visibility.Collapsed;
        PreviewMedia.Visibility = Visibility.Collapsed;
        PlaceholderPanel.Visibility = Visibility.Collapsed;
        LoadingPanel.Visibility = Visibility.Visible;

        // Async-void → must wrap the entire body. Any unhandled exception
        // here would terminate the dispatcher and crash the window.
        try
        {
            await SetFileCoreAsync(path, kind, sizeBytes, modifiedAt);
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("FilePreviewSheet.SetFile threw: " + ex);
            ShowPlaceholder(kind, "Preview failed: " + ex.Message);
        }
        finally
        {
            LoadingPanel.Visibility = Visibility.Collapsed;
        }
    }

    private async Task SetFileCoreAsync(string path, string kind, long sizeBytes, double? modifiedAt)
    {
        FilePath = path;
        FileNameText.Text = Path.GetFileName(path);
        ParentPathText.Text = Path.GetDirectoryName(path) ?? string.Empty;

        // Show the Analyze button only for images — matches macOS, which
        // gates Deep Analyze to image files on the sheet (other kinds
        // route through the Deep Analyze tab).
        AnalyzeButton.Visibility = kind == "image" ? Visibility.Visible : Visibility.Collapsed;

        // Build the sidebar metadata grid.
        BuildMetadata(path, kind, sizeBytes, modifiedAt);

        // Set the kind glyph for the placeholder fallback.
        KindGlyphIcon.Glyph = GlyphFor(kind);

        // Kind dispatch — matches macOS LibraryView.swift:975-1006.
        switch (kind)
        {
            case "image":
            case "pdf":
            case "doc":
                await LoadShellThumbnailAsync(path, kind);
                break;
            case "video":
            case "audio":
                await LoadVideoOrAudioAsync(path, kind, _mediaGen);
                break;
            default:
                ShowPlaceholder(kind, "No inline preview for this file type — use Open.");
                break;
        }
    }

    /// <summary>Use the Windows shell IThumbnailProvider chain to render
    /// the file at 1024 px. Same API Explorer uses, so HEIC / RAW / Office
    /// / .pages / PDF all render correctly when their providers are
    /// installed. Falls back to the placeholder if the provider returns
    /// nothing.</summary>
    private async Task LoadShellThumbnailAsync(string path, string kind)
    {
        // BitmapImage is a DispatcherObject. The await on
        // GetThumbnailAsync can resume on a worker thread; constructing the
        // BitmapImage there is a known native-fast-fail shape —
        // RaiseFailFastException, no managed catch. Capture the dispatcher
        // before any await and marshal the BitmapImage construction +
        // PreviewImage.Source/Visibility assignment inside one TryEnqueue.
        var dispatcher = DispatcherQueue;
        Windows.Storage.FileProperties.StorageItemThumbnail? thumb = null;
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            thumb = await file.GetThumbnailAsync(
                Windows.Storage.FileProperties.ThumbnailMode.SingleItem,
                1024,
                Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale);
            if (thumb is { Size: > 0 } && dispatcher != null)
            {
                var captured = thumb;
                thumb = null;
                var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
                var enqueued = dispatcher.TryEnqueue(async () =>
                {
                    try
                    {
                        var bmp = new BitmapImage();
                        await bmp.SetSourceAsync(captured);
                        PreviewImage.Source = bmp;
                        PreviewImage.Visibility = Visibility.Visible;
                        tcs.TrySetResult(true);
                    }
                    catch (Exception ex)
                    {
                        Services.DebugLog.Warn($"FilePreviewSheet UI render ({kind}): {ex.Message}");
                        tcs.TrySetResult(false);
                    }
                    finally
                    {
                        try { captured.Dispose(); } catch { }
                    }
                });
                if (!enqueued)
                {
                    Services.DebugLog.Warn("FilePreviewSheet: dispatcher.TryEnqueue returned false.");
                    try { captured.Dispose(); } catch { }
                }
                else if (await tcs.Task)
                {
                    return;
                }
            }
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn($"FilePreviewSheet shell thumbnail failed for {kind}: {ex.Message}");
        }
        finally
        {
            try { thumb?.Dispose(); } catch { }
        }
        ShowPlaceholder(kind, kind switch
        {
            "image" => "Image couldn't be decoded by any installed provider.",
            "pdf" => "PDF preview unavailable — open it in your default reader.",
            "doc" => "No preview available — Office handler may not be installed.",
            _ => "No preview available for this file type.",
        });
    }

    /// <summary>Hand the file to MediaPlayerElement. Windows ships a
    /// Media Foundation–backed control that handles most codecs out of
    /// the box (H.264, HEVC if codec installed, MP3, AAC, WMA, FLAC, …).
    /// Transport controls are XAML-rendered (AreTransportControlsEnabled
    /// in the .xaml).
    ///
    /// wire MediaFailed for the async failure path. Codec-missing
    /// (HEVC video without the Store codec extension, exotic container,
    /// DRM-protected file) fires MediaFailed off-thread; without this
    /// handler the user would just see a blank media surface forever.
    /// On failure we fall back to the kind placeholder + "Open in default
    /// app" hint, matching macOS's "video opens externally" behavior.</summary>
    private string _currentMediaKind = string.Empty;
    private Windows.Media.Core.MediaSource? _currentMediaSource;
    // Bumped on every SetFile / teardown so a slow async media load that
    // resolves AFTER the user has navigated away detects it's stale and bails.
    private int _mediaGen;

    /// <summary>Load a video/audio file into the MediaPlayerElement via
    /// CreateFromStorageFile (the StorageFile broker — the same path the
    /// thumbnail loader uses), which is more reliable for arbitrary local paths
    /// than a raw file:// URI. The MediaSource is tracked so it can be disposed
    /// on the next navigation/close — setting Source=null alone leaks it and
    /// pins the file handle.</summary>
    private async Task LoadVideoOrAudioAsync(string path, string kind, int gen)
    {
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            if (gen != _mediaGen) return; // navigated away during the await
            _currentMediaKind = kind;
            var src = Windows.Media.Core.MediaSource.CreateFromStorageFile(file);
            _currentMediaSource = src;
            PreviewMedia.Source = src;
            PreviewMedia.Visibility = Visibility.Visible;
            // Attach failure handler on the (lazily-created) MediaPlayer; detach
            // first so rapid sibling navigation can't multi-subscribe.
            if (PreviewMedia.MediaPlayer is { } mp)
            {
                mp.MediaFailed -= OnMediaPlayerFailed;
                mp.MediaFailed += OnMediaPlayerFailed;
            }
        }
        catch (Exception ex)
        {
            if (gen != _mediaGen) return;
            Services.DebugLog.Warn($"FilePreviewSheet media load failed for {kind}: {ex.Message}");
            ShowPlaceholder(kind, "Media couldn't load — try Open in default app.");
        }
    }

    private void OnMediaPlayerFailed(Windows.Media.Playback.MediaPlayer sender,
                                     Windows.Media.Playback.MediaPlayerFailedEventArgs args)
    {
        // Fires on a Media Foundation worker thread. Marshal to UI thread
        // before touching XAML state. Capture the kind under the lock so a
        // navigation that races our failure doesn't bleed kind labels.
        var kind = _currentMediaKind;
        var err = args.ErrorMessage ?? args.Error.ToString();
        DispatcherQueue.TryEnqueue(() =>
        {
            Services.DebugLog.Warn($"FilePreviewSheet media failed ({kind}): {err}");
            ShowPlaceholder(kind, kind switch
            {
                "video" => "Video couldn't decode — missing codec? Try Open in default app.",
                "audio" => "Audio couldn't decode — try Open in default app.",
                _ => "Media couldn't load.",
            });
        });
    }

    private void ShowPlaceholder(string kind, string caption)
    {
        PreviewImage.Visibility = Visibility.Collapsed;
        PreviewMedia.Visibility = Visibility.Collapsed;
        PlaceholderPanel.Visibility = Visibility.Visible;
        PlaceholderText.Text = caption;
    }

    private void StopAndClearMedia()
    {
        // Invalidate any in-flight async media load so a slow StorageFile open
        // can't bind a stale source after the user navigated away.
        _mediaGen++;
        try
        {
            // MediaPlayer is null until Source is set; guard before
            // touching to avoid an NRE on the initial hide pass.
            if (PreviewMedia?.MediaPlayer is { } mp)
            {
                try { mp.MediaFailed -= OnMediaPlayerFailed; } catch { /* swallow */ }
                mp.Pause();
            }
            if (PreviewMedia != null)
            {
                PreviewMedia.Source = null;
            }
            // Setting Source=null does NOT dispose the MediaSource — do it here
            // so its file handle + buffers free (matters when arrow-navigating
            // through many videos).
            if (_currentMediaSource is { } src)
            {
                _currentMediaSource = null;
                try { src.Dispose(); } catch { /* swallow */ }
            }
            _currentMediaKind = string.Empty;
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("FilePreviewSheet.StopAndClearMedia: " + ex.Message);
        }
    }

    /// <summary>Fully tear down the MediaPlayerElement's auto-created MediaPlayer
    /// on close — pausing + nulling the source isn't always enough to stop audio
    /// or release the file handle. Only call on close (Unloaded), not on sibling
    /// navigation (the element reuses its MediaPlayer for the next source).</summary>
    private void DisposeMediaPlayer()
    {
        try
        {
            if (PreviewMedia?.MediaPlayer is { } mp)
            {
                PreviewMedia.SetMediaPlayer(null);
                mp.Dispose();
            }
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("FilePreviewSheet.DisposeMediaPlayer: " + ex.Message);
        }
    }

    private void BuildMetadata(string path, string kind, long sizeBytes, double? modifiedAt)
    {
        MetadataGrid.Children.Clear();
        MetadataGrid.RowDefinitions.Clear();

        AddMetadataRow("Path", path, monospaced: true, wrap: true);
        AddMetadataRow("Kind", KindDisplay(kind));
        AddMetadataRow("Size", FormatSize(sizeBytes));
        if (modifiedAt.HasValue)
        {
            var dt = DateTimeOffset.FromUnixTimeMilliseconds((long)(modifiedAt.Value * 1000)).LocalDateTime;
            AddMetadataRow("Modified", dt.ToString("g"));
        }
        if (TextBadge.Visibility == Visibility.Visible) AddMetadataRow("Text", "Detected (OCR)");
    }

    private void AddMetadataRow(string label, string value, bool monospaced = false, bool wrap = false)
    {
        var rowIdx = MetadataGrid.RowDefinitions.Count;
        MetadataGrid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        var labelText = new TextBlock
        {
            Text = label,
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            Foreground = (Microsoft.UI.Xaml.Media.Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
            VerticalAlignment = VerticalAlignment.Top,
        };
        Grid.SetRow(labelText, rowIdx);
        Grid.SetColumn(labelText, 0);
        MetadataGrid.Children.Add(labelText);

        var valueText = new TextBlock
        {
            Text = value,
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            TextWrapping = wrap ? TextWrapping.Wrap : TextWrapping.NoWrap,
            TextTrimming = wrap ? TextTrimming.None : TextTrimming.CharacterEllipsis,
            IsTextSelectionEnabled = true,
        };
        if (monospaced)
        {
            valueText.FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Consolas");
        }
        Grid.SetRow(valueText, rowIdx);
        Grid.SetColumn(valueText, 1);
        MetadataGrid.Children.Add(valueText);
    }

    private static string KindDisplay(string kind) => kind switch
    {
        "image" => "Image",
        "video" => "Video",
        "pdf" => "PDF",
        "doc" => "Document",
        "audio" => "Audio",
        _ => kind,
    };

    // ─── Proposed rename card ─────────────────────────────────────────
    private string? _pendingProposedName;

    private async System.Threading.Tasks.Task LoadProposedNameAsync(long fileId)
    {
        ProposedRenameCard.Visibility = Visibility.Collapsed;
        _pendingProposedName = null;
        if (fileId <= 0) return;
        string? name = null;
        try
        {
            name = await System.Threading.Tasks.Task.Run(() =>
            {
                try
                {
                    if (!System.IO.File.Exists(Services.AppPaths.DbPath)) return null;
                    var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                        new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                        {
                            DataSource = Services.AppPaths.DbPath,
                            Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                        }.ToString());
                    conn.Open();
                    using var cmd = conn.CreateCommand();
                    cmd.CommandText = "SELECT vlm_proposed_name FROM files WHERE id = $id";
                    cmd.Parameters.AddWithValue("$id", fileId);
                    var v = cmd.ExecuteScalar();
                    var s = v as string;
                    return string.IsNullOrWhiteSpace(s) ? null : s;
                }
                catch { return null; }
            }).ConfigureAwait(true);
        }
        catch { name = null; }

        if (string.IsNullOrWhiteSpace(name)) return;
        _pendingProposedName = name;
        // Defensive: sheet may have closed during the async query. Wrap
        // the UI mutation so a torn-down dialog content tree doesn't
        // fast-fail the dispatcher.
        try
        {
            ProposedRenameText.Text = name;
            ProposedRenameCard.Visibility = Visibility.Visible;
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("LoadProposedName UI update threw (sheet closed?): " + ex.Message);
        }
    }

    private async void OnApplyRenameClicked(object sender, RoutedEventArgs e)
        => await Services.DebugLog.SafeRunAsync(nameof(OnApplyRenameClicked), async () =>
        {
            var name = _pendingProposedName;
            if (FileId <= 0 || string.IsNullOrWhiteSpace(name)) return;
            try
            {
                await ViewModels.EngineClient.Instance.RenameFilesAsync(new[]
                {
                    new IpcSchema.RenameEntry(FileId, name!),
                });
                ProposedRenameCard.Visibility = Visibility.Collapsed;
                _pendingProposedName = null;
            }
            catch (Exception ex)
            {
                Services.DebugLog.Warn("Apply rename failed: " + ex.Message);
            }
        });

    private void OnDismissRenameClicked(object sender, RoutedEventArgs e)
        => Services.DebugLog.SafeRun(nameof(OnDismissRenameClicked), () =>
        {
            ProposedRenameCard.Visibility = Visibility.Collapsed;
            _pendingProposedName = null;
        });

    // ─── Vision Tags card ─────────────────────────────────────────────
    // Reads every auto + user tag for this file from the DB and renders
    // them as TagChip controls inside VisionTagsHost. Mirrors the Library
    // card chip strip — same control, same formatting, same visual weight.
    private async System.Threading.Tasks.Task LoadVisionTagsAsync(long fileId)
    {
        VisionTagsCard.Visibility = Visibility.Collapsed;
        VisionTagsHost.Children.Clear();
        if (fileId <= 0) return;

        System.Collections.Generic.List<string> tags;
        try
        {
            tags = await System.Threading.Tasks.Task.Run(() =>
            {
                var list = new System.Collections.Generic.List<string>();
                try
                {
                    if (!System.IO.File.Exists(Services.AppPaths.DbPath)) return list;
                    var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                        new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                        {
                            DataSource = Services.AppPaths.DbPath,
                            Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                        }.ToString());
                    conn.Open();
                    using var cmd = conn.CreateCommand();
                    cmd.CommandText = """
                        SELECT tag FROM tags
                        WHERE file_id = $id AND source IN ('auto','user')
                        ORDER BY source DESC, rowid
                        """;
                    cmd.Parameters.AddWithValue("$id", fileId);
                    using var rdr = cmd.ExecuteReader();
                    while (rdr.Read())
                    {
                        list.Add(rdr.GetString(0));
                    }
                }
                catch { /* DB unavailable; return empty list */ }
                return list;
            }).ConfigureAwait(true);
        }
        catch { return; }

        if (tags.Count == 0) return;
        // UI mutation in a try/catch — sheet may have closed during the
        // async DB read; the XAML element references could be disposed.
        try
        {
            // Build a wrapping chip layout via runtime row construction:
            // each new chip is appended to the last row; if the row exceeds
            // 320 DIP we start a new row. Cheaper than a custom panel and
            // good enough for ≤16 tag chips.
            const double maxRowDip = 320;
            var outer = new StackPanel { Orientation = Microsoft.UI.Xaml.Controls.Orientation.Vertical, Spacing = 4 };
            StackPanel row = new() { Orientation = Microsoft.UI.Xaml.Controls.Orientation.Horizontal, Spacing = 4 };
            double rowDip = 0;
            foreach (var t in tags)
            {
                var formatted = FileID.Theme.Controls.TagChip.FormatTag(t);
                var chip = new FileID.Theme.Controls.TagChip { TagText = formatted };
                // Estimate chip width — TagChip is 11pt font; rough heuristic
                // is "~7 DIP per char + 10 DIP padding", capped at 120.
                double estDip = System.Math.Min(120, 7.0 * System.Math.Max(2, formatted.Length) + 10.0);
                if (rowDip + estDip > maxRowDip && row.Children.Count > 0)
                {
                    outer.Children.Add(row);
                    row = new StackPanel { Orientation = Microsoft.UI.Xaml.Controls.Orientation.Horizontal, Spacing = 4 };
                    rowDip = 0;
                }
                row.Children.Add(chip);
                rowDip += estDip + 4;
            }
            if (row.Children.Count > 0) outer.Children.Add(row);
            VisionTagsHost.Children.Add(outer);
            VisionTagsCard.Visibility = Visibility.Visible;
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("LoadVisionTags UI update threw (sheet closed?): " + ex.Message);
        }
    }

    private static string GlyphFor(string kind) => kind switch
    {
        // Segoe Fluent Icons — match the tile-level glyphs.
        "image" => "", // Photo
        "video" => "", // Video
        "pdf" => "", // Document
        "doc" => "", // Document
        "audio" => "", // MusicNote
        _ => "", // Folder / generic
    };

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:0.#} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):0.#} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):0.##} GB";
    }

    private async void OnAnalyzeClicked(object sender, RoutedEventArgs e)
    {
        if (FileId <= 0) return;
        try
        {
            AnalyzeButton.IsEnabled = false;
            await ViewModels.EngineClient.Instance.DeepAnalyzeFileAsync(FileId, Services.AppSettings.Load().SelectedVlmModelKind);
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("Analyze failed: " + ex);
        }
        finally
        {
            AnalyzeButton.IsEnabled = true;
        }
    }

    private void OnRevealClicked(object sender, RoutedEventArgs e)
        => Services.DebugLog.SafeRun(nameof(OnRevealClicked), () => Services.SafeOpen.Reveal(FilePath));

    private void OnOpenClicked(object sender, RoutedEventArgs e)
        => Services.DebugLog.SafeRun(nameof(OnOpenClicked), () =>
        {
            // SEC-9: ext-gated; falls back to Reveal for non-allowlisted ext.
            if (!Services.SafeOpen.TryOpenFile(FilePath))
            {
                Services.SafeOpen.Reveal(FilePath);
            }
        });

    private void OnTagInputKeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        if (e.Key == Windows.System.VirtualKey.Enter)
        {
            ApplyDraftTags();
            e.Handled = true;
        }
    }

    private void OnApplyTagsClicked(object sender, RoutedEventArgs e) => ApplyDraftTags();

    private async void ApplyDraftTags()
    {
        if (FileId <= 0)
        {
            ShowTagStatus("Reopen the preview before tagging — no file id.");
            return;
        }
        var raw = (TagInput.Text ?? string.Empty).Trim();
        if (raw.Length == 0) return;
        var tags = raw
            .Split(',', StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries)
            .Where(t => t.Length > 0)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .ToArray();
        if (tags.Length == 0)
        {
            ShowTagStatus("Type one or more tags separated by commas.");
            return;
        }
        try
        {
            ApplyTagsButton.IsEnabled = false;
            await ViewModels.EngineClient.Instance.ApplyTagsAsync(new long[] { FileId }, tags, mode: "add");
            ShowTagStatus($"Added {tags.Length} tag" + (tags.Length == 1 ? string.Empty : "s") + ".");
            TagInput.Text = string.Empty;
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("FilePreviewSheet.ApplyDraftTags failed: " + ex);
            ShowTagStatus("Failed: " + ex.Message);
        }
        finally
        {
            ApplyTagsButton.IsEnabled = true;
        }
    }

    private void ShowTagStatus(string msg)
    {
        TagStatusText.Text = msg;
        TagStatusText.Visibility = Visibility.Visible;
    }
}
