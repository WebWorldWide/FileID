using System.IO;
using FileID.IpcSchema;
using FileID.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Media.Core;
using Windows.Media.Playback;
using Windows.Storage;

namespace FileID.Views.Workbenches;

internal sealed class EvidencePanel : StackPanel
{
    private readonly TextBlock _label = new() { TextWrapping = TextWrapping.Wrap, IsTextSelectionEnabled = true };
    private readonly MediaPlayerElement _preview = new() { Height = 220, AreTransportControlsEnabled = true, Visibility = Visibility.Collapsed };
    private readonly Action<string> _status;
    private MediaPlayer? _player;
    private CatalogHit? _hit;
    private int _revision;
    private readonly Button _open;
    private readonly Button _reveal;
    private static readonly string[] MediaExtensions = { ".mov", ".mp4", ".m4v", ".mkv", ".avi", ".webm", ".mp3", ".m4a", ".wav", ".aac", ".flac", ".wma" };

    public EvidencePanel(Action<string> status)
    {
        _status = status;
        Spacing = 8;
        Children.Add(_label);
        _open = new Button { Content = "Open", IsEnabled = false };
        _open.Click += async (_, _) =>
        {
            try { await OpenAsync(); }
            catch (Exception) { _status("This file can't be opened. Its drive may be disconnected."); }
        };
        _reveal = new Button { Content = "Show in Explorer", IsEnabled = false };
        _reveal.Click += (_, _) => DebugLog.SafeRun("Evidence.Reveal", () =>
        {
            if (_hit is { } hit) SafeOpen.Reveal(hit.Path);
        });
        Children.Add(new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Children = { _open, _reveal } });
        Children.Add(_preview);
        Unloaded += (_, _) => Stop();
    }

    /// <summary>The inline player while a media source plays here, else null.</summary>
    public MediaPlayer? Player => _player;

    /// <summary>Raised on the UI thread when the inline player starts or stops.</summary>
    public event EventHandler? PlayerChanged;

    public void HideLabel() => _label.Visibility = Visibility.Collapsed;

    /// <summary>Moves the inline player to <paramref name="seconds"/>, or opens the
    /// selected media there when nothing is playing yet.</summary>
    public async Task SeekAsync(double seconds)
    {
        if (!double.IsFinite(seconds) || seconds < 0) seconds = 0;
        if (_player is { } player)
        {
            player.PlaybackSession.Position = TimeSpan.FromSeconds(seconds);
            return;
        }
        try { await OpenAsync(seconds); }
        catch (Exception) { _status("This file can't be opened. Its drive may be disconnected."); }
    }

    public static string Label(CatalogHit hit) =>
        $"{hit.Path}\n{KindLabel(hit.Kind)}"
        + (hit.StartSeconds is { } seconds ? $" · {seconds:0.0} s" : "")
        + (hit.Page is { } page ? $" · Page {page}" : "") + $"\n{hit.Text}";

    private static string KindLabel(string kind) => kind switch
    {
        "sampledFrame" => "Video frame, not checked",
        "chapter" => "Chapter",
        "passage" => "Speech or text",
        "person" => "Person",
        "documentText" => "Text in document",
        "ocrText" => "Text in image",
        "handwrittenText" => "Handwritten text",
        _ => kind,
    };

    public void Select(CatalogHit? hit)
    {
        Stop();
        _hit = hit;
        _label.Text = hit is null ? "Results show here: matching file names, descriptions, chapters and moments inside videos." : Label(hit);
        _open.IsEnabled = hit is not null;
        _reveal.IsEnabled = hit is not null;
        _open.Content = hit is null || !IsMedia(hit.Path)
            ? "Open"
            : hit.StartSeconds is > 0 ? "Play from match" : "Play from start";
    }

    private static bool IsMedia(string path) =>
        MediaExtensions.Contains(Path.GetExtension(path), StringComparer.OrdinalIgnoreCase);

    private void Stop()
    {
        _revision++;
        var player = _player;
        _player = null;
        _preview.SetMediaPlayer(null);
        player?.Dispose();
        _preview.Visibility = Visibility.Collapsed;
        if (player is not null) PlayerChanged?.Invoke(this, EventArgs.Empty);
    }

    private async Task OpenAsync(double? start = null)
    {
        var hit = _hit;
        if (hit is null) return;
        var revision = _revision;
        if (!File.Exists(hit.Path)) { _status("This file can't be found. Its drive may be disconnected."); return; }
        if (IsMedia(hit.Path))
        {
            double seconds = (start ?? hit.StartSeconds) is { } s && double.IsFinite(s) && s >= 0 ? s : 0;
            var file = await StorageFile.GetFileFromPathAsync(hit.Path);
            if (revision != _revision) return;
            Stop();
            var playbackRevision = _revision;
            var player = new MediaPlayer { AutoPlay = false };
            player.MediaOpened += (_, _) => DebugLog.SafeRun("Evidence.MediaOpened", () => DispatcherQueue.TryEnqueue(() =>
            {
                if (_revision != playbackRevision) return;
                player.PlaybackSession.Position = TimeSpan.FromSeconds(seconds);
            }));
            player.MediaFailed += (_, _) => DebugLog.SafeRun("Evidence.MediaFailed", () => DispatcherQueue.TryEnqueue(() =>
            {
                if (_revision == playbackRevision) _status("Windows cannot play this media format. No timestamp playback is available for this file.");
            }));
            _player = player;
            _preview.SetMediaPlayer(player);
            _preview.Visibility = Visibility.Visible;
            player.Source = MediaSource.CreateFromStorageFile(file);
            PlayerChanged?.Invoke(this, EventArgs.Empty);
        }
        else if (!SafeOpen.TryOpenFile(hit.Path))
        {
            SafeOpen.Reveal(hit.Path);
            _status("Default opening is unavailable for this file; requested Reveal in Explorer.");
        }
    }
}
