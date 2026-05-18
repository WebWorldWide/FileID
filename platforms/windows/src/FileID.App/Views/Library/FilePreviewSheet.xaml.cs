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
        KeyDown += OnKeyDown;
        // media playback must stop when the sheet unloads.
        // MediaPlayerElement holds an IMediaPlaybackSource that pins the
        // file handle — without an explicit pause the audio keeps playing
        // after the dialog dismisses.
        Unloaded += (_, _) => StopAndClearMedia();
        IsTabStop = true;
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

    private void OnKeyDown(object sender, Microsoft.UI.Xaml.Input.KeyRoutedEventArgs e)
    {
        switch (e.Key)
        {
            case Windows.System.VirtualKey.Left:
                NavigateSibling(-1);
                e.Handled = true;
                break;
            case Windows.System.VirtualKey.Right:
                NavigateSibling(+1);
                e.Handled = true;
                break;
            case Windows.System.VirtualKey.Escape:
                RaiseClose();
                e.Handled = true;
                break;
        }
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
        FacesBadge.Visibility = hasFaces ? Visibility.Visible : Visibility.Collapsed;
        TextBadge.Visibility = hasText ? Visibility.Visible : Visibility.Collapsed;

        TagInput.Text = string.Empty;
        TagStatusText.Visibility = Visibility.Collapsed;

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
                LoadVideoOrAudio(path, kind);
                break;
            case "audio":
                LoadVideoOrAudio(path, kind);
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
    private void LoadVideoOrAudio(string path, string kind)
    {
        try
        {
            _currentMediaKind = kind;
            var uri = new Uri(path, UriKind.Absolute);
            PreviewMedia.Source = Windows.Media.Core.MediaSource.CreateFromUri(uri);
            PreviewMedia.Visibility = Visibility.Visible;
            // Attach failure handler on the MediaPlayer (it's lazily created
            // when Source is set). Detach the previous subscription so we
            // don't multi-fire on rapid sibling navigation.
            if (PreviewMedia.MediaPlayer is { } mp)
            {
                mp.MediaFailed -= OnMediaPlayerFailed;
                mp.MediaFailed += OnMediaPlayerFailed;
            }
        }
        catch (Exception ex)
        {
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
            _currentMediaKind = string.Empty;
        }
        catch (Exception ex)
        {
            Services.DebugLog.Warn("FilePreviewSheet.StopAndClearMedia: " + ex.Message);
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
        if (FacesBadge.Visibility == Visibility.Visible) AddMetadataRow("Faces", "Detected");
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
            await ViewModels.EngineClient.Instance.DeepAnalyzeFileAsync(FileId, "qwen2_5_vl_3b");
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
