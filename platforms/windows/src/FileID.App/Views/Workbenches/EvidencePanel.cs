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
            catch (Exception) { _status("This catalog file is offline, unavailable, or cannot be opened."); }
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

    public static string Label(CatalogHit hit) =>
        $"{hit.Path}\n{(hit.Kind == "sampledFrame" ? "Unverified sampled frame" : hit.Kind)}"
        + (hit.StartSeconds is { } seconds ? $" · {seconds:0.0} s" : "")
        + (hit.Page is { } page ? $" · Page {page}" : "") + $"\n{hit.Text}";

    public void Select(CatalogHit? hit)
    {
        Stop();
        _hit = hit;
        _label.Text = hit is null ? "Select catalog evidence." : Label(hit);
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
        _player?.Dispose();
        _player = null;
        _preview.SetMediaPlayer(null);
        _preview.Visibility = Visibility.Collapsed;
    }

    private async Task OpenAsync()
    {
        var hit = _hit;
        if (hit is null) return;
        var revision = _revision;
        if (!File.Exists(hit.Path)) { _status("This catalog file is offline or unavailable."); return; }
        if (IsMedia(hit.Path))
        {
            double seconds = hit.StartSeconds is { } s && double.IsFinite(s) && s >= 0 ? s : 0;
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
        }
        else if (!SafeOpen.TryOpenFile(hit.Path))
        {
            SafeOpen.Reveal(hit.Path);
            _status("Default opening is unavailable for this file; requested Reveal in Explorer.");
        }
    }
}
