// WelcomeSheet code-behind. Subscribes to ModelInstallerService and the
// EngineClient's ModelDownloadProgress to keep the three rows in sync.
//
// Auto-dismisses when AllInstalled. The "Skip for now" button is always
// available — users who don't want any models installed can dismiss
// immediately and run a CLIP-less / VLM-less FileID (Library tab works
// without CLIP via FTS5; Deep Analyze is gated until VLM lands).

using System.ComponentModel;
using FileID.Services;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views;

public sealed partial class WelcomeSheet : UserControl
{
    /// <summary>Raised when the user clicks Skip OR all models finish installing.</summary>
    public event EventHandler? Dismissed;

    public WelcomeSheet()
    {
        InitializeComponent();
        Loaded += (_, _) =>
        {
            ModelInstallerService.Instance.PropertyChanged += OnInstallerChanged;
            Sync();
        };
        Unloaded += (_, _) =>
        {
            ModelInstallerService.Instance.PropertyChanged -= OnInstallerChanged;
        };
    }

    private void OnInstallerChanged(object? sender, PropertyChangedEventArgs e)
    {
        DispatcherQueue.TryEnqueue(Sync);
    }

    private void Sync()
    {
        var svc = ModelInstallerService.Instance;
        ApplyRow(ClipStatusIcon, ClipProgressBar, svc.ClipStatus, svc.ClipProgress);
        ApplyRow(ArcfaceStatusIcon, ArcfaceProgressBar, svc.ArcfaceStatus, svc.ArcfaceProgress);
        ApplyRow(VlmStatusIcon, VlmProgressBar, svc.VlmStatus, svc.VlmProgress);

        InstallAllButton.IsEnabled = !(svc.ClipStatus == ModelInstallStatus.Downloading
                                    || svc.ArcfaceStatus == ModelInstallStatus.Downloading
                                    || svc.VlmStatus == ModelInstallStatus.Downloading);

        if (!string.IsNullOrEmpty(svc.StatusMessage))
        {
            StatusMessage.Text = svc.StatusMessage;
            StatusMessage.Visibility = Visibility.Visible;
        }
        else
        {
            StatusMessage.Visibility = Visibility.Collapsed;
        }

        if (svc.AllInstalled)
        {
            Dismissed?.Invoke(this, EventArgs.Empty);
        }
    }

    // Segoe Fluent Icons code points. Spelled as `\uXXXX` so the bytes
    // survive any source-control or editor encoding step that previously
    // emptied raw glyph chars.
    private const string GlyphCheck    = ""; // CheckMark
    private const string GlyphDownload = ""; // Download
    private const string GlyphError    = ""; // Warning / error
    private const string GlyphCloud    = ""; // CloudDownload (NotInstalled hint)

    private static void ApplyRow(FontIcon icon, ProgressBar bar, ModelInstallStatus status, double progress)
    {
        var goldBrush = (Microsoft.UI.Xaml.Media.SolidColorBrush)Application.Current.Resources["GoldBrush"];
        switch (status)
        {
            case ModelInstallStatus.Installed:
                icon.Glyph = GlyphCheck;
                icon.Foreground = goldBrush;
                bar.Visibility = Visibility.Collapsed;
                break;
            case ModelInstallStatus.Downloading:
                icon.Glyph = GlyphDownload;
                icon.Foreground = goldBrush;
                bar.Visibility = Visibility.Visible;
                bar.Value = progress;
                break;
            case ModelInstallStatus.Failed:
                icon.Glyph = GlyphError;
                icon.Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(
                    Windows.UI.Color.FromArgb(0xFF, 0xE5, 0x55, 0x55));
                bar.Visibility = Visibility.Collapsed;
                break;
            default:
                icon.Glyph = GlyphCloud;
                icon.Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(
                    Windows.UI.Color.FromArgb(0x99, 0xFF, 0xFF, 0xFF));
                bar.Visibility = Visibility.Collapsed;
                break;
        }
    }

    private void OnInstallAllClicked(object sender, RoutedEventArgs e)
    {
        // Truly fire-and-forget. Awaiting on the UI thread blocks window
        // chrome (drag, resize) for the duration of all three IPC writes
        // even if each is fast — and any synchronous step downstream (a
        // lock contention, a dispatcher hop) compounds into a freeze.
        // The downloads themselves run in the engine; the app only fires
        // the start signal. UI updates flow back through the existing
        // ModelInstallerService PropertyChanged → DispatcherQueue path.
        _ = Task.Run(async () =>
        {
            try
            {
                await ModelInstallerService.Instance.InstallAllAsync().ConfigureAwait(false);
            }
            catch (Exception ex)
            {
                DebugLog.Warn("InstallAllAsync background task threw: " + ex);
            }
        });
    }

    private void OnSkipClicked(object sender, RoutedEventArgs e)
    {
        Dismissed?.Invoke(this, EventArgs.Empty);
    }
}
