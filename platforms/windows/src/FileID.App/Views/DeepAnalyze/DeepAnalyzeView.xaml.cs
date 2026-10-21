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
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.Views.DeepAnalyze;

public sealed partial class DeepAnalyzeView : UserControl
{
    private string _activeModel = "qwen2_5_vl_7b";
    private string _captionAccumulator = string.Empty;
    private bool _unloaded;

    // Trips when Cancel is clicked during an "Analyze Selected" batch so the
    // per-file send loop stops dispatching the remaining files. The engine
    // cancel only stops the in-flight file; without this the loop would keep
    // queuing files 2..N after the user pressed Cancel.
    private bool _selectedRunCancelled;

    // Monotonic generation for the streamed-thumbnail load. Each progress event
    // fires LoadStreamThumbAsync fire-and-forget; a slow decode for an earlier
    // file can resolve after a later one's. We bump this at the start of every
    // load and only commit StreamImage.Source if our captured generation is
    // still the latest, so a stale thumbnail never overwrites the current file's.
    private int _streamThumbGeneration;

    // Warm-up watchdog: the engine emits DeepAnalyzeStarting (IsIndeterminate
    // "Preparing…") BEFORE the first DeepAnalyzeProgress/stream token while the
    // VLM loads (~5-30 s first run). If the load stalls there's no failure
    // event, so the spinner would otherwise spin forever. This timer fires if
    // no progress/stream token arrives in time; it's cancelled the moment any
    // progress/last/complete lands or the view unloads. Runs on the UI thread
    // (DispatcherQueueTimer.Tick), so touching XAML in the handler is safe.
    private static readonly TimeSpan WarmupTimeout = TimeSpan.FromSeconds(45);
    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _warmupTimer;

    public DeepAnalyzeView()
    {
        InitializeComponent();
        // Restore the user's last VLM choice so the auto-chain after
        // face clustering and a manual Analyze All both use the same
        // weights the user last picked. Falls back to qwen2_5_vl_7b.
        try { _activeModel = AppViewModel.Instance.Settings.SelectedVlmModelKind; }
        catch { /* keep default */ }
        Loaded += OnLoadedHandler;
        Unloaded += OnUnloadedHandler;
    }

    private void OnUnloadedHandler(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        CancelWarmupTimer();
        ModelInstallerService.Instance.DeepVlm.PropertyChanged -= OnInstallerChanged;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        SelectionRegistry.Instance.PropertyChanged -= OnSelectionRegistryChanged;
        Loaded -= OnLoadedHandler;
        Unloaded -= OnUnloadedHandler;
    }

    // Resident-RAM budgets for previously supported VLMs reserve 8 GB
    // for the OS, scan engine, and DB cache.
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
        // Qwen3 working-set peaks are not measured. Do not reject an
        // explicitly selected model using an invented RAM threshold.
        if (kind is "qwen3_vl_4b" or "qwen3_vl_8b") return true;
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
        SyncExplainerBanner();
        // Restore the pending-rename pill from the DB on every load so navigating
        // away and back doesn't wipe the count. _proposedNameCount is incremented
        // per-FileDone during a run; this seeds it from the authoritative DB count
        // so the pill is correct even after a tab switch between runs.
        _ = RestorePendingNamesCountAsync();
    }

    private async System.Threading.Tasks.Task RestorePendingNamesCountAsync()
    {
        try
        {
            using var store = new Services.ReadStore(Services.AppPaths.DbPath);
            await store.OpenAsync().ConfigureAwait(false);
            var ct = System.Threading.CancellationToken.None;
            var count = await store.PendingProposedRenameCountAsync(ct).ConfigureAwait(false);
            if (_unloaded) return;
            DispatcherQueue.TryEnqueue(() =>
            {
                if (_unloaded) return;
                // Only override the in-memory counter when a run is NOT in
                // progress (Starting/Progress latched) — a live run's counter is
                // correct; the DB version may lag by one file.
                var ec = EngineClient.Instance;
                if (ec.DeepAnalyzeStarting is not null || ec.DeepAnalyzeProgress is not null) return;
                _proposedNameCount = count;
                SyncProposedNamesPill();
            });
        }
        catch (Exception ex) { DebugLog.Warn("RestorePendingNamesCountAsync failed: " + ex.Message); }
    }

    // Item 4: show the Tagging-vs-Deep-Analyze explainer unless the user has
    // dismissed it (persisted in AppSettings, lockstep with macOS).
    private void SyncExplainerBanner()
    {
        bool hidden = false;
        try { hidden = AppViewModel.Instance.Settings.HideDeepAnalyzeExplainer; }
        catch (Exception ex) { DebugLog.Warn("SyncExplainerBanner read failed: " + ex.Message); }
        ExplainerBanner.Visibility = hidden ? Visibility.Collapsed : Visibility.Visible;
    }

    private void OnDismissExplainerClicked(object sender, RoutedEventArgs e)
    {
        ExplainerBanner.Visibility = Visibility.Collapsed;
        try
        {
            var s = AppViewModel.Instance.Settings;
            s.HideDeepAnalyzeExplainer = true;
            s.Save();
        }
        catch (Exception ex) { DebugLog.Warn("Persist explainer dismiss failed: " + ex.Message); }
    }

    private void OnSelectionRegistryChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnSelectionRegistryChanged), () => OnSelectionRegistryChangedCore(sender, e));

    private void OnSelectionRegistryChangedCore(object? sender, PropertyChangedEventArgs e)
        => DispatcherQueue.TryEnqueue(SyncSelectionButtons);

    private void SyncSelectionButtons()
    {
        if (_unloaded) return;
        var sel = SelectionRegistry.Instance.LibrarySelection;
        AnalyzeSelectedText.Text = sel.Count == 0 ? "Chosen files" : $"Chosen files ({sel.Count:N0})";
        SyncDeepAnalyzeControls();
    }

    private static bool _namePeopleTipDismissed;
    private int _eligibleTotal = -1;
    private int _pendingTotal = -1;
    private bool _hasPeople;
    private bool _hasNamedAnyone;
    private string _countsModel = string.Empty;

    private sealed record LibraryCounts(int Total, int Pending, bool HasPeople, bool HasNamed);

    private async System.Threading.Tasks.Task RefreshNamePeopleGateAsync()
    {
        var model = _activeModel;
        LibraryCounts? counts = null;
        try
        {
            var dbPath = AppPaths.DbPath;
            counts = await System.Threading.Tasks.Task.Run(() => ReadLibraryCounts(dbPath, model)).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("RefreshNamePeopleGateAsync failed: " + ex.Message);
        }
        if (_unloaded) return;
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_unloaded) return;
            if (counts is not null)
            {
                _eligibleTotal = counts.Total;
                _pendingTotal = counts.Pending;
                _hasPeople = counts.HasPeople;
                _hasNamedAnyone = counts.HasNamed;
                _countsModel = model;
            }
            SyncLibraryStatus();
            SyncDeepAnalyzeControls();
        });
    }

    private static LibraryCounts? ReadLibraryCounts(string dbPath, string model)
    {
        try
        {
            if (!System.IO.File.Exists(dbPath)) return new LibraryCounts(0, 0, false, false);
            using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                {
                    DataSource = dbPath,
                    Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                }.ToString());
            conn.Open();
            int total = 0, pending = 0;
            using (var cmd = conn.CreateCommand())
            {
                cmd.CommandText =
                    "SELECT COUNT(*), COALESCE(SUM(CASE WHEN vlm_full_model IS $model THEN 0 ELSE 1 END), 0) "
                    + "FROM files WHERE kind IN ('image','video','pdf') AND failed = 0;";
                cmd.Parameters.AddWithValue("$model", model);
                using var reader = cmd.ExecuteReader();
                if (reader.Read())
                {
                    total = reader.IsDBNull(0) ? 0 : Convert.ToInt32(reader.GetValue(0));
                    pending = reader.IsDBNull(1) ? 0 : Convert.ToInt32(reader.GetValue(1));
                }
            }
            int people = 0, named = 0;
            try
            {
                using var cmd = conn.CreateCommand();
                cmd.CommandText =
                    "SELECT COUNT(*), COALESCE(SUM(CASE WHEN name IS NOT NULL OR first_name IS NOT NULL THEN 1 ELSE 0 END), 0) "
                    + "FROM persons WHERE IFNULL(is_unknown, 0) = 0;";
                using var reader = cmd.ExecuteReader();
                if (reader.Read())
                {
                    people = reader.IsDBNull(0) ? 0 : Convert.ToInt32(reader.GetValue(0));
                    named = reader.IsDBNull(1) ? 0 : Convert.ToInt32(reader.GetValue(1));
                }
            }
            catch (Exception ex) { DebugLog.Warn("ReadLibraryCounts persons query failed: " + ex.Message); }
            return new LibraryCounts(total, pending, people > 0, named > 0);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("ReadLibraryCounts failed: " + ex.Message);
            return null;
        }
    }

    private void SyncLibraryStatus()
    {
        if (_unloaded) return;
        var known = _eligibleTotal >= 0;
        LibraryEmptyText.Visibility = known && _eligibleTotal == 0 ? Visibility.Visible : Visibility.Collapsed;
        EligibleCountText.Text = known ? $"{_eligibleTotal:N0} files" : "—";
        PendingCountText.Text = known ? $"{_pendingTotal:N0} files" : "—";
        var ec = EngineClient.Instance;
        var running = ec.DeepAnalyzeStarting is not null || ec.DeepAnalyzeProgress is not null;
        NamePeopleGateBanner.Visibility = _hasPeople && !_hasNamedAnyone && !running && !_namePeopleTipDismissed
            ? Visibility.Visible
            : Visibility.Collapsed;
        var canTagPeople = _hasNamedAnyone || !known;
        ApplyPeopleButton.IsEnabled = _applyInFlight == 0 && canTagPeople;
        ToolTipService.SetToolTip(ApplyPeopleButton, canTagPeople
            ? "Tag each photo with the names of the people in it"
            : "Name someone on the People page first");
    }

    private void OnDismissNamePeopleTipClicked(object sender, RoutedEventArgs e)
    {
        _namePeopleTipDismissed = true;
        NamePeopleGateBanner.Visibility = Visibility.Collapsed;
    }

    private void OnSkipExistingClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnSkipExistingClicked), SyncDeepAnalyzeControls);

    private void OnGoToPeopleClicked(object sender, RoutedEventArgs e)
    {
        AppViewModel.Instance.ActiveTab = SidebarTab.People;
    }

    private void OnInstallerChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_unloaded) return;
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_unloaded) return;
            SyncCards();
            SyncDeepAnalyzeControls();
        });
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
                case nameof(EngineClient.DeepAnalyzeCommandInFlight):
                    DebugLog.Debug($"[ENGINE-SUB:DeepAnalyzeView] {e.PropertyName}");
                    var ec = EngineClient.Instance;
                    var attemptId = ec.DeepAnalyzeCommandAttemptId;
                    DispatcherQueue.TryEnqueue(() =>
                    {
                        if (_unloaded || ec.DeepAnalyzeCommandAttemptId != attemptId) return;
                        SyncDeepAnalyzeControls();
                    });
                    break;
                case nameof(EngineClient.GpuDeviceRemoved):
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncDeepAnalyzeControls(); });
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
        ApplyVlmCard(Qwen3FourCard, Qwen3FourStatus, Qwen3FourProgress, Qwen3FourInstallButton, "qwen3_vl_4b", slot, ramGB);
        ApplyVlmCard(Qwen3EightCard, Qwen3EightStatus, Qwen3EightProgress, Qwen3EightInstallButton, "qwen3_vl_8b", slot, ramGB);
        HighlightActiveCard();
    }

    /// <summary>Require both GGUF halves in the engine registry's on-disk
    /// directory before reporting a specific VLM installed.</summary>
    private static bool VlmWeightsPresent(string kind)
    {
        try { return VlmWeightDirs.WeightsPresent(AppPaths.ModelsDir, kind); }
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
            status.Text = $"Needs {RamBudgetGB(kind):0} GB memory (this PC has {available:0})";
            status.Foreground = ThemeHelper.GetBrushSafe("DestructiveTextBrush");
            bar.Visibility = Visibility.Collapsed;
            installButton.IsEnabled = false;
            ToolTipService.SetToolTip(card,
                "Too big for this PC's memory. Choose a smaller model.");
            card.Opacity = 0.55;
            card.IsHitTestVisible = false;
            return;
        }
        status.Foreground = ThemeHelper.GetBrushSafe("AiBrush");
        ToolTipService.SetToolTip(card, null);
        card.Opacity = 1.0;
        card.IsHitTestVisible = true;

        // All cards share one download slot. Keep other models selectable,
        // but block their Install/Reinstall actions until this transfer ends.
        bool isThisModel = string.Equals(slot.CurrentModelKind, kind, StringComparison.OrdinalIgnoreCase);
        if (slot.Status == ModelInstallStatus.Downloading)
        {
            if (isThisModel)
            {
                status.Text = $"Downloading… {Math.Round(slot.Fraction * 100)}%";
                bar.Visibility = Visibility.Visible;
                bar.Value = slot.Fraction;
            }
            else
            {
                var installed = VlmWeightsPresent(kind);
                status.Text = installed ? "Ready" : "Another model is downloading…";
                bar.Visibility = Visibility.Collapsed;
                installButton.Content = installed ? "Download again" : "Download";
            }
            installButton.IsEnabled = false;
            return;
        }
        if (VlmWeightsPresent(kind))
        {
            status.Text = "Ready";
            bar.Visibility = Visibility.Collapsed;
            installButton.Content = "Download again";
            installButton.IsEnabled = true;
        }
        else if (slot.Status == ModelInstallStatus.Failed && isThisModel)
        {
            status.Text = "Download failed. Try again.";
            bar.Visibility = Visibility.Collapsed;
            installButton.Content = "Download";
            installButton.IsEnabled = true;
        }
        else
        {
            status.Text = string.Empty;
            bar.Visibility = Visibility.Collapsed;
            installButton.Content = "Download";
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
        Qwen3FourCard.BorderBrush = _activeModel == "qwen3_vl_4b" ? gold : idle;
        Qwen3EightCard.BorderBrush = _activeModel == "qwen3_vl_8b" ? gold : idle;
        MistralCard.BorderThickness = _activeModel == "mistral_small_3_2" ? new Thickness(2) : new Thickness(1);
        QwenLargeCard.BorderThickness = _activeModel == "qwen2_5_vl_7b" ? new Thickness(2) : new Thickness(1);
        GemmaCard.BorderThickness = _activeModel == "gemma_3_4b" ? new Thickness(2) : new Thickness(1);
        Qwen3FourCard.BorderThickness = _activeModel == "qwen3_vl_4b" ? new Thickness(2) : new Thickness(1);
        Qwen3EightCard.BorderThickness = _activeModel == "qwen3_vl_8b" ? new Thickness(2) : new Thickness(1);
    }

    private void UpdateActiveModelLabel()
    {
        ActiveModelText.Text = ModelDisplayName(AppSettings.IsAllowedVlmKind(_activeModel) ? _activeModel : "qwen2_5_vl_7b");
    }

    private int _proposedNameCount;
    // DeepAnalyzeLast is a latched EngineClient property: the engine nulls it
    // only on DeepAnalyzeStarting and overwrites it on the next (throttled)
    // FileDone — it is NOT cleared on a progress event. Remember the instance
    // already consumed so the last-result effects run exactly once per file and
    // a later file's progress tick can't re-process the previous file's result.
    private FileID.IpcSchema.DeepAnalyzeFileDone? _lastConsumedFileDone;

    private void SyncStream()
    {
        var ec = EngineClient.Instance;
        var starting = ec.DeepAnalyzeStarting;
        var prog = ec.DeepAnalyzeProgress;
        var last = ec.DeepAnalyzeLast;
        var complete = ec.DeepAnalyzeComplete;

        if (starting is null && prog is null && last is null && complete is null) return;

        // Any real progress/result/terminal event proves the model loaded and
        // tokens are flowing — disarm the warm-up watchdog so it can't false-fire.
        if (prog is not null || last is not null || complete is not null)
        {
            CancelWarmupTimer();
        }

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
            StreamTitleText.Text = "Getting the AI ready…";
            StreamFileNameText.Text = ModelDisplayName(string.IsNullOrEmpty(starting.ModelKind) ? _activeModel : starting.ModelKind);
            StreamCaptionText.Text = string.IsNullOrEmpty(starting.Message)
                ? "Loading the model on this PC. The first description usually appears within 30 seconds."
                : starting.Message;
            StreamInheritedText.Visibility = Visibility.Collapsed;
            StreamProposedNameText.Text = string.Empty;
            // Reset the smart-rename tally at the start of each run so the pill
            // reflects only THIS run, not a cumulative count across runs.
            _proposedNameCount = 0;
            _lastConsumedFileDone = null;
            SyncProposedNamesPill();
            OverallProgress.Value = 0;
            OverallProgress.IsIndeterminate = true;
            OverallProgressText.Text = "Getting ready…";
            // Arm the warm-up watchdog so a stalled model load surfaces a
            // dismissible error + reverts the optimistic UI instead of
            // spinning "Preparing…" indefinitely.
            ArmWarmupTimer();
        }

        if (prog is not null)
        {
            StreamCard.Visibility = Visibility.Visible;
            CancelButton.IsEnabled = true;
            AnalyzeAllButton.IsEnabled = false;
            StreamTitleText.Text = "Describing files…";
            OverallProgress.IsIndeterminate = false;

            var pct = prog.Total == 0 ? 0 : (double)prog.Processed / prog.Total;
            OverallProgress.Value = pct;
            // include ETA + processed/total + per-second rate
            // when the engine reports it.
            var etaSuffix = prog.EtaSeconds is double eta && eta > 0
                ? $" · about {FormatEta(eta)} left"
                : string.Empty;
            OverallProgressText.Text = $"{prog.Processed:N0} of {prog.Total:N0} files{etaSuffix}";

            if (!string.IsNullOrEmpty(prog.CurrentPath))
            {
                StreamFileNameText.Text = Path.GetFileName(prog.CurrentPath);
                _ = LoadStreamThumbAsync(prog.CurrentPath);
                _captionAccumulator = string.Empty;
                StreamCaptionText.Text = string.Empty;
                StreamInheritedText.Visibility = Visibility.Collapsed;
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

        // Only act on a genuinely new FileDone. DeepAnalyzeLast stays latched
        // across every subsequent progress tick for the *next* file; without
        // this reference guard those ticks would re-increment _proposedNameCount
        // (inflated pill) and clobber the current file's live caption / proposed
        // name with the previous file's finished text.
        if (last is not null && !ReferenceEquals(last, _lastConsumedFileDone))
        {
            _lastConsumedFileDone = last;
            StreamCaptionText.Text = last.Description ?? string.Empty;
            StreamInheritedText.Visibility = last.InheritedFromFileId is null ? Visibility.Collapsed : Visibility.Visible;
            if (!string.IsNullOrEmpty(last.ProposedName))
            {
                StreamProposedNameText.Text = $"Suggested name: {last.ProposedName}";
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
            StreamTitleText.Text = complete.Cancelled ? "Stopped" : "Finished";
            OverallProgressText.Text = $"{complete.Processed:N0} file{(complete.Processed == 1 ? "" : "s")} described"
                + (complete.Failed > 0 ? $" · {complete.Failed:N0} couldn't be read" : string.Empty)
                + $" · took {FormatEta(complete.TotalSeconds)}";
            SyncProposedNamesPill();
            _ = RefreshNamePeopleGateAsync();
        }

        SyncDeepAnalyzeControls();
    }

    private static Style? _accentStyle;
    private Brush? _runMetaDefaultBrush;

    private static bool IsDeepAnalyzeBusy(EngineClient ec)
        => ec.DeepAnalyzeCommandInFlight
           || ec.DeepAnalyzeStarting is not null
           || ec.DeepAnalyzeProgress is not null;

    private static string ModelDisplayName(string kind) => kind switch
    {
        "qwen2_5_vl_7b" => "Qwen 2.5-VL 7B",
        "qwen3_vl_4b" => "Qwen3-VL 4B",
        "qwen3_vl_8b" => "Qwen3-VL 8B",
        "gemma_3_4b" => "Gemma 3 4B",
        "mistral_small_3_2" => "Mistral-Small 3.2",
        _ => kind,
    };

    private string? ModelBlocker()
    {
        var ec = EngineClient.Instance;
        if (ec.GpuDeviceRemoved) return EngineClient.GpuRestartRequiredMessage;
        if (PhysicalRamGB() is double ramGB && !Fits(_activeModel, ramGB))
        {
            return $"{ModelDisplayName(_activeModel)} needs more memory than this PC has. Choose a smaller model above.";
        }
        if (IsDeepAnalyzeBusy(ec)) return "FileID is already describing files.";
        if (!VlmWeightsPresent(_activeModel))
        {
            return $"Download {ModelDisplayName(_activeModel)} above first.";
        }
        return null;
    }

    private string? RunBlocker()
    {
        if (ModelBlocker() is { } blocker) return blocker;
        if (_eligibleTotal == 0) return "Scan a folder first.";
        if (SkipExistingToggle.IsChecked == true && _eligibleTotal > 0 && _pendingTotal == 0 && _countsModel == _activeModel)
        {
            return "Every file is already described. Turn off \"Only files not described yet\" to describe them again.";
        }
        return null;
    }

    private void SyncDeepAnalyzeControls()
    {
        if (_unloaded) return;
        var ec = EngineClient.Instance;
        var busy = IsDeepAnalyzeBusy(ec);
        var modelBlocker = ModelBlocker();
        var blocker = RunBlocker();
        CancelButton.IsEnabled = busy;
        CancelButton.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;

        AnalyzeAllButton.IsEnabled = blocker is null;
        var count = SkipExistingToggle.IsChecked == true ? _pendingTotal : _eligibleTotal;
        AnalyzeAllText.Text = count > 0 && _countsModel == _activeModel
            ? $"Describe {count:N0} file{(count == 1 ? "" : "s")}"
            : "Describe files";
        var tip = blocker ?? "Write a description and a suggested name for each file. You can stop at any time.";
        ToolTipService.SetToolTip(AnalyzeAllButton, tip);
        AutomationProperties.SetHelpText(AnalyzeAllButton, tip);

        var selectionCount = SelectionRegistry.Instance.LibrarySelection.Count;
        AnalyzeSelectedButton.IsEnabled = modelBlocker is null && selectionCount > 0;
        ToolTipService.SetToolTip(AnalyzeSelectedButton,
            modelBlocker ?? (selectionCount == 0 ? "Choose files in Library first" : "Describe only the files chosen in Library"));
        var hasPreview = SelectionRegistry.Instance.PreviewedFileId is not null;
        AnalyzeCurrentButton.IsEnabled = modelBlocker is null && hasPreview;
        ToolTipService.SetToolTip(AnalyzeCurrentButton,
            modelBlocker ?? (hasPreview ? "Describe only the file open in the preview" : "Open a file in Library first"));

        RunMetaText.Text = blocker is not null && !busy
            ? blocker
            : "You can keep using your PC while this runs.";
        _runMetaDefaultBrush ??= RunMetaText.Foreground;
        RunMetaText.Foreground = blocker is not null && !busy && PhysicalRamGB() is double ram && !Fits(_activeModel, ram)
            ? ThemeHelper.GetBrushSafe("WarningBrush")
            : _runMetaDefaultBrush;

        _accentStyle ??= ThemeHelper.GetStyleSafe("AccentButtonStyle");
        if (_proposedNameCount > 0)
        {
            AnalyzeAllButton.ClearValue(FrameworkElement.StyleProperty);
            ReviewNamesButton.Style = _accentStyle;
        }
        else
        {
            AnalyzeAllButton.Style = _accentStyle;
            ReviewNamesButton.ClearValue(FrameworkElement.StyleProperty);
        }
        SyncLibraryStatus();
    }

    // (Re)arm the warm-up watchdog. Always restarts the interval so a fresh
    // DeepAnalyzeStarting (e.g. a re-queued run, or a phase transition still in
    // the pre-progress window) gives the model the full WarmupTimeout to load.
    private void ArmWarmupTimer()
    {
        if (_unloaded) return;
        var dq = DispatcherQueue;
        if (dq is null) return;
        if (_warmupTimer is null)
        {
            _warmupTimer = dq.CreateTimer();
            _warmupTimer.IsRepeating = false;
            _warmupTimer.Tick += OnWarmupTimerTick;
        }
        _warmupTimer.Stop();
        _warmupTimer.Interval = WarmupTimeout;
        _warmupTimer.Start();
    }

    private void CancelWarmupTimer()
    {
        try { _warmupTimer?.Stop(); } catch { /* best-effort */ }
    }

    private void OnWarmupTimerTick(Microsoft.UI.Dispatching.DispatcherQueueTimer sender, object args)
        => DebugLog.SafeRun("DeepAnalyzeView.OnWarmupTimerTick", () =>
        {
            sender.Stop();
            if (_unloaded) return;
            // Only fire if we're still in the pre-progress warm-up window — if a
            // progress/last/complete event already landed, CancelWarmupTimer was
            // called and we shouldn't be here, but guard defensively.
            var ec = EngineClient.Instance;
            if (ec.DeepAnalyzeProgress is not null
                || ec.DeepAnalyzeLast is not null
                || ec.DeepAnalyzeComplete is not null)
            {
                return;
            }
            // Revert the optimistic UI so it doesn't look like a run is still in
            // flight, then surface a dismissible, actionable error.
            StreamCard.Visibility = Visibility.Collapsed;
            OverallProgress.IsIndeterminate = false;
            OverallProgress.Value = 0;
            OverallProgressText.Text = string.Empty;
            CancelButton.IsEnabled = false;
            AnalyzeAllButton.IsEnabled = true;
            SyncSelectionButtons();
            _ = ShowAlertAsync("The AI took too long to start",
                "The description model didn't finish loading in time. Try again in a moment.");
        });

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
            ProposedNamesPillText.Text = _proposedNameCount == 1 ? "1 file" : $"{_proposedNameCount:N0} files";
        }
        else
        {
            ProposedNamesPill.Visibility = Visibility.Collapsed;
        }
        ApplyAllText.Text = _proposedNameCount > 0
            ? $"Add tags and rename {_proposedNameCount:N0} file{(_proposedNameCount == 1 ? "" : "s")}"
            : "Add tags and names";
        var applyAllTip = _proposedNameCount > 0
            ? "Adds tags and people, and renames files to their suggested names. You confirm first."
            : "Adds tags and people's names to the files";
        ToolTipService.SetToolTip(ApplyAllButton, applyAllTip);
        AutomationProperties.SetName(ApplyAllButton, ApplyAllText.Text);
        SyncDeepAnalyzeControls();
    }

    /// <summary>open BulkRenameSheet pre-seeded with every
    /// VLM-proposed rename pending in the DB. One-click bulk-apply
    /// of the model's smart filename suggestions, no need to
    /// navigate to Library + multi-select first.</summary>
    private async void OnProposedNamesPillClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            // `using` so the SQLite connection + SemaphoreSlim are released on
            // every exit — the empty-pending early return and the catch path
            // below both used to leak the store.
            using var store = new Services.ReadStore(Services.AppPaths.DbPath);
            await store.OpenAsync();
            var pending = await store.PendingProposedRenamesAsync(200000, System.Threading.CancellationToken.None);
            if (pending.Count == 0)
            {
                _proposedNameCount = 0;
                SyncProposedNamesPill();
                return;
            }
            var plan = new System.Collections.Generic.List<Views.Library.RenamePlan>(pending.Count);
            foreach (var p in pending)
            {
                plan.Add(new Views.Library.RenamePlan
                {
                    FileId = p.Id,
                    CurrentPath = p.Path,
                    ProposedName = p.ProposedName,
                    Include = true,
                });
            }
            var sheet = new Views.Library.BulkRenameSheet();
            sheet.SetPlan(plan, suggested: true);
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = $"Rename {pending.Count:N0} file{(pending.Count == 1 ? "" : "s")} to the suggested names?",
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
        }
        catch (Exception ex)
        {
            DebugLog.Warn("OnProposedNamesPillClicked threw: " + ex.Message);
        }
    }

    private static string FormatEta(double seconds)
    {
        if (seconds < 60) return $"{Math.Max(1, seconds):F0} sec";
        if (seconds < 3600) return $"{seconds / 60:F0} min";
        var hours = seconds / 3600;
        if (hours > 99) return "99+ hr";
        return $"{hours:F1} hr";
    }

    private async System.Threading.Tasks.Task LoadStreamThumbAsync(string path)
    {
        // BitmapImage is a UI-thread DispatcherObject; constructing it off the UI
        // thread (this await can resume on a worker) is a native fast-fail. Capture
        // the dispatcher before any await and do the construct + StreamImage.Source
        // set inside one TryEnqueue; null the source on failure (placeholder, not stale).
        if (_unloaded) return;
        // Sequence guard: a slow decode for an earlier file must not overwrite a
        // later file's thumbnail. Capture this load's generation; only commit if
        // it's still the latest when the decode completes.
        var generation = System.Threading.Interlocked.Increment(ref _streamThumbGeneration);
        // In-proc shell video/audio thumbnail providers can native-fast-fail the
        // whole app (no managed exception). This path calls GetThumbnailAsync
        // directly, bypassing ThumbnailService, so it must apply the same skip —
        // single source of truth in ThumbnailService.SkipShellThumbnailForExtension.
        if (Services.ThumbnailService.SkipShellThumbnailForExtension(path))
        {
            // No shell thumbnail for video/audio — clear any prior file's image so
            // the stream card doesn't show a stale preview under the current
            // filename (placeholder, not stale). Mirrors the no-thumbnail paths below.
            ClearStreamImageOnDispatcher(DispatcherQueue, generation);
            return;
        }
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
                        // Only commit if this is still the latest load — a slower
                        // decode for an earlier file must not clobber a newer one.
                        if (!_unloaded
                            && System.Threading.Volatile.Read(ref _streamThumbGeneration) == generation)
                        {
                            StreamImage.Source = bmp;
                        }
                    }
                    catch (Exception ex)
                    {
                        DebugLog.Warn($"LoadStreamThumbAsync UI render: {ex.Message}");
                        if (!_unloaded
                            && System.Threading.Volatile.Read(ref _streamThumbGeneration) == generation)
                        {
                            try { StreamImage.Source = null; } catch { }
                        }
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
            ClearStreamImageOnDispatcher(dispatcher, generation);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"LoadStreamThumbAsync({PathRedactor.Redact(path)}) failed: {ex.Message}");
            ClearStreamImageOnDispatcher(dispatcher, generation);
        }
        finally
        {
            try { thumb?.Dispose(); } catch { }
        }
    }

    private void ClearStreamImageOnDispatcher(Microsoft.UI.Dispatching.DispatcherQueue? dispatcher, int generation)
    {
        if (dispatcher is null || _unloaded) return;
        // Only clear if this is still the latest load — a stale "no thumbnail"
        // result must not wipe a newer file's freshly-set image.
        dispatcher.TryEnqueue(() =>
        {
            if (_unloaded
                || System.Threading.Volatile.Read(ref _streamThumbGeneration) != generation)
            {
                return;
            }
            try { StreamImage.Source = null; } catch { }
        });
    }

    private void OnModelCardTapped(object sender, Microsoft.UI.Xaml.Input.TappedRoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: string id }) SelectModel(id);
    }

    private bool SelectModel(string id)
    {
        if (!AppSettings.IsAllowedVlmKind(id)) return false;
        if (PhysicalRamGB() is double ramGB && !Fits(id, ramGB)) return false;

        _activeModel = id;
        HighlightActiveCard();
        UpdateActiveModelLabel();
        SyncDeepAnalyzeControls();
        _ = RefreshNamePeopleGateAsync();
        try
        {
            // Use the shared settings instance so a pending debounced save
            // cannot overwrite this choice with another instance's snapshot.
            var settings = AppViewModel.Instance.Settings;
            if (settings.SelectedVlmModelKind != id)
            {
                settings.SelectedVlmModelKind = id;
                settings.Save();
            }
        }
        catch (Exception ex) { DebugLog.Warn("Persist VLM choice failed: " + ex.Message); }
        return true;
    }

    // Every `async void` handler below has the entire body inside a
    // try/catch. An async-void handler that throws kills the dispatcher
    // and crashes the window; the catch makes failures surface as log
    // entries instead.
    private async void OnInstallModelClicked(object sender, RoutedEventArgs e)
    {
        string? modelId = null;
        try
        {
            if (sender is not Button { Tag: string requestedKind }) return;
            var slot = ModelInstallerService.Instance.DeepVlm;
            if (slot.Status == ModelInstallStatus.Downloading || !SelectModel(requestedKind)) return;
            modelId = requestedKind;
            if (!await ModelLicenseGate.EnsureAcceptedAsync([modelId]))
            {
                DebugLog.Info($"VLM install cancelled because license terms for {modelId} were declined.");
                return;
            }
            // Mark the shared slot before the first await. Otherwise a second
            // card or Welcome Install All can prewarm a different VLM while
            // the first click is still waiting for engine progress.
            slot.ResetForRetry();
            slot.CurrentModelKind = modelId;
            slot.Status = ModelInstallStatus.Downloading;
            slot.Message = "Starting…";
            slot.LastProgressAt = DateTime.UtcNow;
            SyncCards();
            await EngineClient.Instance.PrewarmModelAsync(modelId);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("VLM install failed: " + ex);
            if (modelId is not null)
            {
                var slot = ModelInstallerService.Instance.DeepVlm;
                slot.CurrentModelKind = modelId;
                slot.Fail(ex.Message);
            }
        }
    }

    private async void OnAnalyzeAllClicked(object sender, RoutedEventArgs e)
    {
        if (EngineClient.Instance.GpuDeviceRemoved)
        {
            var reason = EngineClient.GpuRestartRequiredMessage;
            await ShowAlertAsync("Describing stopped", reason);
            return;
        }
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
            await EngineClient.Instance.DeepAnalyzeAllAsync(_activeModel, SkipExistingToggle.IsChecked == true, tagsOnly: false, proposeRenames: ProposeRenamesCheck.IsChecked == true);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("DeepAnalyzeAll failed: " + ex);
            var reason = EngineClient.Instance.GpuDeviceRemoved
                ? EngineClient.GpuRestartRequiredMessage
                : ex.Message;
            // Revert the optimistic state so the UI doesn't falsely look like a
            // run is in flight, then surface a dismissible error.
            StreamCard.Visibility = Visibility.Collapsed;
            CancelButton.IsEnabled = false;
            AnalyzeAllButton.IsEnabled = true;
            if (EngineClient.Instance.GpuDeviceRemoved)
            {
                await ShowAlertAsync("Describing stopped", reason);
            }
            else
            {
                await ShowAlertAsync("Describing stopped", ex.Message);
            }
        }
    }

    // ───── Item 5: "Apply to your files" ──────────────────────────────
    // Write keyword tags / named people / smart names onto the actual files.
    // Tags + people go through the engine's applyTags (sidecar + IPropertyStore),
    // grouped by tag/person so the call count is bounded. Smart names route
    // through the proven BulkRenameSheet (correct extension + uniqueness).
    private int _applyInFlight; // 0 = idle, 1 = an apply running

    private async void OnApplyTagsClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnApplyTagsClicked),
            () => RunApplyAsync(keywords: true, people: false, names: false));

    private async void OnApplyPeopleClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnApplyPeopleClicked),
            () => RunApplyAsync(keywords: false, people: true, names: false));

    private async void OnApplyAllClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnApplyAllClicked),
            () => RunApplyAsync(keywords: true, people: true, names: true));

    private async System.Threading.Tasks.Task RunApplyAsync(bool keywords, bool people, bool names)
    {
        if (System.Threading.Interlocked.CompareExchange(ref _applyInFlight, 1, 0) != 0) return;
        SetApplyBusy(true, "Adding…");
        int tagged = 0, peopled = 0;
        bool openRenameSheet = false;
        try
        {
            using var store = new Services.ReadStore(Services.AppPaths.DbPath);
            var ct = System.Threading.CancellationToken.None;
            if (keywords)
            {
                var map = await store.KeywordTagFileIdsAsync(ct);
                foreach (var kv in map)
                {
                    if (kv.Value.Count == 0) continue;
                    await EngineClient.Instance.ApplyTagsAsync(kv.Value, new[] { kv.Key }, "add");
                    tagged += kv.Value.Count;
                }
            }
            if (people)
            {
                var map = await store.NamedPersonFileIdsAsync(ct);
                foreach (var kv in map)
                {
                    if (kv.Value.Count == 0) continue;
                    await EngineClient.Instance.ApplyTagsAsync(kv.Value, new[] { kv.Key }, "add");
                    peopled += kv.Value.Count;
                }
            }
            if (names)
            {
                var pending = await store.PendingProposedRenamesAsync(200000, ct);
                openRenameSheet = pending.Count > 0;
            }
        }
        finally
        {
            System.Threading.Interlocked.Exchange(ref _applyInFlight, 0);
        }

        var parts = new System.Collections.Generic.List<string>();
        if (keywords) parts.Add($"{tagged} tagged");
        if (people) parts.Add($"{peopled} tagged with names");
        // Tags are written to both a sidecar JSON file and (for JPEG/PNG/MP4 etc.)
        // Explorer's Keywords property. Non-media extensions get sidecar-only —
        // Explorer's Details > Tags column stays blank for those, which is expected.
        string statusNote = (tagged > 0 || peopled > 0)
            ? " Tags show in File Explorer for supported media files (JPEG, PNG, MP4…); other types keep them in a separate tag file."
            : string.Empty;
        SetApplyBusy(false, parts.Count == 0
            ? "Nothing to add yet."
            : "Done: " + string.Join(", ", parts) + "." + statusNote);


        // Smart names go through the review sheet (correct extension + uniqueness
        // handling) rather than a blind bulk rename — safer for a destructive op,
        // and reuses the same path as the pill.
        if (openRenameSheet) OnProposedNamesPillClicked(this, new RoutedEventArgs());
    }

    private void SetApplyBusy(bool busy, string status)
    {
        if (_unloaded) return;
        try
        {
            ApplyTagsButton.IsEnabled = !busy;
            ApplyPeopleButton.IsEnabled = !busy && (_hasNamedAnyone || _eligibleTotal < 0);
            ApplyAllButton.IsEnabled = !busy;
            ApplyProgressRing.IsActive = busy;
            ApplyProgressRing.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
            ApplyStatusText.Text = status;
            ApplyStatusText.Visibility = string.IsNullOrEmpty(status) ? Visibility.Collapsed : Visibility.Visible;
        }
        catch (Exception ex) { DebugLog.Warn("SetApplyBusy UI update threw: " + ex.Message); }
    }

    // Analyzes every file currently selected in the Library view. We send
    // one DeepAnalyzeFile per file. Engine throttles parallelism via its
    // model pool; sending N requests just queues them up.
    private async void OnAnalyzeSelectedClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnAnalyzeSelectedClicked), async () =>
        {
            if (EngineClient.Instance.GpuDeviceRemoved)
            {
                await ShowAlertAsync("Restart engine", EngineClient.GpuRestartRequiredMessage
                    + " Use Restart Engine in the sidebar.");
                return;
            }
            var sel = SelectionRegistry.Instance.LibrarySelection;
            if (sel.Count == 0) return;
            _selectedRunCancelled = false;
            StreamCard.Visibility = Visibility.Visible;
            CancelButton.IsEnabled = true;
            foreach (var id in sel)
            {
                if (_selectedRunCancelled) break;
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
            if (EngineClient.Instance.GpuDeviceRemoved)
            {
                await ShowAlertAsync("Restart engine", EngineClient.GpuRestartRequiredMessage
                    + " Use Restart Engine in the sidebar.");
                return;
            }
            var id = SelectionRegistry.Instance.PreviewedFileId;
            if (id is null) return;
            StreamCard.Visibility = Visibility.Visible;
            CancelButton.IsEnabled = true;
            try { await EngineClient.Instance.DeepAnalyzeFileAsync(id.Value, _activeModel); }
            catch (Exception ex) { DebugLog.Warn($"DeepAnalyzeFile (current) failed: {ex.Message}"); }
        });

    private async void OnCancelClicked(object sender, RoutedEventArgs e)
    {
        // Also stop the per-file "Analyze Selected" send loop — the engine
        // cancel below only stops the file currently in flight.
        _selectedRunCancelled = true;
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
