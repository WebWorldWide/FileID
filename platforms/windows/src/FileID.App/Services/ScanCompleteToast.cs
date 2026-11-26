// Fires a Windows shell toast when a scan completes. Started once at app startup and lives for the process; the toast text stays local.

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

    private static void Show(ScanComplete result)
    {
        try
        {
            var doc = new XmlDocument();
            var body = $"{result.ProcessedFiles:N0} files ready to browse.";
            var safeFailed = result.FailedFiles > 0 ? $" {result.FailedFiles:N0} couldn't be read." : string.Empty;
            doc.LoadXml($"""
                <toast>
                  <visual>
                    <binding template="ToastGeneric">
                      <text>Scan complete</text>
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
        }
    }

    private static string Escape(string s) =>
        System.Net.WebUtility.HtmlEncode(s);
}
