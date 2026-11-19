// FilePreviewSheet code-behind. Kind-dispatched preview; CloseInternal pauses the media player so audio doesn't keep playing after the sheet closes.

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
    private bool _isClosed;

    public event EventHandler? RequestClose;

    public FilePreviewSheet()
    {
        InitializeComponent();
        PreviewKeyDown += OnPreviewKeyDown;
        Loaded += OnSheetLoaded;
        IsTabStop = true;
    }

    /// <summary>Only ShowAsync completion is a terminal close signal.</summary>
    internal void CloseFromHost()
    {
        if (_isClosed) return;
        _isClosed = true;
        try { _loadingDelayTimer?.Stop(); } catch { /* swallow */ }
        StopAndClearMedia();
        DisposeMediaPlayer();
        _previewCache.Clear();
        FileID.Services.SelectionRegistry.Instance.PreviewedFileId = null;
    }

    private void OnSheetLoaded(object sender, RoutedEventArgs e)
    {
        // The host ContentDialog has no default button, so focus would otherwise sit on the dialog chrome and the tunneling PreviewKeyDown would never fire.
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

    /// <summary>Public because the host wires it on the ContentDialog via AddHandler(PreviewKeyDownEvent, …, handledEventsToo:true): once the dialog is shown IT owns keyboard focus (not this UserControl), so the sheet's own PreviewKeyDown never fires.</summary>
    internal void HandleKeyDown(Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        var focused = XamlRoot is null
            ? null
            : Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement(XamlRoot);
        bool typing = focused is TextBox;
        switch (e.Key)
        {
            case Windows.System.VirtualKey.Left:
            case Windows.System.VirtualKey.Up:
                if (typing) return;
                NavigateSibling(-1);
                e.Handled = true;
                break;
            case Windows.System.VirtualKey.Right:
            case Windows.System.VirtualKey.Down:
                if (typing) return;
                NavigateSibling(+1);
                e.Handled = true;
                break;
            case Windows.System.VirtualKey.Space:
                // Only when a media surface is up; otherwise let Space fall through (e.g. activating a focused button).
                if (typing) return;
                if (TryTogglePlayback()) e.Handled = true;
                break;
            case Windows.System.VirtualKey.Escape:
                RaiseClose();
                e.Handled = true;
                break;
        }
    }

    /// <summary>The MediaPlayer is created lazily when Source is set, so it exists by the time a video/audio preview is visible.</summary>
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
        SetFile(t.Path, t.Kind, t.SizeBytes, t.ModifiedAt, t.Id, t.HasText);
        UpdateNavButtons();
    }

    public async void SetFile(string path, string kind, long sizeBytes, double? modifiedAt, long fileId = 0,
                               bool hasText = false)
    {
        int navGen = System.Threading.Interlocked.Increment(ref _navGen);
        FileId = fileId;
        // Publish the active file so the Deep Analyze tab's "Analyze current" button can target it without LibraryView wiring.
        FileID.Services.SelectionRegistry.Instance.PreviewedFileId =
            fileId > 0 ? fileId : null;
        TextBadge.Visibility = hasText ? Visibility.Visible : Visibility.Collapsed;

        TagInput.Text = string.Empty;
        TagStatusText.Visibility = Visibility.Collapsed;
        // Refresh the proposed-rename card from the DB (separate query because the sheet may be opened for a file the LibraryView hasn't loaded yet).
        _ = LoadProposedNameAsync(fileId, navGen);
        _ = LoadVisionTagsAsync(fileId, navGen);

        // Media + placeholder are cleared up front (media must stop the instant we navigate away; neither flashes a loading ring).
        StopAndClearMedia();
        PreviewMedia.Visibility = Visibility.Collapsed;
        PlaceholderPanel.Visibility = Visibility.Collapsed;
        // Arm a short timer instead so only a genuinely slow decode shows it; the continuation that binds the new image (or shows media / placeholder) cancels the timer first.
        ArmLoadingDelay();

        // Any unhandled exception here would terminate the dispatcher and crash the window.
        try
        {
            await SetFileCoreAsync(path, kind, sizeBytes, modifiedAt, navGen);
        }
        catch (Exception ex)
        {
            if (_isClosed) return;
            Services.DebugLog.Warn("FilePreviewSheet.SetFile threw: " + ex);
            ShowPlaceholder("Preview failed: " + ex.Message);
        }
        finally
        {
            if (!_isClosed) HideLoadingChrome();
        }
    }

    // Shows LoadingPanel only if the new preview surface hasn't bound within the delay, so sub-100 ms cached sibling navigations never flash a ring.
    private Microsoft.UI.Xaml.DispatcherTimer? _loadingDelayTimer;
    private static readonly TimeSpan LoadingDelay = TimeSpan.FromMilliseconds(120);

    private void ArmLoadingDelay()
    {
        if (_loadingDelayTimer is null)
        {
            _loadingDelayTimer = new Microsoft.UI.Xaml.DispatcherTimer { Interval = LoadingDelay };
            _loadingDelayTimer.Tick += (_, _) =>
            {
                _loadingDelayTimer?.Stop();
                if (_isClosed) return;
                // Only reveal the ring while still genuinely loading.
                if (PreviewImage.Visibility == Visibility.Visible
                    || PreviewMedia.Visibility == Visibility.Visible)
                {
                    return;
                }
                // Collapse the prior frame so the ring isn't drawn over it.
                PreviewImage.Visibility = Visibility.Collapsed;
                LoadingPanel.Visibility = Visibility.Visible;
            };
        }
        _loadingDelayTimer.Stop();
        _loadingDelayTimer.Start();
    }

    // Called the instant a new preview surface binds (or the load finishes) so a fast cache-warm load never reveals the ring.
    private void HideLoadingChrome()
    {
        _loadingDelayTimer?.Stop();
        LoadingPanel.Visibility = Visibility.Collapsed;
    }

    private async Task SetFileCoreAsync(string path, string kind, long sizeBytes, double? modifiedAt, int navGen)
    {
        FilePath = path;
        FileNameText.Text = Path.GetFileName(path);
        ParentPathText.Text = Path.GetDirectoryName(path) ?? string.Empty;

        // Show the Analyze button only for images — matches macOS, which gates Deep Analyze to image files on the sheet (other kinds route through the Deep Analyze tab).
        AnalyzeButton.Visibility = kind == "image" ? Visibility.Visible : Visibility.Collapsed;

        BuildMetadata(path, kind, sizeBytes, modifiedAt);

        KindGlyphIcon.Glyph = GlyphFor(kind);

        switch (kind)
        {
            case "image":
            case "pdf":
            case "doc":
                await LoadShellThumbnailAsync(path, kind, modifiedAt, navGen);
                break;
            case "video":
            case "audio":
                await LoadVideoOrAudioAsync(path, kind, _mediaGen);
                break;
            default:
                ShowPlaceholder("No inline preview for this file type — use Open.");
                break;
        }
    }

    /// <summary>Use the Windows shell IThumbnailProvider chain to render the file at display size.</summary>
    private const int PreviewCacheCap = 8;
    private const long PreviewCacheBudgetBytes = 96L * 1024 * 1024;
    private const int MaxPreviewDecodePixels = 4096;
    private const uint MaxShellThumbnailDip = 2560;
    private const uint MaxDirectPreviewEncodedBytes = 32 * 1024 * 1024;
    private readonly LinkedList<(string Key, BitmapImage Image, long Bytes)> _previewCache = new();

    private double PreviewTargetDip()
    {
        double width = 0;
        try { width = PreviewCanvas.ActualWidth - 40; }
        catch (Exception ex) { Services.DebugLog.Warn("FilePreviewSheet: preview width unavailable: " + ex.Message); }
        return width > 64 ? width : 1024;
    }

    internal static int PreviewDecodePixels(double dip, double rasterizationScale)
    {
        var scale = rasterizationScale > 0 ? rasterizationScale : 1;
        return (int)Math.Clamp(Math.Ceiling(Math.Max(1, dip) * scale), 1, MaxPreviewDecodePixels);
    }

    private async Task LoadShellThumbnailAsync(string path, string kind, double? modifiedAt, int navGen)
    {
        // The await on GetThumbnailAsync can resume on a worker thread; constructing the BitmapImage there is a known native-fast-fail shape — RaiseFailFastException, no managed catch.
        var dispatcher = DispatcherQueue;
        var targetDip = PreviewTargetDip();
        var decodePixels = PreviewDecodePixels(targetDip, XamlRoot?.RasterizationScale ?? 1);
        var cacheKey = (modifiedAt.HasValue ? $"{path}|{modifiedAt.Value:R}" : path) + $"|{decodePixels}";
        if (dispatcher != null && TryShowCachedPreview(cacheKey, dispatcher, navGen))
        {
            return;
        }
        Windows.Storage.FileProperties.StorageItemThumbnail? thumb = null;
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            thumb = await file.GetThumbnailAsync(
                Windows.Storage.FileProperties.ThumbnailMode.SingleItem,
                (uint)Math.Clamp(Math.Ceiling(targetDip), 1, MaxShellThumbnailDip),
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
                        // Stale navigation — don't overwrite a newer file's preview.
                        if (_isClosed || _navGen != navGen) { tcs.TrySetResult(false); return; }
                        // Decode at the displayed width in physical pixels, never native resolution (e.g. 48 MP), so the cache budget holds.
                        var bmp = new BitmapImage { DecodePixelWidth = decodePixels };
                        await bmp.SetSourceAsync(captured);
                        PreviewImage.Source = bmp;
                        PreviewImage.Visibility = Visibility.Visible;
                        // On the UI thread here — safe to populate the DispatcherObject cache.
                        StorePreview(cacheKey, bmp);
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
        if (kind == "image" && dispatcher is not null
            && await TryLoadDirectImageAsync(path, cacheKey, decodePixels, dispatcher, navGen))
        {
            return;
        }

        // Don't show the failure placeholder if we navigated away (or the stale guard above set tcs=false for a superseded nav) — otherwise the prior file's load clobbers the CURRENT sibling's preview with a placeholder.
        if (_isClosed || _navGen != navGen) return;
        ShowPlaceholder(kind switch
        {
            "image" => "Image couldn't be decoded by any installed provider.",
            "pdf" => "PDF preview unavailable — open it in your default reader.",
            "doc" => "No preview available — Office handler may not be installed.",
            _ => "No preview available for this file type.",
        });
    }

    private async Task<bool> TryLoadDirectImageAsync(
        string path,
        string cacheKey,
        int decodePixels,
        Microsoft.UI.Dispatching.DispatcherQueue dispatcher,
        int navGen)
    {
        Windows.Storage.Streams.IRandomAccessStream? stream = null;
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            stream = await file.OpenReadAsync();
            if (stream.Size == 0 || stream.Size > (ulong)MaxDirectPreviewEncodedBytes)
            {
                return false;
            }

            var size = (uint)stream.Size;
            using var reader = new Windows.Storage.Streams.DataReader(stream.GetInputStreamAt(0));
            var loaded = await reader.LoadAsync(size);
            if (loaded != size)
            {
                return false;
            }

            var buffer = reader.ReadBuffer(loaded);
            using var memory = new Windows.Storage.Streams.InMemoryRandomAccessStream();
            await memory.WriteAsync(buffer);
            memory.Seek(0);

            var completion = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
            if (!dispatcher.TryEnqueue(async () =>
            {
                try
                {
                    if (_isClosed || _navGen != navGen)
                    {
                        completion.TrySetResult(false);
                        return;
                    }
                    var image = new BitmapImage { DecodePixelWidth = decodePixels };
                    await image.SetSourceAsync(memory);
                    if (_isClosed || _navGen != navGen)
                    {
                        completion.TrySetResult(false);
                        return;
                    }
                    PreviewImage.Source = image;
                    PreviewImage.Visibility = Visibility.Visible;
                    StorePreview(cacheKey, image);
                    completion.TrySetResult(true);
                }
                catch (Exception ex)
                {
                    Services.DebugLog.Warn($"FilePreviewSheet direct image preview failed: {ex.Message}");
                    completion.TrySetResult(false);
                }
            }))
            {
                return false;
            }

            return await completion.Task;
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn($"FilePreviewSheet direct image read failed: {ex.Message}");
            return false;
        }
        finally
        {
            try { stream?.Dispose(); } catch { }
        }
    }

    private bool TryShowCachedPreview(string cacheKey, Microsoft.UI.Dispatching.DispatcherQueue dispatcher, int navGen)
    {
        for (var node = _previewCache.First; node != null; node = node.Next)
        {
            if (node.Value.Key != cacheKey) continue;
            var bmp = node.Value.Image;
            _previewCache.Remove(node);
            _previewCache.AddFirst(node);
            dispatcher.TryEnqueue(() =>
            {
                if (_isClosed || _navGen != navGen) return; // stale navigation
                PreviewImage.Source = bmp;
                PreviewImage.Visibility = Visibility.Visible;
            });
            return true;
        }
        return false;
    }

    /// <summary>UI-thread-only (BitmapImage is a DispatcherObject).</summary>
    private void StorePreview(string cacheKey, BitmapImage bmp)
    {
        for (var node = _previewCache.First; node != null; node = node.Next)
        {
            if (node.Value.Key == cacheKey) { _previewCache.Remove(node); break; }
        }
        long bytes = Math.Max(1L, (long)bmp.PixelWidth * bmp.PixelHeight * 4);
        _previewCache.AddFirst((cacheKey, bmp, bytes));
        long total = 0;
        foreach (var entry in _previewCache) total += entry.Bytes;
        while (_previewCache.Count > 1
               && (_previewCache.Count > PreviewCacheCap || total > PreviewCacheBudgetBytes))
        {
            total -= _previewCache.Last!.Value.Bytes;
            _previewCache.RemoveLast();
        }
    }

    /// <summary>Windows ships a Media Foundation–backed control that handles most codecs out of the box (H.264, HEVC if codec installed, MP3, AAC, WMA, FLAC, …).</summary>
    private string _currentMediaKind = string.Empty;
    private Windows.Media.Core.MediaSource? _currentMediaSource;
    private int _mediaGen;
    private int _navGen;

    /// <summary>The MediaSource is tracked so it can be disposed on the next navigation/close — setting Source=null alone leaks it and pins the file handle.</summary>
    private async Task LoadVideoOrAudioAsync(string path, string kind, int gen)
    {
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            if (gen != _mediaGen) return; // navigated away during the await
            _currentMediaKind = kind;
            var src = Windows.Media.Core.MediaSource.CreateFromStorageFile(file);
            _currentMediaSource = src;
            PreviewImage.Visibility = Visibility.Collapsed;
            PreviewMedia.Source = src;
            PreviewMedia.Visibility = Visibility.Visible;
            // Attach failure handler on the (lazily-created) MediaPlayer; detach first so rapid sibling navigation can't multi-subscribe.
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
            ShowPlaceholder("Media couldn't load — try Open in default app.");
        }
    }

    private void OnMediaPlayerFailed(Windows.Media.Playback.MediaPlayer sender,
                                     Windows.Media.Playback.MediaPlayerFailedEventArgs args)
    {
        // Fires on a Media Foundation worker thread.
        var kind = _currentMediaKind;
        var err = args.ErrorMessage ?? args.Error.ToString();
        var gen = _mediaGen;
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_isClosed || gen != _mediaGen) return; // superseded media failure
            Services.DebugLog.Warn($"FilePreviewSheet media failed ({kind}): {err}");
            ShowPlaceholder(kind switch
            {
                "video" => "Video couldn't decode — missing codec? Try Open in default app.",
                "audio" => "Audio couldn't decode — try Open in default app.",
                _ => "Media couldn't load.",
            });
        });
    }

    private void ShowPlaceholder(string caption)
    {
        PreviewImage.Visibility = Visibility.Collapsed;
        PreviewMedia.Visibility = Visibility.Collapsed;
        PlaceholderPanel.Visibility = Visibility.Visible;
        PlaceholderText.Text = caption;
    }

    private void StopAndClearMedia()
    {
        // Invalidate any in-flight async media load so a slow StorageFile open can't bind a stale source after the user navigated away.
        _mediaGen++;
        try
        {
            // MediaPlayer is null until Source is set; guard before touching to avoid an NRE on the initial hide pass.
            if (PreviewMedia?.MediaPlayer is { } mp)
            {
                try { mp.MediaFailed -= OnMediaPlayerFailed; } catch { /* swallow */ }
                mp.Pause();
            }
            if (PreviewMedia != null)
            {
                PreviewMedia.Source = null;
            }
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

    /// <summary>Only call on close (Unloaded), not on sibling navigation (the element reuses its MediaPlayer for the next source).</summary>
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
        if (TextBadge.Visibility == Visibility.Visible) AddMetadataRow("Text", "Found in this file");
    }

    private void AddMetadataRow(string label, string value, bool monospaced = false, bool wrap = false)
    {
        var rowIdx = MetadataGrid.RowDefinitions.Count;
        MetadataGrid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        var labelText = new TextBlock
        {
            Text = label,
            Style = FileID.Services.ThemeHelper.GetStyleSafe("CaptionTextBlockStyle")!,
            Foreground = FileID.Services.ThemeHelper.GetBrushSafe("TextFillColorSecondaryBrush"),
            VerticalAlignment = VerticalAlignment.Top,
        };
        Grid.SetRow(labelText, rowIdx);
        Grid.SetColumn(labelText, 0);
        MetadataGrid.Children.Add(labelText);

        var valueText = new TextBlock
        {
            Text = value,
            Style = FileID.Services.ThemeHelper.GetStyleSafe("CaptionTextBlockStyle")!,
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

    private string? _pendingProposedName;

    private async System.Threading.Tasks.Task LoadProposedNameAsync(long fileId, int navGen)
    {
        ProposedRenameCard.Visibility = Visibility.Collapsed;
        _pendingProposedName = null;
        SetDescribeButtonState(false);
        if (fileId <= 0) return;
        string? name = null;
        var described = false;
        try
        {
            (name, described) = await System.Threading.Tasks.Task.Run(() =>
            {
                try
                {
                    if (!System.IO.File.Exists(Services.AppPaths.DbPath)) return ((string?)null, false);
                    using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                        new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                        {
                            DataSource = Services.AppPaths.DbPath,
                            Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                        }.ToString());
                    conn.Open();
                    using var cmd = conn.CreateCommand();
                    cmd.CommandText = "SELECT vlm_proposed_name, vlm_description FROM files WHERE id = $id";
                    cmd.Parameters.AddWithValue("$id", fileId);
                    using var reader = cmd.ExecuteReader();
                    if (!reader.Read()) return ((string?)null, false);
                    var s = reader.IsDBNull(0) ? null : reader.GetString(0);
                    var hasDescription = !reader.IsDBNull(1) && !string.IsNullOrWhiteSpace(reader.GetString(1));
                    return (string.IsNullOrWhiteSpace(s) ? null : s, hasDescription);
                }
                catch { return ((string?)null, false); }
            }).ConfigureAwait(true);
        }
        catch { name = null; }

        if (_isClosed || _navGen != navGen) return;
        SetDescribeButtonState(described);
        if (string.IsNullOrWhiteSpace(name)) return;
        _pendingProposedName = name;
        // Bail before touching XAML, then wrap the UI mutation so a torn-down dialog content tree doesn't fast-fail the dispatcher.
        if (_isClosed || _navGen != navGen) return; // navigated away during the await
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
                // Await the engine's BulkActionResult instead of fire-and-forget: handle_rename_files reports per-file failure (unsafe name, destination exists, source missing, DB-update failed) in the result, not as a thrown exception.
                var result = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                    "renameFiles",
                    () => ViewModels.EngineClient.Instance.RenameFilesAsync(new[]
                    {
                        new IpcSchema.RenameEntry(FileId, name!),
                    }),
                    TimeSpan.FromSeconds(30));
                if (result.Failed > 0 || result.Succeeded == 0)
                {
                    // Keep the proposed-name card open so the user can see it didn't apply and retry, rather than silently vanishing.
                    Services.DebugLog.Warn("Apply rename reported failure; leaving card open");
                    return;
                }
                ProposedRenameCard.Visibility = Visibility.Collapsed;
                _pendingProposedName = null;
            }
            catch (TimeoutException ex)
            {
                Services.DebugLog.Warn("Apply rename timed out: " + ex.Message);
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

    private async System.Threading.Tasks.Task LoadVisionTagsAsync(long fileId, int navGen)
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
                    using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
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
        if (_isClosed || _navGen != navGen) return; // navigated away during the await
        try
        {
            const double maxRowDip = 320;
            var outer = new StackPanel { Orientation = Microsoft.UI.Xaml.Controls.Orientation.Vertical, Spacing = 4 };
            StackPanel row = new() { Orientation = Microsoft.UI.Xaml.Controls.Orientation.Horizontal, Spacing = 4 };
            double rowDip = 0;
            foreach (var t in tags)
            {
                var formatted = FileID.Theme.Controls.TagChip.FormatTag(t);
                var chip = new FileID.Theme.Controls.TagChip { TagText = formatted };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(chip, $"Tag: {formatted}");
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
        "image" => "", // Photo
        "video" => "", // Video
        "pdf" => "", // Document
        "doc" => "", // Document
        "audio" => "", // MusicNote
        _ => "", // Folder / generic
    };

    private static string FormatSize(long bytes) => FileID.Services.ByteFormat.File(bytes);

    private void SetDescribeButtonState(bool described)
    {
        try
        {
            AnalyzeButtonText.Text = described ? "Describe again" : "Describe";
            var busy = ViewModels.EngineClient.Instance.DeepAnalyzeCommandInFlight
                       || ViewModels.EngineClient.Instance.DeepAnalyzeProgress is not null;
            ToolTipService.SetToolTip(AnalyzeButton, busy
                ? "FileID is already describing files. Try again when it finishes."
                : "Write a description and a suggested name for this file");
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("SetDescribeButtonState threw (sheet closed?): " + ex.Message);
        }
    }

    private async void OnAnalyzeClicked(object sender, RoutedEventArgs e)
    {
        if (FileId <= 0) return;
        try
        {
            AnalyzeButton.IsEnabled = false;
            await ViewModels.EngineClient.Instance.DeepAnalyzeFileAsync(FileId, ViewModels.AppViewModel.Instance.Settings.SelectedVlmModelKind);
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
            var result = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                "applyTags",
                () => ViewModels.EngineClient.Instance.ApplyTagsAsync(new long[] { FileId }, tags, mode: "add"),
                TimeSpan.FromSeconds(30));
            if (result.Failed > 0 || result.Succeeded == 0)
            {
                var first = result.Messages?.FirstOrDefault(m => !m.Ok)?.Message;
                ShowTagStatus(string.IsNullOrWhiteSpace(first) ? "Couldn't apply tags." : "Failed: " + first);
                return;
            }
            ShowTagStatus($"Added {tags.Length} tag" + (tags.Length == 1 ? string.Empty : "s") + ".");
            TagInput.Text = string.Empty;
        }
        catch (TimeoutException ex)
        {
            Services.DebugLog.Warn("FilePreviewSheet.ApplyDraftTags timed out: " + ex.Message);
            ShowTagStatus("Tagging didn't confirm — try again.");
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
