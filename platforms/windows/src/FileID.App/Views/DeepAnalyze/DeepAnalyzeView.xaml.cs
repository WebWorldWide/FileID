// DeepAnalyzeView code-behind. Subscribes to EngineClient observables
// + ModelInstallerService for the per-model install state. Drives the
// llama.cpp runtime install, model install, full-library/per-file
// analyze, cancel, and renders the live caption stream as tokens
// arrive from the engine.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Views.DeepAnalyze;

public sealed partial class DeepAnalyzeView : UserControl
{
    private string _activeModel = "qwen2_5_vl_3b";
    private string _captionAccumulator = string.Empty;

    public DeepAnalyzeView()
    {
        InitializeComponent();
        Loaded += OnLoadedHandler;
        Unloaded += OnUnloadedHandler;
    }

    private void OnUnloadedHandler(object sender, RoutedEventArgs e)
    {
        // Use named handlers (not lambdas) so -= actually unregisters.
        // Lambdas create a fresh delegate object each time, so -= silently
        // misses the original subscription → leaked event listeners.
        ModelInstallerService.Instance.Vlm.PropertyChanged -= OnInstallerChanged;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        Loaded -= OnLoadedHandler;
        Unloaded -= OnUnloadedHandler;
    }

    private void OnLoadedHandler(object sender, RoutedEventArgs e)
    {
        ModelInstallerService.Instance.Vlm.PropertyChanged += OnInstallerChanged;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        SyncCards();
        UpdateActiveModelLabel();
        // V14.9-C5: refresh the "Name people first" gate every time the
        // view loads; also refreshed in OnEngineChanged when face
        // clustering finishes.
        _ = RefreshNamePeopleGateAsync();
    }

    /// <summary>V14.9-C5: query the DB for any person row with NULL
    /// name + first_name. Disables Analyze All + shows the gate banner
    /// when the count is non-zero.</summary>
    private async System.Threading.Tasks.Task RefreshNamePeopleGateAsync()
    {
        int unnamed = 0;
        try
        {
            var dbPath = AppPaths.DbPath;
            unnamed = await System.Threading.Tasks.Task.Run(() =>
            {
                try
                {
                    if (!System.IO.File.Exists(dbPath)) return 0;
                    var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                        new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                        {
                            DataSource = dbPath,
                            Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                        }.ToString());
                    conn.Open();
                    using var cmd = conn.CreateCommand();
                    // A cluster is "unnamed" when both `name` (legacy) and
                    // `first_name` (v5) are NULL — the display falls back
                    // to "Person N" in PeopleViewModel.
                    cmd.CommandText = "SELECT COUNT(*) FROM persons WHERE name IS NULL AND first_name IS NULL;";
                    var result = cmd.ExecuteScalar();
                    return result is null ? 0 : Convert.ToInt32(result);
                }
                catch { return 0; }
            }).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("RefreshNamePeopleGateAsync failed: " + ex.Message);
            unnamed = 0;
        }
        // Marshal UI updates back.
        DispatcherQueue.TryEnqueue(() =>
        {
            if (unnamed > 0)
            {
                NamePeopleGateBanner.Visibility = Visibility.Visible;
                NamePeopleGateText.Text = unnamed == 1
                    ? "1 face cluster doesn't have a name yet. Name it for sharper captions."
                    : $"{unnamed} face clusters don't have names yet. Name them for sharper captions.";
                AnalyzeAllButton.IsEnabled = false;
                ToolTipService.SetToolTip(AnalyzeAllButton,
                    "Name unnamed people first so smart-name proposals can use their names.");
            }
            else
            {
                NamePeopleGateBanner.Visibility = Visibility.Collapsed;
                AnalyzeAllButton.IsEnabled = true;
                ToolTipService.SetToolTip(AnalyzeAllButton, null);
            }
        });
    }

    private void OnGoToPeopleClicked(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.ActiveTab = SidebarTab.People;
    }

    private void OnInstallerChanged(object? sender, PropertyChangedEventArgs e)
        => DispatcherQueue.TryEnqueue(SyncCards);

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        switch (e.PropertyName)
        {
            case nameof(EngineClient.DeepAnalyzeStarting):
            case nameof(EngineClient.DeepAnalyzeProgress):
            case nameof(EngineClient.DeepAnalyzeLast):
            case nameof(EngineClient.DeepAnalyzeComplete):
                DispatcherQueue.TryEnqueue(SyncStream);
                break;
            // V14.9-C5: re-evaluate the "Name people first" gate every
            // time face clustering re-runs (new clusters → new unnamed)
            // or a scan completes (new files → maybe new clusters next
            // run).
            case nameof(EngineClient.Phase):
            case nameof(EngineClient.LastFaceClustering):
                _ = RefreshNamePeopleGateAsync();
                break;
        }
    }

    private void SyncCards()
    {
        var slot = ModelInstallerService.Instance.Vlm;
        ApplyCard(QwenSmallStatus, QwenSmallProgress, QwenSmallInstallButton, slot.Status, slot.Fraction);
        ApplyCard(QwenLargeStatus, QwenLargeProgress, QwenLargeInstallButton, slot.Status, slot.Fraction);
        ApplyCard(SmolVlmStatus, SmolVlmProgress, SmolVlmInstallButton, slot.Status, slot.Fraction);
        HighlightActiveCard();
    }

    private static void ApplyCard(TextBlock status, ProgressBar bar, Button installButton, ModelInstallStatus s, double progress)
    {
        switch (s)
        {
            case ModelInstallStatus.Installed:
                status.Text = "Installed";
                bar.Visibility = Visibility.Collapsed;
                installButton.Content = "Reinstall";
                installButton.IsEnabled = true;
                break;
            case ModelInstallStatus.Downloading:
                status.Text = $"Downloading… {Math.Round(progress * 100)}%";
                bar.Visibility = Visibility.Visible;
                bar.Value = progress;
                installButton.IsEnabled = false;
                break;
            case ModelInstallStatus.Failed:
                status.Text = "Install failed — retry?";
                bar.Visibility = Visibility.Collapsed;
                installButton.IsEnabled = true;
                break;
            default:
                status.Text = string.Empty;
                bar.Visibility = Visibility.Collapsed;
                installButton.IsEnabled = true;
                break;
        }
    }

    private void HighlightActiveCard()
    {
        var idle = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"];
        var gold = (Brush)Application.Current.Resources["GoldBrush"];
        QwenSmallCard.BorderBrush = _activeModel == "qwen2_5_vl_3b" ? gold : idle;
        QwenLargeCard.BorderBrush = _activeModel == "qwen2_5_vl_7b" ? gold : idle;
        SmolVlmCard.BorderBrush = _activeModel == "smolvlm" ? gold : idle;
        QwenSmallCard.BorderThickness = _activeModel == "qwen2_5_vl_3b" ? new Thickness(2) : new Thickness(1);
        QwenLargeCard.BorderThickness = _activeModel == "qwen2_5_vl_7b" ? new Thickness(2) : new Thickness(1);
        SmolVlmCard.BorderThickness = _activeModel == "smolvlm" ? new Thickness(2) : new Thickness(1);
    }

    private void UpdateActiveModelLabel()
    {
        ActiveModelText.Text = _activeModel switch
        {
            "qwen2_5_vl_7b" => "Active model: Qwen 2.5-VL 7B (best quality)",
            "smolvlm" => "Active model: SmolVLM 256M (fastest)",
            _ => "Active model: Qwen 2.5-VL 3B (balanced)",
        };
    }

    private int _proposedNameCount;

    private void SyncStream()
    {
        var ec = EngineClient.Instance;
        var starting = ec.DeepAnalyzeStarting;
        var prog = ec.DeepAnalyzeProgress;
        var last = ec.DeepAnalyzeLast;
        var complete = ec.DeepAnalyzeComplete;

        if (starting is null && prog is null && last is null && complete is null) return;

        // V14.9-D5: starting-card pre-progress. Engine emits
        // DeepAnalyzeStarting with phase = Queued / Loading / Resolving
        // BEFORE the first DeepAnalyzeProgress event. Surface the phase
        // text so the user knows we're not stalled while the VLM warms
        // up (~5-30 s on first run).
        if (starting is not null && prog is null)
        {
            StreamCard.Visibility = Visibility.Visible;
            CancelButton.IsEnabled = true;
            AnalyzeAllButton.IsEnabled = false;
            StreamFileNameText.Text = $"{starting.Phase}: {starting.ModelKind}";
            StreamCaptionText.Text = starting.Message ?? string.Empty;
            StreamProposedNameText.Text = string.Empty;
            OverallProgress.Value = 0;
            OverallProgress.IsIndeterminate = true;
            OverallProgressText.Text = "Preparing…";
        }

        if (prog is not null)
        {
            StreamCard.Visibility = Visibility.Visible;
            CancelButton.IsEnabled = true;
            AnalyzeAllButton.IsEnabled = false;
            OverallProgress.IsIndeterminate = false;

            var pct = prog.Total == 0 ? 0 : (double)prog.Processed / prog.Total;
            OverallProgress.Value = pct;
            // V14.9-D3: include ETA + processed/total + per-second rate
            // when the engine reports it.
            var etaSuffix = prog.EtaSeconds is double eta && eta > 0
                ? $" · {FormatEta(eta)} left"
                : string.Empty;
            OverallProgressText.Text = $"{prog.Processed} / {prog.Total} files{etaSuffix}";

            if (!string.IsNullOrEmpty(prog.CurrentPath))
            {
                StreamFileNameText.Text = Path.GetFileName(prog.CurrentPath);
                _ = LoadStreamThumbAsync(prog.CurrentPath);
                _captionAccumulator = string.Empty;
                StreamCaptionText.Text = string.Empty;
            }
            // V14.9-I: live caption stream. Engine emits the partial
            // accumulated text at 4 Hz; show it directly in the caption
            // line so the user sees the model generating word-by-word.
            if (!string.IsNullOrEmpty(prog.CurrentCaption))
            {
                _captionAccumulator = prog.CurrentCaption!;
                StreamCaptionText.Text = prog.CurrentCaption!;
            }
        }

        if (last is not null)
        {
            StreamCaptionText.Text = last.Description ?? string.Empty;
            if (!string.IsNullOrEmpty(last.ProposedName))
            {
                StreamProposedNameText.Text = $"Proposed name: {last.ProposedName}";
                _proposedNameCount++;
                SyncProposedNamesPill();
            }
            else
            {
                StreamProposedNameText.Text = string.Empty;
            }
        }

        if (complete is not null)
        {
            CancelButton.IsEnabled = false;
            AnalyzeAllButton.IsEnabled = true;
            OverallProgress.IsIndeterminate = false;
            OverallProgressText.Text = complete.Cancelled
                ? $"Cancelled ({complete.Processed} done, {complete.Failed} failed)"
                : $"Done — {complete.Processed} captioned in {complete.TotalSeconds:0.#}s ({complete.Failed} failed)";
            SyncProposedNamesPill();
        }
    }

    /// <summary>V14.9-D4: smart-names pending-rename pill. Shows the
    /// running count of ProposedName values the engine has produced
    /// during this Deep Analyze run. Tap routes to BulkRenameSheet to
    /// apply or discard them.</summary>
    private void SyncProposedNamesPill()
    {
        if (ProposedNamesPill == null) return;
        if (_proposedNameCount > 0)
        {
            ProposedNamesPill.Visibility = Visibility.Visible;
            ProposedNamesPillText.Text = _proposedNameCount == 1
                ? "1 smart name pending rename"
                : $"{_proposedNameCount} smart names pending rename";
        }
        else
        {
            ProposedNamesPill.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>V14.9-I: open BulkRenameSheet pre-seeded with every
    /// VLM-proposed rename pending in the DB. One-click bulk-apply
    /// of the model's smart filename suggestions, no need to
    /// navigate to Library + multi-select first.</summary>
    private async void OnProposedNamesPillClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            var store = new Services.ReadStore(Services.AppPaths.DbPath);
            await store.OpenAsync();
            var pending = await store.PendingProposedRenamesAsync(500, System.Threading.CancellationToken.None);
            if (pending.Count == 0)
            {
                _proposedNameCount = 0;
                SyncProposedNamesPill();
                return;
            }
            var plan = new System.Collections.Generic.List<Views.Library.BulkRenameSheet.RenamePlan>(pending.Count);
            foreach (var p in pending)
            {
                plan.Add(new Views.Library.BulkRenameSheet.RenamePlan
                {
                    FileId = p.Id,
                    CurrentPath = p.Path,
                    ProposedName = p.ProposedName,
                    Include = true,
                });
            }
            var sheet = new Views.Library.BulkRenameSheet();
            sheet.SetPlan(plan);
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = $"Apply {pending.Count} smart rename{(pending.Count == 1 ? "" : "s")}",
                Content = sheet,
                PrimaryButtonText = "Rename",
                CloseButtonText = "Close",
                DefaultButton = ContentDialogButton.Primary,
            };
            dialog.PrimaryButtonClick += async (_, args) =>
            {
                var deferral = args.GetDeferral();
                var ok = await sheet.CommitAsync();
                if (!ok) args.Cancel = true;
                deferral.Complete();
            };
            try { await dialog.ShowAsync(); } catch { /* dialog already open */ }
            // Refresh count after possible apply.
            var remaining = await store.PendingProposedRenameCountAsync(System.Threading.CancellationToken.None);
            _proposedNameCount = remaining;
            SyncProposedNamesPill();
            store.Dispose();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnProposedNamesPillClicked threw: " + ex.Message);
        }
    }

    private static string FormatEta(double seconds)
    {
        if (seconds < 60) return $"{seconds:F0}s";
        if (seconds < 3600) return $"{seconds / 60:F0}m";
        var hours = seconds / 3600;
        if (hours > 99) return "99+h";
        return $"{hours:F1}h";
    }

    private async System.Threading.Tasks.Task LoadStreamThumbAsync(string path)
    {
        // V14.9-A17: swallowing all exceptions left the image at whatever
        // previous frame was shown — confusing when the user can see the
        // filename advance but the thumb sticks. Clear the source on any
        // failure so it falls back to the placeholder glyph instead.
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            using var thumb = await file.GetThumbnailAsync(
                Windows.Storage.FileProperties.ThumbnailMode.SingleItem, 320,
                Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale);
            if (thumb != null && thumb.Size > 0)
            {
                var bmp = new BitmapImage();
                await bmp.SetSourceAsync(thumb);
                StreamImage.Source = bmp;
                return;
            }
            StreamImage.Source = null;
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"LoadStreamThumbAsync({PathRedactor.Redact(path)}) failed: {ex.Message}");
            try { StreamImage.Source = null; } catch { }
        }
    }

    private void OnModelCardTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        if (sender is FrameworkElement el && el.Tag is string id)
        {
            _activeModel = id;
            HighlightActiveCard();
            UpdateActiveModelLabel();
        }
    }

    // Every `async void` handler below has the entire body inside a
    // try/catch. An async-void handler that throws kills the dispatcher
    // and crashes the window; the catch makes failures surface as log
    // entries instead.
    private async void OnInstallModelClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            // V14.7.6: previous version ignored the Tag and ALWAYS installed
            // qwen2_5_vl_3b. Now uses the per-card model id from Tag so each
            // model card actually installs its own model.
            if (sender is not Button b || b.Tag is not string modelId || string.IsNullOrWhiteSpace(modelId)) return;
            await EngineClient.Instance.PrewarmModelAsync(modelId);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("VLM install failed: " + ex);
        }
    }

    private async void OnAnalyzeAllClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            await EngineClient.Instance.DeepAnalyzeAllAsync(_activeModel, SkipExistingToggle.IsOn);
            StreamCard.Visibility = Visibility.Visible;
            CancelButton.IsEnabled = true;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("DeepAnalyzeAll failed: " + ex);
        }
    }

    private async void OnCancelClicked(object sender, RoutedEventArgs e)
    {
        try { await EngineClient.Instance.DeepAnalyzeCancelAsync(); }
        catch (Exception ex) { DebugLog.Warn("Cancel failed: " + ex); }
    }
}
