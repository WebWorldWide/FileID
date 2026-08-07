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
// sibling navigation. The host calls CloseFromHost after ShowAsync returns
// so media and file handles are released after every dismissal path.

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
    private bool _unloaded;

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
        IsTabStop = true;
        // Auto-reset the Analyze/Cancel button when the engine finishes the
        // run this sheet started. Detached in CloseFromHost.
        ViewModels.EngineClient.Instance.PropertyChanged += OnEngineChangedForAnalyze;
    }

    private void OnSheetLoaded(object sender, RoutedEventArgs e)
    {
        _unloaded = false;
        // The host ContentDialog has no default button, so focus would otherwise
        // sit on the dialog chrome and the tunneling PreviewKeyDown would never
        // fire. Grab focus into the sheet so arrow keys navigate immediately.
        try { Focus(FocusState.Programmatic); } catch { /* best-effort */ }
    }

    // ContentDialog reparents its content through a transient Unloaded event
    // while opening. Only ShowAsync completion is a terminal close signal.
    internal void CloseFromHost()
    {
        if (_unloaded) return;
        _unloaded = true;
        try { ViewModels.EngineClient.Instance.PropertyChanged -= OnEngineChangedForAnalyze; }
        catch { /* best-effort */ }
        try { _loadingDelayTimer?.Stop(); } catch { }
        StopAndClearMedia();
        DisposeMediaPlayer();
        FileID.Services.SelectionRegistry.Instance.PreviewedFileId = null;
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

    // Accelerator path (see the XAML comment): fires even when the routed
    // KeyDown never reaches the dialog handler. Accelerators are evaluated
    // BEFORE routed key events, so when we handle one here the routed path
    // never fires (no double-navigation); when we decline (TextBox focused),
    // the key continues to the routed pipeline where HandleKeyDown's own
    // typing guard also declines and the caret key reaches the TextBox.
    private void OnLeftAccelerator(
        Microsoft.UI.Xaml.Input.KeyboardAccelerator sender,
        Microsoft.UI.Xaml.Input.KeyboardAcceleratorInvokedEventArgs args)
        => args.Handled = TryNavigateFromAccelerator(-1);

    private void OnRightAccelerator(
        Microsoft.UI.Xaml.Input.KeyboardAccelerator sender,
        Microsoft.UI.Xaml.Input.KeyboardAcceleratorInvokedEventArgs args)
        => args.Handled = TryNavigateFromAccelerator(+1);

    // Set when an accelerator navigates, so the routed pass for the SAME
    // keystroke (the dialog handler runs with handledEventsToo:true and
    // deliberately ignores e.Handled — see HandleKeyDown) can't navigate a
    // second time. Both passes run within one input dispatch, so a short
    // wall-clock window is a reliable same-keystroke discriminator.
    private long _acceleratorNavTick;

    private bool TryNavigateFromAccelerator(int delta)
    {
        var focused = XamlRoot is null
            ? null
            : Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement(XamlRoot);
        if (focused is TextBox) return false; // typing — give the caret key back
        _acceleratorNavTick = Environment.TickCount64;
        NavigateSibling(delta);
        return true;
    }

    private bool AcceleratorAlreadyNavigated()
        => Environment.TickCount64 - _acceleratorNavTick < 100;

    /// <summary>Arrow-key sibling nav + Space play/pause + Esc close. Public
    /// because the host wires it on the ContentDialog via
    /// AddHandler(PreviewKeyDownEvent, …, handledEventsToo:true): once the
    /// dialog is shown IT owns keyboard focus (not this UserControl), so the
    /// sheet's own PreviewKeyDown never fires. The dialog is an ancestor of the
    /// focused element, so its tunneling Preview pass reaches us BEFORE a focused
    /// Button consumes Space — letting us intercept it for play/pause.</summary>
    internal void HandleKeyDown(Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        // Do NOT bail on e.Handled here. The host wires this on the ContentDialog
        // via AddHandler(PreviewKeyDownEvent, …, handledEventsToo:true) *because*
        // the dialog's own tunneling key handling marks Left/Right/Space/Esc as
        // Handled before the tunnel reaches us — an early `if (e.Handled) return;`
        // swallowed every sibling navigation. We set e.Handled after acting, and
        // the sheet's own (non-handledEventsToo) PreviewKeyDown subscription is
        // still gated by Handled, so acting here can't double-navigate.
        // While typing in the tag box, let Left/Right/Space act as text input.
        var focused = XamlRoot is null
            ? null
            : Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement(XamlRoot);
        bool typing = focused is TextBox;
        switch (e.Key)
        {
            case Windows.System.VirtualKey.Left:
                if (typing) return;
                if (!AcceleratorAlreadyNavigated()) NavigateSibling(-1);
                e.Handled = true;
                break;
            case Windows.System.VirtualKey.Right:
                if (typing) return;
                if (!AcceleratorAlreadyNavigated()) NavigateSibling(+1);
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
        // Stamp this navigation so async loads from a prior file bail. (audit A9)
        int navGen = System.Threading.Interlocked.Increment(ref _navGen);
        FileId = fileId;
        // Publish the active file so the Deep Analyze tab's "Analyze
        // current" button can target it without LibraryView wiring.
        FileID.Services.SelectionRegistry.Instance.PreviewedFileId =
            fileId > 0 ? fileId : null;
        TextBadge.Visibility = hasText ? Visibility.Visible : Visibility.Collapsed;

        TagInput.Text = string.Empty;
        TagStatusText.Visibility = Visibility.Collapsed;
        // Navigation resets the Analyze button for the NEW file; a run already
        // in flight keeps going (the Deep Analyze tab shows its progress).
        if (_analyzeRunning) SetAnalyzeIdle();
        // Refresh the proposed-rename card from the DB (separate query
        // because the sheet may be opened for a file the LibraryView
        // hasn't loaded yet).
        _ = LoadProposedNameAsync(fileId, navGen);
        // Refresh the Vision Tags card (auto + user tags for this file).
        _ = LoadVisionTagsAsync(fileId, navGen);

        // Media + placeholder are cleared up front (media must stop the instant
        // we navigate away; neither flashes a loading ring). The prior IMAGE
        // frame is deliberately left on screen until the new thumbnail binds —
        // see the deferred LoadingPanel below. The kind dispatcher's success
        // paths each collapse PreviewImage when they show their own surface.
        StopAndClearMedia();
        PreviewMedia.Visibility = Visibility.Collapsed;
        PlaceholderPanel.Visibility = Visibility.Collapsed;
        // Defer the loading ring: arrow-key sibling nav is usually a cache-warm
        // load that resolves in well under 100 ms. Collapsing the prior image
        // and showing LoadingPanel synchronously made the ring flash between
        // every sibling. Arm a short timer instead so only a genuinely slow
        // decode shows it; the continuation that binds the new image (or shows
        // media / placeholder) cancels the timer first.
        ArmLoadingDelay();

        // Async-void → must wrap the entire body. Any unhandled exception
        // here would terminate the dispatcher and crash the window.
        try
        {
            await SetFileCoreAsync(path, kind, sizeBytes, modifiedAt, navGen);
        }
        catch (Exception ex)
        {
            // The sheet may have unloaded while SetFileCoreAsync awaited; a
            // post-continuation ShowPlaceholder would touch a torn-down tree.
            if (_unloaded) return;
            Services.DebugLog.Warn("FilePreviewSheet.SetFile threw: " + ex);
            ShowPlaceholder(kind, "Preview failed: " + ex.Message);
        }
        finally
        {
            if (!_unloaded) HideLoadingChrome();
        }
    }

    // Deferred loading-ring timer. Shows LoadingPanel only if the new preview
    // surface hasn't bound within the delay, so sub-100 ms cached sibling
    // navigations never flash a ring. Single instance reused across SetFile
    // calls; each call re-arms it and every terminal show-path cancels it.
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
                if (_unloaded) return;
                // If a fast decode already bound a surface before this 120ms delay
                // elapsed (the Tick can be queued just after the bind continuation
                // but before HideLoadingChrome stops the timer), do NOT blank it —
                // collapsing PreviewImage here with no re-show would leave a blank
                // preview. Only reveal the ring while still genuinely loading.
                if (PreviewImage.Visibility == Visibility.Visible
                    || PreviewMedia.Visibility == Visibility.Visible)
                {
                    return;
                }
                // Still loading after the delay — now it's worth showing the
                // ring. Collapse the prior frame so the ring isn't drawn over it.
                PreviewImage.Visibility = Visibility.Collapsed;
                LoadingPanel.Visibility = Visibility.Visible;
            };
        }
        _loadingDelayTimer.Stop();
        _loadingDelayTimer.Start();
    }

    // Cancel the pending loading-ring timer and hide the ring. Called the
    // instant a new preview surface binds (or the load finishes) so a fast
    // cache-warm load never reveals the ring.
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
                await LoadShellThumbnailAsync(path, kind, modifiedAt, navGen);
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
    /// <summary>Bounded MRU cache of the 1024px preview BitmapImages, keyed by
    /// path|modifiedAt. Arrow-keying back and forth over the same siblings would
    /// otherwise re-run the shell extract + ~4 MB decode every step (this is the
    /// 1024px preview path — separate from ThumbnailService's 192px L1/L2). All
    /// access is UI-thread-affine: BitmapImage is a DispatcherObject, so this is
    /// only ever read/written from the dispatcher-marshaled paths below.</summary>
    private const int PreviewCacheCap = 4;
    private const long MaxDirectPreviewEncodedBytes = 256L * 1024 * 1024;
    private readonly LinkedList<KeyValuePair<string, BitmapImage>> _previewCache = new();

    private async Task LoadShellThumbnailAsync(string path, string kind, double? modifiedAt, int navGen)
    {
        // BitmapImage is a DispatcherObject. The await on
        // GetThumbnailAsync can resume on a worker thread; constructing the
        // BitmapImage there is a known native-fast-fail shape —
        // RaiseFailFastException, no managed catch. Capture the dispatcher
        // before any await and marshal the BitmapImage construction +
        // PreviewImage.Source/Visibility assignment inside one TryEnqueue.
        var dispatcher = DispatcherQueue;
        var cacheKey = modifiedAt.HasValue ? $"{path}|{modifiedAt.Value:R}" : path;
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
                1024,
                Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale);
            if (thumb is { Size: > 0 } && dispatcher != null)
            {
                var bytes = await ReadThumbnailBytesAsync(thumb);
                if (await TryRenderPreviewBytesAsync(bytes, cacheKey, dispatcher, navGen, kind))
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
        if (kind == "image" && dispatcher != null && !_unloaded && _navGen == navGen
            && await TryDirectImageDecodeAsync(path, cacheKey, dispatcher, navGen))
        {
            return;
        }
        // Don't show the failure placeholder if we navigated away (or the stale
        // guard above set tcs=false for a superseded nav) — otherwise the prior
        // file's load clobbers the CURRENT sibling's preview with a placeholder.
        // A genuine decode failure on the still-current file falls through. (audit A9 re-audit)
        if (_unloaded || _navGen != navGen) return;
        ShowPlaceholder(kind, kind switch
        {
            "image" => "Image couldn't be decoded by any installed provider.",
            "pdf" => "PDF preview unavailable — open it in your default reader.",
            "doc" => "No preview available — Office handler may not be installed.",
            _ => "No preview available for this file type.",
        });
    }

    /// <summary>UI-thread-only. On a cache hit, marshal the cached BitmapImage
    /// onto PreviewImage and bump it to most-recently-used. Returns true when a
    /// cached preview was shown (caller skips the shell extract + decode).</summary>
    private static async Task<byte[]> ReadThumbnailBytesAsync(
        Windows.Storage.FileProperties.StorageItemThumbnail thumbnail)
    {
        thumbnail.Seek(0);
        var size = checked((uint)thumbnail.Size);
        using var reader = new Windows.Storage.Streams.DataReader(thumbnail.GetInputStreamAt(0));
        var loaded = await reader.LoadAsync(size);
        if (loaded != size)
        {
            throw new EndOfStreamException($"Shell thumbnail ended after {loaded} of {size} bytes.");
        }
        var bytes = new byte[size];
        reader.ReadBytes(bytes);
        return bytes;
    }

    private async Task<bool> TryDirectImageDecodeAsync(
        string path, string cacheKey, Microsoft.UI.Dispatching.DispatcherQueue dispatcher, int navGen)
    {
        Windows.Storage.Streams.IRandomAccessStream? stream = null;
        try
        {
            var length = new FileInfo(path).Length;
            if (length <= 0 || length > MaxDirectPreviewEncodedBytes)
            {
                Services.DebugLog.Warn($"FilePreviewSheet direct decode skipped encodedBytes={length}.");
                return false;
            }
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            stream = await file.OpenReadAsync();
            if (stream.Size == 0 || stream.Size > (ulong)MaxDirectPreviewEncodedBytes)
            {
                Services.DebugLog.Warn($"FilePreviewSheet direct decode skipped openedBytes={stream.Size}.");
                return false;
            }
            var accepted = await TryRenderPreviewStreamAsync(stream, cacheKey, dispatcher, navGen, "image-direct");
            if (accepted) stream = null;
            return accepted;
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn($"FilePreviewSheet direct decode open failed for {Services.PathRedactor.Redact(path)}: {ex.Message}");
            return false;
        }
        finally
        {
            try { stream?.Dispose(); } catch { }
        }
    }

    private async Task<bool> TryRenderPreviewStreamAsync(
        Windows.Storage.Streams.IRandomAccessStream stream,
        string cacheKey,
        Microsoft.UI.Dispatching.DispatcherQueue dispatcher,
        int navGen,
        string source)
    {
        var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var enqueued = dispatcher.TryEnqueue(() => _ = RenderPreviewStreamOnUiAsync(
            stream, cacheKey, navGen, source, tcs));
        if (!enqueued)
        {
            Services.DebugLog.Warn($"FilePreviewSheet dispatcher rejected {source} render.");
            return false;
        }
        return await tcs.Task;
    }

    private async Task<bool> TryRenderPreviewBytesAsync(
        byte[] bytes,
        string cacheKey,
        Microsoft.UI.Dispatching.DispatcherQueue dispatcher,
        int navGen,
        string source)
    {
        if (bytes.Length == 0) return false;
        var tcs = new TaskCompletionSource<bool>(TaskCreationOptions.RunContinuationsAsynchronously);
        var enqueued = dispatcher.TryEnqueue(() => _ = RenderPreviewBytesOnUiAsync(
            bytes, cacheKey, navGen, source, tcs));
        if (!enqueued)
        {
            Services.DebugLog.Warn($"FilePreviewSheet dispatcher rejected {source} render.");
            return false;
        }
        return await tcs.Task;
    }

    private async Task RenderPreviewBytesOnUiAsync(
        byte[] bytes,
        string cacheKey,
        int navGen,
        string source,
        TaskCompletionSource<bool> completion)
    {
        Windows.Storage.Streams.InMemoryRandomAccessStream? stream = null;
        try
        {
            if (_unloaded || _navGen != navGen)
            {
                completion.TrySetResult(false);
                return;
            }
            stream = new Windows.Storage.Streams.InMemoryRandomAccessStream();
            using (var writer = new Windows.Storage.Streams.DataWriter(stream.GetOutputStreamAt(0)))
            {
                writer.WriteBytes(bytes);
                await writer.StoreAsync();
                await writer.FlushAsync();
                writer.DetachStream();
            }
            stream.Seek(0);
            await RenderPreviewStreamCoreAsync(stream, cacheKey, navGen, source, completion, (ulong)bytes.Length);
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn($"FilePreviewSheet render failed source={source}: {ex.GetType().Name}: {ex.Message}");
            completion.TrySetResult(false);
        }
        finally
        {
            try { stream?.Dispose(); } catch { }
        }
    }

    private async Task RenderPreviewStreamOnUiAsync(
        Windows.Storage.Streams.IRandomAccessStream stream,
        string cacheKey,
        int navGen,
        string source,
        TaskCompletionSource<bool> completion)
    {
        try
        {
            stream.Seek(0);
            await RenderPreviewStreamCoreAsync(stream, cacheKey, navGen, source, completion, stream.Size);
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn($"FilePreviewSheet render failed source={source}: {ex.GetType().Name}: {ex.Message}");
            completion.TrySetResult(false);
        }
        finally
        {
            try { stream.Dispose(); } catch { }
        }
    }

    private async Task RenderPreviewStreamCoreAsync(
        Windows.Storage.Streams.IRandomAccessStream stream,
        string cacheKey,
        int navGen,
        string source,
        TaskCompletionSource<bool> completion,
        ulong encodedBytes)
    {
        if (_unloaded || _navGen != navGen)
        {
            completion.TrySetResult(false);
            return;
        }
        var bmp = new BitmapImage { DecodePixelWidth = 1024 };
        await bmp.SetSourceAsync(stream);
        if (_unloaded || _navGen != navGen)
        {
            completion.TrySetResult(false);
            return;
        }
        PreviewImage.Source = bmp;
        PreviewImage.Visibility = Visibility.Visible;
        StorePreview(cacheKey, bmp);
        Services.DebugLog.Debug($"FilePreviewSheet render completed source={source} bytes={encodedBytes} pixel={bmp.PixelWidth}x{bmp.PixelHeight}");
        completion.TrySetResult(true);
    }

    private bool TryShowCachedPreview(string cacheKey, Microsoft.UI.Dispatching.DispatcherQueue dispatcher, int navGen)
    {
        for (var node = _previewCache.First; node != null; node = node.Next)
        {
            if (node.Value.Key != cacheKey) continue;
            var bmp = node.Value.Value;
            _previewCache.Remove(node);
            _previewCache.AddFirst(node);
            var enqueued = dispatcher.TryEnqueue(() =>
            {
                if (_unloaded || _navGen != navGen) return; // stale navigation (audit A9)
                PreviewImage.Source = bmp;
                PreviewImage.Visibility = Visibility.Visible;
            });
            if (!enqueued)
            {
                Services.DebugLog.Warn("FilePreviewSheet dispatcher rejected cached preview render.");
            }
            return enqueued;
        }
        return false;
    }

    /// <summary>UI-thread-only (BitmapImage is a DispatcherObject). Insert at MRU
    /// front, dedupe the key, and evict the oldest beyond the cap.</summary>
    private void StorePreview(string cacheKey, BitmapImage bmp)
    {
        for (var node = _previewCache.First; node != null; node = node.Next)
        {
            if (node.Value.Key == cacheKey) { _previewCache.Remove(node); break; }
        }
        _previewCache.AddFirst(new KeyValuePair<string, BitmapImage>(cacheKey, bmp));
        while (_previewCache.Count > PreviewCacheCap)
        {
            _previewCache.RemoveLast();
        }
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
    // Bumped at the top of every SetFile. The image preview + metadata-card
    // loaders capture it and skip their UI write if it changed, so a slow async
    // result from a prior file can't overwrite the sibling the user navigated to.
    // (audit A9)
    private int _navGen;

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
            // The prior image frame is kept on screen until a new surface binds
            // (deferred-loading); collapse it now that the media surface is up.
            PreviewImage.Visibility = Visibility.Collapsed;
            // Ensure a MediaPlayer exists BEFORE binding the source: the
            // element creates one lazily, so `MediaPlayer is { }` could still
            // be null here — skipping the MediaFailed attach and leaving a
            // codec/DRM failure as a silent black surface with no message.
            if (PreviewMedia.MediaPlayer is null)
            {
                PreviewMedia.SetMediaPlayer(new Windows.Media.Playback.MediaPlayer());
            }
            PreviewMedia.Source = src;
            PreviewMedia.Visibility = Visibility.Visible;
            // Detach first so rapid sibling navigation can't multi-subscribe.
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
        // Fires on a Media Foundation worker thread. Marshal to UI thread before
        // touching XAML state. R6-05: capture the kind + media generation up front
        // so a navigation/close that races our failure can't stamp a stale
        // placeholder over the current sibling or touch a torn-down content tree.
        var kind = _currentMediaKind;
        var err = args.ErrorMessage ?? args.Error.ToString();
        var gen = _mediaGen;
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_unloaded || gen != _mediaGen) return; // superseded media failure
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
    /// or release the file handle. Only call on host close, not on sibling
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

    // ─── Proposed rename card ─────────────────────────────────────────
    private string? _pendingProposedName;

    private async System.Threading.Tasks.Task LoadProposedNameAsync(long fileId, int navGen)
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
                    using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
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
        // Defensive: sheet may have closed or navigated during the async query.
        // Reject the stale result before changing either backing state or XAML.
        if (_unloaded || _navGen != navGen) return;
        _pendingProposedName = name;
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
            // Capture the pre-rename name NOW for the session change log's
            // inverse action (renaming back is just another renameFiles).
            // Also capture the file id + navigation generation at CLICK time: a
            // quick navigate-to-next while the 30 s rename await is pending must
            // not let this completion hide/clear the NEWLY-displayed file's
            // proposed-rename card. (audit A9 — rename vs quick-navigate)
            var fileId = FileId;
            var navGenAtClick = _navGen;
            var oldName = System.IO.Path.GetFileName(FilePath);
            try
            {
                // Await the engine's BulkActionResult instead of fire-and-forget:
                // handle_rename_files reports per-file failure (unsafe name,
                // destination exists, source missing, DB-update failed) in the
                // result, not as a thrown exception. Collapsing the card on the
                // IPC send alone told the user the rename succeeded when it hadn't
                // (the silent-failure class). Mirrors BulkRenameSheet.
                var result = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                    "renameFiles",
                    () => ViewModels.EngineClient.Instance.RenameFilesAsync(new[]
                    {
                        new IpcSchema.RenameEntry(fileId, name!),
                    }),
                    TimeSpan.FromSeconds(30));
                // Succeeded==0 (with Failed==0) is the engine's wholesale-error
                // shape (e.g. a busy/locked DB → emit_bulk_result Ok(Err)); guard
                // it too so a total failure isn't treated as success.
                if (result.Failed > 0 || result.Succeeded == 0)
                {
                    // Keep the proposed-name card open so the user can see it
                    // didn't apply and retry, rather than silently vanishing.
                    Services.DebugLog.Warn("Apply rename reported failure; leaving card open");
                    return;
                }
                if (!string.IsNullOrEmpty(oldName))
                {
                    Services.UndoStack.Instance.Push(
                        $"rename “{oldName}”",
                        Services.ChangeKind.Rename,
                        async () =>
                        {
                            try
                            {
                                var undoResult = await ViewModels.EngineClient.Instance
                                    .WaitForBulkActionResultAsync(
                                        "renameFiles",
                                        () => ViewModels.EngineClient.Instance.RenameFilesAsync(new[]
                                        {
                                            new IpcSchema.RenameEntry(fileId, oldName),
                                        }),
                                        TimeSpan.FromSeconds(30));
                                return undoResult.Failed == 0 && undoResult.Succeeded > 0;
                            }
                            catch (Exception ex)
                            {
                                Services.DebugLog.Warn("Undo single rename failed: " + ex.Message);
                                return false;
                            }
                        });
                }
                // Only collapse the card / clear the pending name if the SAME
                // file is still displayed. If the user navigated to the next file
                // while the rename was in flight, that file's own proposed-rename
                // card (loaded by LoadProposedNameAsync) must be left intact.
                if (!_unloaded && _navGen == navGenAtClick && FileId == fileId)
                {
                    ProposedRenameCard.Visibility = Visibility.Collapsed;
                    _pendingProposedName = null;
                }
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

    // ─── Vision Tags card ─────────────────────────────────────────────
    // Reads every auto + user tag for this file from the DB and renders
    // them as TagChip controls inside VisionTagsHost. Mirrors the Library
    // card chip strip — same control, same formatting, same visual weight.
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
                        WHERE file_id = $id AND source IN ('auto','user','vlm')
                        ORDER BY CASE source WHEN 'user' THEN 0 WHEN 'vlm' THEN 1 ELSE 2 END,
                                 score DESC,
                                 rowid
                        """;
                    cmd.Parameters.AddWithValue("$id", fileId);
                    using var rdr = cmd.ExecuteReader();
                    var seen = new System.Collections.Generic.HashSet<string>(System.StringComparer.OrdinalIgnoreCase);
                    while (rdr.Read())
                    {
                        var tag = rdr.GetString(0).Trim();
                        if (tag.Length > 0 && seen.Add(tag)) list.Add(tag);
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
        if (_unloaded || _navGen != navGen) return; // navigated away during the await (audit A9)
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
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(chip, $"Tag: {formatted}");
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

    // While a run started from THIS sheet is in flight the button becomes
    // "Cancel analysis" — an accidental click must never cost a multi-minute
    // VLM run with no way out. Reset to idle by DeepAnalyzeComplete (see
    // OnEngineChangedForAnalyze), by navigation, or by the cancel itself.
    private bool _analyzeRunning;

    private async void OnAnalyzeClicked(object sender, RoutedEventArgs e)
    {
        if (FileId <= 0) return;
        if (_analyzeRunning)
        {
            try { await ViewModels.EngineClient.Instance.DeepAnalyzeCancelAsync(); }
            catch (Exception ex) { Services.DebugLog.Warn("Analyze cancel failed: " + ex); }
            SetAnalyzeIdle();
            return;
        }
        try
        {
            SetAnalyzeRunning();
            await ViewModels.EngineClient.Instance.DeepAnalyzeFileAsync(FileId, ViewModels.AppViewModel.Instance.Settings.SelectedVlmModelKind);
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("Analyze failed: " + ex);
            SetAnalyzeIdle();
        }
    }

    private void SetAnalyzeRunning()
    {
        _analyzeRunning = true;
        AnalyzeGlyph.Glyph = "\uE711"; // Cancel (X)
        AnalyzeLabel.Text = "Cancel analysis";
        ToolTipService.SetToolTip(AnalyzeButton, "Stop the Deep Analyze run for this file");
    }

    private void SetAnalyzeIdle()
    {
        _analyzeRunning = false;
        AnalyzeGlyph.Glyph = "\uE945";
        AnalyzeLabel.Text = "Analyze";
        ToolTipService.SetToolTip(AnalyzeButton, "Run Deep Analyze (VLM) on this file");
    }

    private void OnEngineChangedForAnalyze(object? s, System.ComponentModel.PropertyChangedEventArgs e)
        => Services.DebugLog.SafeRun("FilePreviewSheet.OnEngineChangedForAnalyze", () =>
        {
            if (e.PropertyName != nameof(ViewModels.EngineClient.DeepAnalyzeComplete)) return;
            Services.DebugLog.Debug($"[ENGINE-SUB:FilePreviewSheet] {e.PropertyName}");
            // Nominally already the UI thread (Apply dispatches there), but per
            // the platform rule treat that as untrusted: post XAML writes
            // through the captured dispatcher.
            DispatcherQueue?.TryEnqueue(() =>
            {
                if (_analyzeRunning) SetAnalyzeIdle();
            });
        });

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
        // Capture the file id + navigation generation at CLICK time so a quick
        // navigate-to-next during the 30 s tag await doesn't render this file's
        // status message (or clear the input) under the newly-displayed file.
        var fileId = FileId;
        var navGenAtClick = _navGen;
        bool StillCurrent() => !_unloaded && _navGen == navGenAtClick && FileId == fileId;
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
            var priorUserTags = await Services.TagChangeJournal
                .CapturePriorUserTagsAsync(new[] { fileId });
            // Await the engine's BulkActionResult instead of fire-and-forget:
            // declaring success on the IPC send alone told the user the tags
            // applied even when the engine reported failure (the silent-failure
            // class). Mirrors OnApplyRenameClicked above.
            var result = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                "applyTags",
                () => ViewModels.EngineClient.Instance.ApplyTagsAsync(new long[] { fileId }, tags, mode: "add"),
                TimeSpan.FromSeconds(30));
            var confirmedFileIds = Services.BulkActionResultTruth
                .ConfirmedSuccessfulFileIds(result, new[] { fileId });
            if (confirmedFileIds.Count > 0)
            {
                Services.TagChangeJournal.PushUndo(
                    Services.TagChangeJournal.FormatLabel("add", confirmedFileIds.Count),
                    confirmedFileIds,
                    priorUserTags);
            }
            // Navigated away while tagging was in flight — the tags still applied
            // to the captured file, but its status/input mutations would land on
            // the wrong (now-displayed) file. Skip the UI update.
            if (!StillCurrent()) return;
            // Also guard Succeeded==0 (the engine's wholesale-error shape, e.g. a
            // busy/locked DB), so a total failure isn't reported as "Added N tags".
            if (result.Failed > 0 || result.Succeeded == 0)
            {
                var first = result.Messages?.FirstOrDefault(m => !m.Ok)?.Message;
                ShowTagStatus(string.IsNullOrWhiteSpace(first) ? "Couldn't apply tags." : "Failed: " + first);
                return;
            }
            ShowTagStatus($"Added {tags.Length} tag" + (tags.Length == 1 ? string.Empty : "s") + ".");
            TagInput.Text = string.Empty;
            _ = LoadVisionTagsAsync(fileId, navGenAtClick);
        }
        catch (TimeoutException ex)
        {
            Services.DebugLog.Warn("FilePreviewSheet.ApplyDraftTags timed out: " + ex.Message);
            if (StillCurrent()) ShowTagStatus("Tagging didn't confirm — try again.");
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("FilePreviewSheet.ApplyDraftTags failed: " + ex);
            if (StillCurrent()) ShowTagStatus("Failed: " + ex.Message);
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
