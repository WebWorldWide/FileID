// Fires a Windows shell toast when a scan completes. We use the raw
// ToastNotification API (no Microsoft.Toolkit.Uwp.Notifications dep —
// that package adds a 600KB transitive surface that we don't need for
// the few toasts FileID raises).
//
// Subscribe at app startup; unsubscribe never (lives for the process).
//
// PRIVACY: toast text is local, never sent anywhere. No registration with
// Notification Hub / WNS — we just push to the local Action Center.

using System;
using System.Reactive.Linq;
using FileID.IpcSchema;
using FileID.ViewModels;
using Windows.Data.Xml.Dom;
using Windows.UI.Notifications;

namespace FileID.Services;

public static class ScanCompleteToast
{
    private static IDisposable? _sub;

    public static void Start()
    {
        if (_sub is not null) return;
        _sub = EngineClient.Instance.Events
            .OfType<IpcEvent>()
            .Subscribe(ev =>
            {
                if (ev.Payload is ScanCompleteEvent sc)
                {
                    Show(sc.Result);
                }
            });
    }

    /// <summary>
    /// Disposes the Rx subscription. Call from app shutdown (App.OnSuspending
    /// or equivalent) so the lambda doesn't outlive the process intent.
    /// </summary>
    public static void Stop()
    {
        _sub?.Dispose();
        _sub = null;
    }

    private static void Show(ScanComplete result)
    {
        try
        {
            var doc = new XmlDocument();
            var seconds = result.TotalSeconds;
            var perSec = seconds > 0 ? result.ProcessedFiles / seconds : 0;
            var body = $"Processed {result.ProcessedFiles} files in {seconds:0.#}s ({perSec:0} files/s).";
            var safeFailed = result.FailedFiles > 0 ? $"  {result.FailedFiles} failed." : string.Empty;
            doc.LoadXml($"""
                <toast>
                  <visual>
                    <binding template="ToastGeneric">
                      <text>FileID scan complete</text>
                      <text>{Escape(body + safeFailed)}</text>
                    </binding>
                  </visual>
                </toast>
                """);
            var toast = new ToastNotification(doc);
            ToastNotificationManager.CreateToastNotifier().Show(toast);
        }
        catch
        {
            // Toast subsystem may be disabled (group policy / focus assist).
            // Best-effort — failure is non-critical.
        }
    }

    private static string Escape(string s) =>
        System.Net.WebUtility.HtmlEncode(s);
}
