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
    private string _activeModel = "qwen2_5_vl_7b";
    private string _captionAccumulator = string.Empty;
    private bool _unloaded;

    public DeepAnalyzeView()
    {
        InitializeComponent();
        // Restore the user's last VLM choice so the auto-chain after
        // face clustering and a manual Analyze All both use the same
        // weights the user last picked. Falls back to qwen2_5_vl_7b.
        try { _activeModel = AppSettings.Load().SelectedVlmModelKind; }
        catch { /* keep default */ }
        Loaded += OnLoadedHandler;
        Unloaded += OnUnloadedHandler;
    }

    private void OnUnloadedHandler(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        ModelInstallerService.Instance.DeepVlm.PropertyChanged -= OnInstallerChanged;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        SelectionRegistry.Instance.PropertyChanged -= OnSelectionRegistryChanged;
        Loaded -= OnLoadedHandler;
        Unloaded -= OnUnloadedHandler;
    }

    // Resident-RAM budget per VLM, in GB. Mirrors the macOS AIModelKind
    // .ramBudgetGB (platforms/apple .../AIModels.swift) so the OOM gate is
    // identical across platforms. A model whose budget can't fit under the
    // headroom is disabled — loading it would OOM-kill the engine.
    private static double RamBudgetGB(string kind) => kind switch
    {
        "mistral_small_3_2" => 16.0,
        "qwen2_5_vl_7b" => 7.0,
        "gemma_3_4b" => 4.5,
        _ => 7.0,
    };

    // Reserves ~8 GB for the OS + scan engine + DB cache, exactly like macOS
    // AIModelKind.fits(ramGB:). Returns the machine's physical RAM in GB from
    // EngineClient.Info (PhysicalMemoryGB, with Hardware.ramTotalMB as the
    // fallback), or null when the engine hasn't reported yet.
    private static double? PhysicalRamGB()
    {
        var info = EngineClient.Instance.Info;
        if (info is null) return null;
        if (info.PhysicalMemoryGB > 0) return info.PhysicalMemoryGB;
        if (info.Hardware is { RamTotalMb: > 0 } hw) return hw.RamTotalMb / 1024.0;
        return null;
    }

    private static bool Fits(string kind, double ramGB)
    {
        var headroom = Math.Max(0, ramGB - 8.0);
        return RamBudgetGB(kind) <= headroom;
    }

    private void OnLoadedHandler(object sender, RoutedEventArgs e)
    {
        ModelInstallerService.Instance.DeepVlm.PropertyChanged += OnInstallerChanged;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        SelectionRegistry.Instance.PropertyChanged += OnSelectionRegistryChanged;
        SyncCards();
        UpdateActiveModelLabel();
        SyncSelectionButtons();
        // refresh the "Name people first" gate every time the
        // view loads; also refreshed in OnEngineChanged when face
        // clustering finishes.
        _ = RefreshNamePeopleGateAsync();
    }

    private void OnSelectionRegistryChanged(object? sender, PropertyChangedEventArgs e)
        => DispatcherQueue.TryEnqueue(SyncSelectionButtons);

    private void SyncSelectionButtons()
    {
        if (_unloaded) return;
        var sel = SelectionRegistry.Instance.LibrarySelection;
        AnalyzeSelectedButton.IsEnabled = sel.Count > 0;
        AnalyzeSelectedText.Text = sel.Count switch
        {
            0 => "Selected",
            1 => "Selected (1)",
            _ => $"Selected ({sel.Count})",
        };
        AnalyzeCurrentButton.IsEnabled = SelectionRegistry.Instance.HasPreviewedFile;
    }

    /// <summary>query the DB for any person row with NULL
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
        if (_unloaded) return;
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_unloaded) return;
            if (unnamed > 0)
            {
                NamePeopleGateBanner.Visibility = Visibility.Visible;
                NamePeopleGateText.Text = unnamed == 1
                    ? "1 face cluster isn't named yet. Naming it first gives sharper captions — or analyze now and name later."
                    : $"{unnamed} face clusters aren't named yet. Naming them first gives sharper captions — or analyze now and name later.";
                // Advisory, NOT blocking — mirrors the macOS two-path banner: the
                // user can name people via the banner button OR run Deep Analyze
                // now. (Previously this hard-disabled Analyze All, which stranded
                // anyone who didn't want to name clusters first.)
                AnalyzeAllButton.IsEnabled = true;
                ToolTipService.SetToolTip(AnalyzeAllButton, null);
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
    {
        if (_unloaded) return;
        DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncCards(); });
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("DeepAnalyzeView.OnEngineChanged", () =>
        {
            if (_unloaded) return;
            switch (e.PropertyName)
            {
                case nameof(EngineClient.DeepAnalyzeStarting):
                case nameof(EngineClient.DeepAnalyzeProgress):
                case nameof(EngineClient.DeepAnalyzeLast):
                case nameof(EngineClient.DeepAnalyzeComplete):
                    DebugLog.Debug($"[ENGINE-SUB:DeepAnalyzeView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncStream(); });
                    break;
                case nameof(EngineClient.Phase):
                case nameof(EngineClient.LastFaceClustering):
                    DebugLog.Debug($"[ENGINE-SUB:DeepAnalyzeView] {e.PropertyName}");
                    _ = RefreshNamePeopleGateAsync();
                    break;
                case nameof(EngineClient.Info):
                    // The engine just reported physical RAM — re-gate the model
                    // cards so any VLM that would OOM-kill the engine is disabled.
                    DebugLog.Debug($"[ENGINE-SUB:DeepAnalyzeView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncCards(); });
                    break;
            }
        });

    private void SyncCards()
    {
        var slot = ModelInstallerService.Instance.DeepVlm;
        // Each card reflects whether ITS model's weights are actually on disk —
        // not the shared "any VLM installed" slot, otherwise installing one model
        // makes the other cards mis-report as installed and Deep Analyze fails
        // every file with "VLM weights not installed".
        var ramGB = PhysicalRamGB();
        ApplyVlmCard(MistralCard, MistralStatus, MistralProgress, MistralInstallButton, "mistral_small_3_2", slot, ramGB);
        ApplyVlmCard(QwenLargeCard, QwenLargeStatus, QwenLargeProgress, QwenLargeInstallButton, "qwen2_5_vl_7b", slot, ramGB);
        ApplyVlmCard(GemmaCard, GemmaStatus, GemmaProgress, GemmaInstallButton, "gemma_3_4b", slot, ramGB);
        HighlightActiveCard();
    }

    /// <summary>True when both gguf halves for this model_kind are on disk under
    /// %LOCALAPPDATA%\FileID\Models\vlm\&lt;kind&gt;\. Mirrors the engine's
    /// vlm::find_weights so a card's "Installed" badge matches what Deep Analyze
    /// can actually run.</summary>
    private static bool VlmWeightsPresent(string kind)
    {
        try
        {
            var dir = System.IO.Path.Combine(AppPaths.ModelsDir, "vlm", kind);
            return System.IO.File.Exists(System.IO.Path.Combine(dir, "model.gguf"))
                && System.IO.File.Exists(System.IO.Path.Combine(dir, "mmproj.gguf"));
        }
        catch { return false; }
    }

    private static void ApplyVlmCard(Border card, TextBlock status, ProgressBar bar, Button installButton, string kind, ModelSlot slot, double? ramGB)
    {
        // RAM gate — mirrors macOS ModelOptionRow. When the engine has reported
        // physical RAM and this VLM's budget can't fit under the ~8 GB headroom,
        // disable install/select and show a "Needs N GB (you have M)" affordance
        // instead of letting the model OOM-kill the engine on load.
        if (ramGB is double available && !Fits(kind, available))
        {
            status.Text = $"Needs {RamBudgetGB(kind):0} GB (you have {available:0})";
            status.Foreground = ThemeHelper.GetBrushSafe("DestructiveTextBrush");
            bar.Visibility = Visibility.Collapsed;
            installButton.IsEnabled = false;
            ToolTipService.SetToolTip(card,
                $"This model needs {RamBudgetGB(kind):0} GB resident RAM. With your {available:0} GB machine and the scan engine running, loading it would OOM-kill the engine. Pick a smaller model.");
            card.Opacity = 0.55;
            card.IsHitTestVisible = false;
            return;
        }
        status.Foreground = ThemeHelper.GetBrushSafe("AiBrush");
        ToolTipService.SetToolTip(card, null);
        card.Opacity = 1.0;
        card.IsHitTestVisible = true;

        // The shared Vlm slot tracks at most one in-flight download; attribute its
        // Downloading/Failed state to a card only when CurrentModelKind matches.
        bool isThisModel = string.Equals(slot.CurrentModelKind, kind, StringComparison.OrdinalIgnoreCase);
        if (slot.Status == ModelInstallStatus.Downloading && isThisModel)
        {
            status.Text = $"Downloading… {Math.Round(slot.Fraction * 100)}%";
            bar.Visibility = Visibility.Visible;
            bar.Value = slot.Fraction;
            installButton.IsEnabled = false;
        }
        else if (VlmWeightsPresent(kind))
        {
            status.Text = "Installed";
            bar.Visibility = Visibility.Collapsed;
            installButton.Content = "Reinstall";
            installButton.IsEnabled = true;
        }
        else if (slot.Status == ModelInstallStatus.Failed && isThisModel)
        {
            status.Text = "Install failed — retry?";
            bar.Visibility = Visibility.Collapsed;
            installButton.Content = "Install";
            installButton.IsEnabled = true;
        }
        else
        {
            status.Text = string.Empty;
            bar.Visibility = Visibility.Collapsed;
            installButton.Content = "Install";
            installButton.IsEnabled = true;
        }
    }

    private void HighlightActiveCard()
    {
        var idle = ThemeHelper.GetBrushSafe("CardStrokeColorDefaultBrush");
        var gold = ThemeHelper.GetBrushSafe("GoldBrush");
        MistralCard.BorderBrush = _activeModel == "mistral_small_3_2" ? gold : idle;
        QwenLargeCard.BorderBrush = _activeModel == "qwen2_5_vl_7b" ? gold : idle;
        GemmaCard.BorderBrush = _activeModel == "gemma_3_4b" ? gold : idle;
        MistralCard.BorderThickness = _activeModel == "mistral_small_3_2" ? new Thickness(2) : new Thickness(1);
        QwenLargeCard.BorderThickness = _activeModel == "qwen2_5_vl_7b" ? new Thickness(2) : new Thickness(1);
        GemmaCard.BorderThickness = _activeModel == "gemma_3_4b" ? new Thickness(2) : new Thickness(1);
    }

    private void UpdateActiveModelLabel()
    {
        ActiveModelText.Text = _activeModel switch
        {
            "qwen2_5_vl_7b" => "Active model: Qwen 2.5-VL 7B (best quality)",
            "gemma_3_4b" => "Active model: Gemma 3 4B (balanced)",
            "mistral_small_3_2" => "Active model: Mistral-Small 3.2 (max quality)",
            _ => "Active model: Qwen 2.5-VL 7B (best quality)",
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

        // starting-card pre-progress. Engine emits
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
            // include ETA + processed/total + per-second rate
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
            // live caption stream. Engine emits the partial
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

    /// <summary>smart-names pending-rename pill. Shows the
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

    /// <summary>open BulkRenameSheet pre-seeded with every
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
        // BitmapImage is a UI-thread DispatcherObject; constructing it off the UI
        // thread (this await can resume on a worker) is a native fast-fail. Capture
        // the dispatcher before any await and do the construct + StreamImage.Source
        // set inside one TryEnqueue; null the source on failure (placeholder, not stale).
        if (_unloaded) return;
        // In-proc shell video/audio thumbnail providers can native-fast-fail the
        // whole app (no managed exception). This path calls GetThumbnailAsync
        // directly, bypassing ThumbnailService, so it must apply the same skip —
        // single source of truth in ThumbnailService.SkipShellThumbnailForExtension.
        if (Services.ThumbnailService.SkipShellThumbnailForExtension(path)) return;
        var dispatcher = DispatcherQueue;
        Windows.Storage.FileProperties.StorageItemThumbnail? thumb = null;
        try
        {
            var file = await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
            if (_unloaded) return;
            thumb = await file.GetThumbnailAsync(
                Windows.Storage.FileProperties.ThumbnailMode.SingleItem, 320,
                Windows.Storage.FileProperties.ThumbnailOptions.UseCurrentScale);
            if (_unloaded) { try { thumb?.Dispose(); } catch { } return; }
            if (thumb != null && thumb.Size > 0 && dispatcher != null)
            {
                var captured = thumb;
                thumb = null;
                var enqueued = dispatcher.TryEnqueue(async () =>
                {
                    try
                    {
                        var bmp = new BitmapImage();
                        await bmp.SetSourceAsync(captured);
                        StreamImage.Source = bmp;
                    }
                    catch (Exception ex)
                    {
                        DebugLog.Warn($"LoadStreamThumbAsync UI render: {ex.Message}");
                        try { StreamImage.Source = null; } catch { }
                    }
                    finally
                    {
                        try { captured.Dispose(); } catch { }
                    }
                });
                if (!enqueued)
                {
                    DebugLog.Warn("LoadStreamThumbAsync: dispatcher.TryEnqueue returned false.");
                    try { captured.Dispose(); } catch { }
                }
                return;
            }
            ClearStreamImageOnDispatcher(dispatcher);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"LoadStreamThumbAsync({PathRedactor.Redact(path)}) failed: {ex.Message}");
            ClearStreamImageOnDispatcher(dispatcher);
        }
        finally
        {
            try { thumb?.Dispose(); } catch { }
        }
    }

    private void ClearStreamImageOnDispatcher(Microsoft.UI.Dispatching.DispatcherQueue? dispatcher)
    {
        if (dispatcher is null || _unloaded) return;
        dispatcher.TryEnqueue(() => { if (!_unloaded) try { StreamImage.Source = null; } catch { } });
    }

    private void OnModelCardTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        if (sender is FrameworkElement el && el.Tag is string id)
        {
            // Don't let the user select a model that would OOM-kill the engine —
            // mirrors the macOS `guard fits else { return }`. The card is also
            // IsHitTestVisible=false in that state, but guard here defensively.
            if (PhysicalRamGB() is double ramGB && !Fits(id, ramGB)) return;
            _activeModel = id;
            HighlightActiveCard();
            UpdateActiveModelLabel();
            // Persist so the next launch (and the post-clustering auto-
            // chain) caption with the same model the user just picked.
            try
            {
                var s = AppSettings.Load();
                s.SelectedVlmModelKind = id;
                s.Save();
            }
            catch (Exception ex) { DebugLog.Warn("Persist VLM choice failed: " + ex.Message); }
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
            // previous version ignored the Tag and ALWAYS installed
            // qwen2_5_vl_3b. Now uses the per-card model id from Tag so each
            // model card actually installs its own model.
            if (sender is not Button b || b.Tag is not string modelId || string.IsNullOrWhiteSpace(modelId)) return;
            // Tell the picker which model is downloading so SyncCards animates
            // THIS card. The engine's progress events carry only model_kind, and
            // this direct-prewarm path doesn't go through ModelInstallerService
            // (which is where CurrentModelKind would otherwise be set).
            ModelInstallerService.Instance.DeepVlm.CurrentModelKind = modelId;
            SyncCards();
            await EngineClient.Instance.PrewarmModelAsync(modelId);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("VLM install failed: " + ex);
        }
    }

    private async void OnAnalyzeAllClicked(object sender, RoutedEventArgs e)
    {
        // Set the optimistic/working UI state BEFORE the await: if the send
        // throws, the catch reverts it and surfaces the error. Setting it after
        // the await meant a send failure showed the user nothing — the run
        // silently never started while the UI looked idle/ready.
        StreamCard.Visibility = Visibility.Visible;
        CancelButton.IsEnabled = true;
        AnalyzeAllButton.IsEnabled = false;
        try
        {
            // Manual pass = full enrichment (caption + smart-rename + tags), so
            // tagsOnly stays false. The background auto-pass uses tagsOnly:true.
            await EngineClient.Instance.DeepAnalyzeAllAsync(_activeModel, SkipExistingToggle.IsOn, tagsOnly: false, proposeRenames: ProposeRenamesCheck.IsChecked == true);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("DeepAnalyzeAll failed: " + ex);
            // Revert the optimistic state so the UI doesn't falsely look like a
            // run is in flight, then surface a dismissible error.
            StreamCard.Visibility = Visibility.Collapsed;
            CancelButton.IsEnabled = false;
            AnalyzeAllButton.IsEnabled = true;
            await ShowAlertAsync("Couldn't start Deep Analyze",
                "Deep Analyze couldn't be started: " + ex.Message +
                "\n\nMake sure the model is installed and the engine is running, then try again.");
        }
    }

    // Analyzes every file currently selected in the Library view. We send
    // one DeepAnalyzeFile per file. Engine throttles parallelism via its
    // model pool; sending N requests just queues them up.
    private async void OnAnalyzeSelectedClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnAnalyzeSelectedClicked), async () =>
        {
            var sel = SelectionRegistry.Instance.LibrarySelection;
            if (sel.Count == 0) return;
            StreamCard.Visibility = Visibility.Visible;
            CancelButton.IsEnabled = true;
            foreach (var id in sel)
            {
                try { await EngineClient.Instance.DeepAnalyzeFileAsync(id, _activeModel); }
                catch (Exception ex) { DebugLog.Warn($"DeepAnalyzeFile({id}) failed: {ex.Message}"); }
            }
        });

    // Analyzes the file currently open in FilePreviewSheet. The preview
    // sheet publishes its file id to SelectionRegistry on open + clears
    // it on close, so this button is only enabled while a sheet is up.
    private async void OnAnalyzeCurrentClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnAnalyzeCurrentClicked), async () =>
        {
            var id = SelectionRegistry.Instance.PreviewedFileId;
            if (id is null) return;
            StreamCard.Visibility = Visibility.Visible;
            CancelButton.IsEnabled = true;
            try { await EngineClient.Instance.DeepAnalyzeFileAsync(id.Value, _activeModel); }
            catch (Exception ex) { DebugLog.Warn($"DeepAnalyzeFile (current) failed: {ex.Message}"); }
        });

    private async void OnCancelClicked(object sender, RoutedEventArgs e)
    {
        try { await EngineClient.Instance.DeepAnalyzeCancelAsync(); }
        catch (Exception ex) { DebugLog.Warn("Cancel failed: " + ex); }
    }

    private async System.Threading.Tasks.Task ShowAlertAsync(string title, string body)
    {
        // ContentDialog.ShowAsync can throw on a broken XamlRoot (mid-shutdown,
        // tab re-host). Catch + log so a failed alert never escalates to
        // App.UnhandledException. Mirrors SidebarProcessingControl.ShowAlertAsync.
        try
        {
            if (XamlRoot is null)
            {
                DebugLog.Warn($"ShowAlertAsync: XamlRoot is null ({title}); skipping dialog.");
                return;
            }
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = title,
                Content = body,
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
            };
            await dialog.ShowAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"ShowAlertAsync({title}) threw: " + ex.Message);
        }
    }
}
