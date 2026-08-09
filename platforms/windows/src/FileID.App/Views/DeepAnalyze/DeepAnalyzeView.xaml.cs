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
    private bool _hasNamedPeople;
    private long _namePeopleGateGeneration;

    // L5: set the moment Cancel is clicked while a Deep Analyze command is still
    // in flight (most importantly while it's QUEUED behind a running scan, where
    // the engine can't act on the cancel until the scan releases the mutation
    // gate). Without it Cancel stayed enabled with no feedback for the whole scan.
    // While set, Cancel is disabled and the stream card shows a 'Cancelling…'
    // state; cleared once the command leaves flight (terminal Complete / slot
    // release), re-arming Cancel for the next run.
    private bool _cancelRequested;

    // Monotonic generation for the streamed-thumbnail load. Each progress event
    // fires LoadStreamThumbAsync fire-and-forget; a slow decode for an earlier
    // file can resolve after a later one's. We bump this at the start of every
    // load and only commit StreamImage.Source if our captured generation is
    // still the latest, so a stale thumbnail never overwrites the current file's.
    private int _streamThumbGeneration;

    // Last file path shown in the stream card. The engine emits a
    // DeepAnalyzeProgress every ~250 ms (4 Hz) carrying the SAME CurrentPath for
    // the whole time a file is being captioned; reloading the shell thumbnail on
    // every one of those frames re-hits the shell thumbnail provider needlessly.
    // Track the displayed path and only reload the thumb + reset the caption
    // accumulator when the path actually changes. Reset on teardown / new run so
    // re-running the same file reloads its preview. (Fix B)
    private string? _lastStreamPath;

    // Warm-up watchdog: the engine emits DeepAnalyzeStarting (IsIndeterminate
    // "Preparing…") BEFORE the first DeepAnalyzeProgress/stream token while the
    // VLM loads (~5-30 s first run). If the load stalls there's no failure
    // event, so the spinner would otherwise spin forever. This timer fires if
    // no progress/stream token arrives in time; it's cancelled the moment any
    // progress/last/complete lands or the view unloads. Runs on the UI thread
    // (DispatcherQueueTimer.Tick), so touching XAML in the handler is safe.
    private static readonly TimeSpan WarmupTimeout = TimeSpan.FromSeconds(45);
    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _warmupTimer;
    private long _warmupAttemptId;
    private long _localPreparingAttemptId;

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
        _lastStreamPath = null;
        CancelWarmupTimer();
        ModelInstallerService.Instance.DeepVlm.PropertyChanged -= OnInstallerChanged;
        EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        EngineClient.Instance.DeepAnalyzeFileDoneReceived -= OnDeepAnalyzeFileDoneReceived;
        SelectionRegistry.Instance.PropertyChanged -= OnSelectionRegistryChanged;
        Loaded -= OnLoadedHandler;
        Unloaded -= OnUnloadedHandler;
    }

    private void OnLoadedHandler(object sender, RoutedEventArgs e)
    {
        _unloaded = false;
        _hasNamedPeople = false;
        ApplyPeopleButton.IsEnabled = false;
        ModelInstallerService.Instance.DeepVlm.PropertyChanged += OnInstallerChanged;
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        EngineClient.Instance.DeepAnalyzeFileDoneReceived += OnDeepAnalyzeFileDoneReceived;
        SelectionRegistry.Instance.PropertyChanged += OnSelectionRegistryChanged;
        SyncCards();
        UpdateActiveModelLabel();
        SyncStream();
        SyncSelectionButtons();
        // refresh the "Name people first" gate every time the
        // view loads; also refreshed in OnEngineChanged when face
        // clustering finishes.
        _ = RefreshNamePeopleGateAsync();
        SyncExplainerBanner();
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
        => DebugLog.SafeRun("DeepAnalyzeView.OnSelectionRegistryChanged",
            () => DispatcherQueue.TryEnqueue(SyncSelectionButtons));

    private void SyncSelectionButtons()
    {
        if (_unloaded) return;
        var sel = SelectionRegistry.Instance.LibrarySelection;
        bool canStart = !EngineClient.Instance.DeepAnalyzeCommandInFlight;
        AnalyzeSelectedButton.IsEnabled = canStart && sel.Count > 0;
        AnalyzeSelectedText.Text = sel.Count switch
        {
            0 => "Selected",
            1 => "Selected (1)",
            _ => $"Selected ({sel.Count})",
        };
        AnalyzeCurrentButton.IsEnabled = canStart && SelectionRegistry.Instance.HasPreviewedFile;
    }

    private void SyncDeepAnalyzeControls()
    {
        if (_unloaded) return;
        var inFlight = EngineClient.Instance.DeepAnalyzeCommandInFlight;
        // L5: once the command leaves flight (terminal reached / slot released),
        // re-arm Cancel for the next run.
        if (!inFlight) _cancelRequested = false;
        AnalyzeAllButton.IsEnabled = !inFlight;
        // L5: a pending cancel (esp. while queued behind a scan) disables Cancel so
        // the user gets feedback and can't spam it while the engine can't yet act.
        CancelButton.IsEnabled = inFlight && !_cancelRequested;
        SyncSelectionButtons();
    }

    /// <summary>query the DB for any person row with NULL
    /// name + first_name. Disables Analyze All + shows the gate banner
    /// when the count is non-zero.</summary>
    private async System.Threading.Tasks.Task RefreshNamePeopleGateAsync()
    {
        var generation = System.Threading.Interlocked.Increment(
            ref _namePeopleGateGeneration);
        int unnamed = 0;
        bool hasNamedPeople = false;
        try
        {
            var dbPath = AppPaths.DbPath;
            var summary = await System.Threading.Tasks.Task.Run(() =>
            {
                if (!System.IO.File.Exists(dbPath)) return (Unnamed: 0, HasNamed: false);
                using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                    new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                    {
                        DataSource = dbPath,
                        Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                    }.ToString());
                conn.Open();
                using var cmd = conn.CreateCommand();
                cmd.CommandText = """
                    SELECT
                        COALESCE(SUM(CASE WHEN TRIM(COALESCE(name, '') || COALESCE(title, '') ||
                                                     COALESCE(first_name, '') || COALESCE(middle_name, '') ||
                                                     COALESCE(last_name, '') || COALESCE(suffix, '')) = ''
                                          THEN 1 ELSE 0 END), 0),
                        COALESCE(SUM(CASE WHEN IFNULL(is_unknown, 0) = 0
                                                   AND TRIM(COALESCE(name, '') || COALESCE(title, '') ||
                                                            COALESCE(first_name, '') || COALESCE(middle_name, '') ||
                                                            COALESCE(last_name, '') || COALESCE(suffix, '')) != ''
                                          THEN 1 ELSE 0 END), 0)
                    FROM persons;
                    """;
                using var reader = cmd.ExecuteReader();
                if (!reader.Read()) return (Unnamed: 0, HasNamed: false);
                var unnamedCount = Math.Min(reader.GetInt64(0), int.MaxValue);
                return (Unnamed: (int)unnamedCount, HasNamed: reader.GetInt64(1) > 0);
            }).ConfigureAwait(false);
            unnamed = summary.Unnamed;
            hasNamedPeople = summary.HasNamed;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("RefreshNamePeopleGateAsync failed: " + ex.Message);
        }
        if (_unloaded
            || generation != System.Threading.Interlocked.Read(
                ref _namePeopleGateGeneration))
        {
            return;
        }
        DispatcherQueue.TryEnqueue(() =>
        {
            if (_unloaded
                || generation != System.Threading.Interlocked.Read(
                    ref _namePeopleGateGeneration))
            {
                return;
            }
            _hasNamedPeople = hasNamedPeople;
            if (unnamed > 0)
            {
                NamePeopleGateBanner.Visibility = Visibility.Visible;
                NamePeopleGateText.Text = unnamed == 1
                    ? "1 face cluster isn't named yet. Naming it first gives sharper captions — or analyze now and name later."
                    : $"{unnamed} face clusters aren't named yet. Naming them first gives sharper captions — or analyze now and name later.";
                ToolTipService.SetToolTip(AnalyzeAllButton, null);
            }
            else
            {
                NamePeopleGateBanner.Visibility = Visibility.Collapsed;
                ToolTipService.SetToolTip(AnalyzeAllButton, null);
            }
            SyncDeepAnalyzeControls();
            SetApplyBusy(
                System.Threading.Volatile.Read(ref _applyInFlight) != 0,
                ApplyStatusText.Text);
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

    private void OnDeepAnalyzeFileDoneReceived(DeepAnalyzeFileDone fileDone)
    {
        if (_unloaded) return;
        DispatcherQueue.TryEnqueue(() =>
        {
            if (!_unloaded) ConsumeFileDone(fileDone);
        });
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("DeepAnalyzeView.OnEngineChanged", () =>
        {
            if (_unloaded) return;
            switch (e.PropertyName)
            {
                case nameof(EngineClient.DeepAnalyzeCommandInFlight):
                case nameof(EngineClient.DeepAnalyzeStarting):
                case nameof(EngineClient.DeepAnalyzeProgress):
                case nameof(EngineClient.DeepAnalyzeLast):
                case nameof(EngineClient.DeepAnalyzeComplete):
                // QueueState drives the "queued behind a scan" branch of
                // SyncStream: the pending deepAnalyze job can land after the
                // command-in-flight flip, so re-sync to disarm the warm-up
                // watchdog once the queued state becomes known. (Fix A)
                case nameof(EngineClient.QueueState):
                    DebugLog.Debug($"[ENGINE-SUB:DeepAnalyzeView] {e.PropertyName}");
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncStream(); });
                    break;
                case nameof(EngineClient.Phase):
                    DebugLog.Debug($"[ENGINE-SUB:DeepAnalyzeView] {e.PropertyName}");
                    _ = RefreshNamePeopleGateAsync();
                    // Phase is the immediate signal that a Deep Analyze command is
                    // queued behind a scan (or that the scan just finished and the
                    // job can now load); re-sync so the queued/prepare state and the
                    // warm-up watchdog track it. (Fix A)
                    DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncStream(); });
                    break;
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
        var profile = VlmRecommendation.CurrentProfile();
        ApplyVlmCard(MistralCard, MistralStatus, MistralProgress, MistralInstallButton, "mistral_small_3_2", slot, profile);
        ApplyVlmCard(QwenLargeCard, QwenLargeStatus, QwenLargeProgress, QwenLargeInstallButton, "qwen2_5_vl_7b", slot, profile);
        ApplyVlmCard(GemmaCard, GemmaStatus, GemmaProgress, GemmaInstallButton, "gemma_3_4b", slot, profile);
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
            // Map the snake_case wire kind to the registry's dotted dir
            // ("vlm/mistral-small-3.2") — joining the kind itself never found
            // installed weights, so every card showed Install on a complete
            // install (the app-side twin of the engine's find_weights bug).
            var dir = System.IO.Path.Combine(AppPaths.ModelsDir, "vlm", Services.VlmWeightDirs.DirNameFor(kind));
            return System.IO.File.Exists(System.IO.Path.Combine(dir, "model.gguf"))
                && System.IO.File.Exists(System.IO.Path.Combine(dir, "mmproj.gguf"));
        }
        catch { return false; }
    }

    private bool ActiveModelReady(out string reason)
    {
        if (EngineClient.Instance.GpuDeviceRemoved)
        {
            reason = EngineClient.GpuRestartRequiredMessage;
            return false;
        }
        var profile = VlmRecommendation.CurrentProfile();
        if (profile.TotalRamGb > 0 && !VlmRecommendation.CanRun(_activeModel, profile))
        {
            reason = $"{VlmRecommendation.DisplayName(_activeModel)} is still your selected model, but current available memory is too low to run it safely. Choose a lighter model.";
            return false;
        }
        if (!VlmWeightsPresent(_activeModel))
        {
            reason = $"Install {VlmRecommendation.DisplayName(_activeModel)} before starting Deep Analyze.";
            return false;
        }
        if (!SentinelProbe.Installed("llama_runtime_x64"))
        {
            reason = "Finish installing the local llama.cpp runtime before starting Deep Analyze.";
            return false;
        }
        reason = string.Empty;
        return true;
    }

    private static void ApplyVlmCard(Border card, TextBlock status, ProgressBar bar, Button installButton, string kind, ModelSlot slot, VlmHardwareProfile profile)
    {
        if (profile.TotalRamGb > 0 && !VlmRecommendation.CanRun(kind, profile))
        {
            status.Text = $"Needs ~{VlmRecommendation.WorkingSetGb(kind):0.#} GB working memory (this PC has {profile.TotalRamGb:0.#} GB RAM)";
            status.Foreground = ThemeHelper.GetBrushSafe("DestructiveTextBrush");
            bar.Visibility = Visibility.Collapsed;
            installButton.IsEnabled = false;
            ToolTipService.SetToolTip(card,
                "System RAM has a hard safety floor even when the GPU has substantial VRAM. Pick a smaller model for this PC.");
            card.Opacity = 0.55;
            card.IsHitTestVisible = false;
            return;
        }

        var installed = VlmWeightsPresent(kind);
        if (!installed && !VlmRecommendation.HasDiskFor(kind, profile.FreeDiskBytes))
        {
            var needGb = VlmRecommendation.RequiredFreeBytes(kind) / (1024.0 * 1024 * 1024);
            status.Text = $"Needs ~{needGb:0.#} GB free while downloading";
            status.Foreground = ThemeHelper.GetBrushSafe("DestructiveTextBrush");
            bar.Visibility = Visibility.Collapsed;
            installButton.IsEnabled = false;
            ToolTipService.SetToolTip(card, "Free space on the models drive, then try again. Downloads stage verified parts beside the final model.");
            card.Opacity = 0.7;
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
        else if (installed)
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
            "qwen2_5_vl_7b" => "Active model: Qwen 2.5-VL 7B (balanced)",
            "gemma_3_4b" => "Active model: Gemma 3 4B (lightest)",
            "mistral_small_3_2" => "Active model: Mistral-Small 3.2 (max quality)",
            _ => "Active model: Qwen 2.5-VL 7B (balanced)",
        };
    }

    private int _proposedNameCount;
    // DeepAnalyzeLast is a latched EngineClient property: the engine nulls it
    // only on DeepAnalyzeStarting and overwrites it on the next (throttled)
    // FileDone — it is NOT cleared on a progress event. Remember the instance
    // already consumed so the last-result effects run exactly once per file and
    // a later file's progress tick can't re-process the previous file's result.
    private FileID.IpcSchema.DeepAnalyzeFileDone? _lastConsumedFileDone;
    // Count of FileDone events consumed since the live card's streamer file last
    // changed. The engine streams only the wave's lowest-idx (streamer) file on
    // the live current_path/current_caption channel, but fires a terminal
    // FileDone for EVERY concurrent wave member — and FileDone carries no path,
    // so a sibling's FileDone can't be matched to the displayed streamer. In a
    // sequential run (one file per wave) exactly one FileDone lands per streamer,
    // so only the FIRST FileDone since the streamer changed may write the live
    // card's caption/proposed-name; later siblings only feed the pill tally.
    private int _fileDonesThisStreamer;

    private void SyncStream()
    {
        var ec = EngineClient.Instance;
        var starting = ec.DeepAnalyzeStarting;
        var prog = ec.DeepAnalyzeProgress;
        var last = ec.DeepAnalyzeLast;
        var complete = ec.DeepAnalyzeComplete;
        SyncDeepAnalyzeControls();

        if (starting is null && prog is null && last is null && complete is null)
        {
            if (ec.DeepAnalyzeCommandInFlight)
            {
                var attemptId = ec.DeepAnalyzeCommandAttemptId;
                if (_localPreparingAttemptId != attemptId)
                {
                    _localPreparingAttemptId = attemptId;
                    _proposedNameCount = 0;
                    _lastConsumedFileDone = null;
                    _fileDonesThisStreamer = 0;
                    _lastStreamPath = null;
                    SyncProposedNamesPill();
                }
                StreamCard.Visibility = Visibility.Visible;
                OverallProgress.Value = 0;
                OverallProgress.IsIndeterminate = true;
                StreamProposedNameText.Text = string.Empty;
                StreamCaptionText.Text = string.Empty;
                if (ec.DeepAnalyzeQueuedBehindScan)
                {
                    // Queued behind a running scan on the engine's mutation gate.
                    // While queued the engine emits only QueueState — no
                    // DeepAnalyzeStarting/Progress — so arming the warm-up watchdog
                    // would false-fire "Model took too long to load" at 45 s on a
                    // healthy job that is merely waiting. Show the truth and disarm;
                    // the DeepAnalyzeStarting that fires when the job actually begins
                    // loading re-arms the watchdog via the branch below. (Fix A)
                    CancelWarmupTimer();
                    if (_cancelRequested)
                    {
                        // L5: Cancel was pressed while queued — the engine can't act
                        // until the scan releases the gate, so show that we heard it.
                        OverallProgressText.Text = "Cancelling…";
                        StreamFileNameText.Text = "Cancelling — will stop when the current scan finishes…";
                    }
                    else
                    {
                        OverallProgressText.Text = "Queued";
                        StreamFileNameText.Text = "Queued — waiting for the current scan to finish…";
                    }
                }
                else if (_cancelRequested)
                {
                    // L5: cancelled during the pre-load prepare window — don't arm
                    // the warm-up watchdog (a cancelled job that never loads must not
                    // trip "model took too long").
                    CancelWarmupTimer();
                    OverallProgressText.Text = "Cancelling…";
                    StreamFileNameText.Text = "Cancelling…";
                }
                else
                {
                    OverallProgressText.Text = "Preparing…";
                    StreamFileNameText.Text = "Preparing Deep Analyze…";
                    ArmWarmupTimer(attemptId);
                }
            }
            else if (_localPreparingAttemptId != 0)
            {
                _localPreparingAttemptId = 0;
                _lastStreamPath = null;
                _fileDonesThisStreamer = 0;
                CancelWarmupTimer();
                StreamCard.Visibility = Visibility.Collapsed;
                OverallProgress.IsIndeterminate = false;
                OverallProgress.Value = 0;
                OverallProgressText.Text = string.Empty;
            }
            return;
        }

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
            StreamFileNameText.Text = $"{starting.Phase}: {starting.ModelKind}";
            StreamCaptionText.Text = starting.Message ?? string.Empty;
            StreamProposedNameText.Text = string.Empty;
            // Reset the smart-rename tally at the start of each run so the pill
            // reflects only THIS run, not a cumulative count across runs. (audit A13)
            _proposedNameCount = 0;
            _lastConsumedFileDone = null;
            _fileDonesThisStreamer = 0;
            _lastStreamPath = null;
            SyncProposedNamesPill();
            OverallProgress.Value = 0;
            OverallProgress.IsIndeterminate = true;
            OverallProgressText.Text = "Preparing…";
            // Arm the warm-up watchdog so a stalled model load surfaces a
            // dismissible error + reverts the optimistic UI instead of
            // spinning "Preparing…" indefinitely.
            ArmWarmupTimer(ec.DeepAnalyzeCommandAttemptId);
        }

        if (prog is not null)
        {
            StreamCard.Visibility = Visibility.Visible;
            OverallProgress.IsIndeterminate = false;

            var pct = prog.Total == 0 ? 0 : (double)prog.Processed / prog.Total;
            OverallProgress.Value = pct;
            // include ETA + processed/total + per-second rate
            // when the engine reports it.
            var etaSuffix = prog.EtaSeconds is double eta && eta > 0
                ? $" · {FormatEta(eta)} left"
                : string.Empty;
            OverallProgressText.Text = $"{prog.Processed} / {prog.Total} files{etaSuffix}";

            if (!string.IsNullOrEmpty(prog.CurrentPath)
                && !string.Equals(prog.CurrentPath, _lastStreamPath, StringComparison.Ordinal))
            {
                _lastStreamPath = prog.CurrentPath;
                StreamFileNameText.Text = Path.GetFileName(prog.CurrentPath);
                _ = LoadStreamThumbAsync(prog.CurrentPath);
                _captionAccumulator = string.Empty;
                StreamCaptionText.Text = string.Empty;
                // New streamer file: clear the prior file's proposed name and
                // re-open the single-FileDone window so this file's own terminal
                // caption/proposed-name may render (and a concurrent sibling's
                // may not).
                StreamProposedNameText.Text = string.Empty;
                _fileDonesThisStreamer = 0;
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

        // PropertyChanged remains a compatibility/replay path. The hot
        // DeepAnalyzeFileDoneReceived event carries every back-to-back payload
        // into the dispatcher; the reference guard prevents double consumption.
        if (last is not null)
        {
            ConsumeFileDone(last);
        }

        if (complete is not null)
        {
            _localPreparingAttemptId = 0;
            OverallProgress.IsIndeterminate = false;
            OverallProgressText.Text = complete.Cancelled
                ? $"Cancelled ({complete.Processed} done, {complete.Failed} failed)"
                : $"Done — {complete.Processed} captioned in {complete.TotalSeconds:0.#}s ({complete.Failed} failed)";
            SyncProposedNamesPill();
        }
    }

    private void ConsumeFileDone(DeepAnalyzeFileDone fileDone)
    {
        if (ReferenceEquals(fileDone, _lastConsumedFileDone)) return;
        _lastConsumedFileDone = fileDone;
        _fileDonesThisStreamer++;
        var hasProposed = !string.IsNullOrEmpty(fileDone.ProposedName);
        if (hasProposed)
        {
            _proposedNameCount++;
            SyncProposedNamesPill();
        }
        if (_fileDonesThisStreamer == 1)
        {
            StreamCaptionText.Text = fileDone.Description ?? string.Empty;
            StreamProposedNameText.Text = hasProposed
                ? $"Proposed name: {fileDone.ProposedName}"
                : string.Empty;
        }
    }

    // (Re)arm the warm-up watchdog. Always restarts the interval so a fresh
    // DeepAnalyzeStarting (e.g. a re-queued run, or a phase transition still in
    // the pre-progress window) gives the model the full WarmupTimeout to load.
    private void ArmWarmupTimer(long attemptId)
    {
        if (_unloaded || attemptId == 0) return;
        var dq = DispatcherQueue;
        if (dq is null) return;
        if (_warmupTimer is null)
        {
            _warmupTimer = dq.CreateTimer();
            _warmupTimer.IsRepeating = false;
            _warmupTimer.Tick += OnWarmupTimerTick;
        }
        _warmupTimer.Stop();
        _warmupAttemptId = attemptId;
        _warmupTimer.Interval = WarmupTimeout;
        _warmupTimer.Start();
    }

    private void CancelWarmupTimer()
    {
        _warmupAttemptId = 0;
        try { _warmupTimer?.Stop(); } catch { /* best-effort */ }
    }

    private void OnWarmupTimerTick(Microsoft.UI.Dispatching.DispatcherQueueTimer sender, object args)
        => DebugLog.SafeRun("DeepAnalyzeView.OnWarmupTimerTick", () =>
        {
            sender.Stop();
            var attemptId = _warmupAttemptId;
            _warmupAttemptId = 0;
            if (_unloaded || attemptId == 0) return;
            // Only fire if we're still in the pre-progress warm-up window — if a
            // progress/last/complete event already landed, CancelWarmupTimer was
            // called and we shouldn't be here, but guard defensively.
            var ec = EngineClient.Instance;
            if (ec.DeepAnalyzeCommandAttemptId != attemptId
                || ec.DeepAnalyzeProgress is not null
                || ec.DeepAnalyzeLast is not null
                || ec.DeepAnalyzeComplete is not null)
            {
                return;
            }
            // Ownership remains authoritative: warn without making controls look
            // idle while this exact engine attempt is still active.
            SyncDeepAnalyzeControls();
            _ = ShowAlertAsync("Model took too long to load",
                "The Deep Analyze model didn't finish loading in time. Check the engine logs and try again.");
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
            // `using` so the SQLite connection + SemaphoreSlim are released on
            // every exit — the empty-pending early return and the catch path
            // below both used to leak the store.
            await using var store = new Services.ReadStore(Services.AppPaths.DbPath);
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
        if (sender is FrameworkElement el && el.Tag is string id)
        {
            // Don't let the user select a model that would OOM-kill the engine —
            // mirrors the macOS `guard fits else { return }`. The card is also
            // IsHitTestVisible=false in that state, but guard here defensively.
            var profile = VlmRecommendation.CurrentProfile();
            if (profile.TotalRamGb > 0 && !VlmRecommendation.CanRun(id, profile)) return;
            _activeModel = id;
            HighlightActiveCard();
            UpdateActiveModelLabel();
            // Persist so the next launch (and the post-clustering auto-
            // chain) caption with the same model the user just picked.
            try
            {
                // Shared singleton, not a fresh Load() — avoids the static-debounce
                // lost-update where a fresh instance's Save() cancels the singleton's
                // pending write. (audit A8)
                var s = AppViewModel.Instance.Settings;
                s.SelectedVlmModelKind = id;
                s.SelectedVlmModelWasUserChosen = true;
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
            await ModelInstallerService.Instance.InstallDeepVlmAsync(modelId);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("VLM install failed: " + ex);
        }
    }

    private async void OnAnalyzeAllClicked(object sender, RoutedEventArgs e)
    {
        if (!ActiveModelReady(out var reason))
        {
            await ShowAlertAsync("Deep Analyze isn't ready", reason);
            return;
        }
        try
        {
            // Manual pass = full enrichment (caption + smart-rename + tags), so
            // tagsOnly stays false. The background auto-pass uses tagsOnly:true.
            await EngineClient.Instance.DeepAnalyzeAllAsync(_activeModel, SkipExistingToggle.IsOn, tagsOnly: false,
                proposeRenames: ProposeRenamesCheck.IsChecked == true,
                excludedFolders: AppViewModel.Instance.Settings.DeepAnalyzeExcludedFolders);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("DeepAnalyzeAll failed: " + ex);
            await ShowAlertAsync("Couldn't start Deep Analyze",
                "Deep Analyze couldn't be started: " + ex.Message +
                "\n\nMake sure the model is installed and the engine is running, then try again.");
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
        SetApplyBusy(true, "Applying…");
        int tagged = 0, peopled = 0, applyFailed = 0;
        bool openRenameSheet = false;
        IReadOnlyDictionary<long, List<string>>? priorUserTags = null;
        var confirmedTagFileIds = new HashSet<long>();
        try
        {
            await using var store = new Services.ReadStore(Services.AppPaths.DbPath);
            await store.OpenAsync();
            var ct = System.Threading.CancellationToken.None;
            IReadOnlyDictionary<string, List<long>>? keywordMap = null;
            IReadOnlyDictionary<string, List<long>>? peopleMap = null;
            if (keywords)
            {
                keywordMap = await store.KeywordTagFileIdsAsync(ct);
            }
            if (people)
            {
                peopleMap = await store.NamedPersonFileIdsAsync(ct);
            }
            var requestedTagFileIds = (keywordMap?.Values ?? [])
                .Concat(peopleMap?.Values ?? [])
                .SelectMany(ids => ids)
                .Distinct()
                .ToArray();
            priorUserTags = await Services.TagChangeJournal
                .CapturePriorUserTagsAsync(requestedTagFileIds);

            if (keywordMap is not null)
            {
                foreach (var kv in keywordMap)
                {
                    if (kv.Value.Count == 0) continue;
                    var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                        "applyTags",
                        () => EngineClient.Instance.ApplyTagsAsync(kv.Value, new[] { kv.Key }, "add"),
                        Services.BulkActionTimeout.ForFileCount(kv.Value.Count));
                    tagged += (int)result.Succeeded;
                    applyFailed += (int)result.Failed;
                    foreach (var fileId in Services.BulkActionResultTruth
                                 .ConfirmedSuccessfulFileIds(result, kv.Value))
                    {
                        confirmedTagFileIds.Add(fileId);
                    }
                }
            }
            if (peopleMap is not null)
            {
                foreach (var kv in peopleMap)
                {
                    if (kv.Value.Count == 0) continue;
                    var result = await EngineClient.Instance.WaitForBulkActionResultAsync(
                        "applyTags",
                        () => EngineClient.Instance.ApplyTagsAsync(kv.Value, new[] { kv.Key }, "add"),
                        Services.BulkActionTimeout.ForFileCount(kv.Value.Count));
                    peopled += (int)result.Succeeded;
                    applyFailed += (int)result.Failed;
                    foreach (var fileId in Services.BulkActionResultTruth
                                 .ConfirmedSuccessfulFileIds(result, kv.Value))
                    {
                        confirmedTagFileIds.Add(fileId);
                    }
                }
            }
            if (names)
            {
                var pending = await store.PendingProposedRenamesAsync(5000, ct);
                openRenameSheet = pending.Count > 0;
            }
        }
        catch
        {
            // The SetApplyBusy(false, …) that clears the busy UI lives after the
            // try and is skipped when an awaited engine/DB call throws — clear it
            // here so the card doesn't wedge on "Applying…". Rethrow so the outer
            // SafeRunAsync still logs the original exception.
            SetApplyBusy(false, "Couldn't apply — check the engine and try again.");
            throw;
        }
        finally
        {
            if (priorUserTags is not null && confirmedTagFileIds.Count > 0)
            {
                var confirmed = confirmedTagFileIds.OrderBy(id => id).ToArray();
                Services.TagChangeJournal.PushUndo(
                    Services.TagChangeJournal.FormatLabel("add", confirmed.Length),
                    confirmed,
                    priorUserTags);
            }
            System.Threading.Interlocked.Exchange(ref _applyInFlight, 0);
        }

        var parts = new System.Collections.Generic.List<string>();
        if (keywords) parts.Add($"{tagged} tagged");
        if (people) parts.Add($"{peopled} people-tagged");
        if (applyFailed > 0) parts.Add($"{applyFailed} failed");
        SetApplyBusy(false, parts.Count == 0
            ? "Nothing to apply yet."
            : (applyFailed == 0 ? "Applied — " : "Finished — ") + string.Join(", ", parts) + ".");
        if (applyFailed > 0)
        {
            await ShowAlertAsync("Some tags couldn't be applied",
                $"{applyFailed:N0} file{(applyFailed == 1 ? "" : "s")} failed. The successful tags were kept; check the engine log, then try again.");
        }

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
            ApplyPeopleButton.IsEnabled = !busy && _hasNamedPeople;
            ApplyAllButton.IsEnabled = !busy;
            ApplyProgressRing.IsActive = busy;
            ApplyProgressRing.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
            ApplyStatusText.Text = status;
            ApplyStatusText.Visibility = string.IsNullOrEmpty(status) ? Visibility.Collapsed : Visibility.Visible;
        }
        catch (Exception ex) { DebugLog.Warn("SetApplyBusy UI update threw: " + ex.Message); }
    }

    // Analyze the current Library selection as one bounded engine batch so the
    // multi-GB VLM loads once and the existing persistent-server wave scheduler
    // handles progress, cancellation, and per-file terminals.
    private async void OnAnalyzeSelectedClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnAnalyzeSelectedClicked), async () =>
        {
            if (!ActiveModelReady(out var reason))
            {
                await ShowAlertAsync("Deep Analyze isn't ready", reason);
                return;
            }
            var selected = SelectionRegistry.Instance.LibrarySelection.ToArray();
            if (selected.Length == 0) return;
            try
            {
                await EngineClient.Instance.DeepAnalyzeAllAsync(
                    _activeModel,
                    skipExisting: false,
                    tagsOnly: false,
                    proposeRenames: ProposeRenamesCheck.IsChecked == true,
                    fileIds: selected);
            }
            catch (Exception ex)
            {
                DebugLog.Warn($"Deep Analyze selected batch failed: {ex.Message}");
                await ShowAlertAsync("Deep Analyze stopped", ex.Message);
            }
        });

    // Analyzes the file currently open in FilePreviewSheet. The preview
    // sheet publishes its file id to SelectionRegistry on open + clears
    // it on close, so this button is only enabled while a sheet is up.
    private async void OnAnalyzeCurrentClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnAnalyzeCurrentClicked), async () =>
        {
            if (!ActiveModelReady(out var reason))
            {
                await ShowAlertAsync("Deep Analyze isn't ready", reason);
                return;
            }
            var id = SelectionRegistry.Instance.PreviewedFileId;
            if (id is null) return;
            try
            {
                await EngineClient.Instance.DeepAnalyzeFileAsync(id.Value, _activeModel);
            }
            catch (Exception ex)
            {
                DebugLog.Warn($"DeepAnalyzeFile (current) failed: {ex.Message}");
                await ShowAlertAsync("Deep Analyze stopped", ex.Message);
            }
        });

    private async void OnCancelClicked(object sender, RoutedEventArgs e)
    {
        // L5: latch a 'Cancelling…' state + disable Cancel now. When the command
        // is queued behind a scan the engine can't act until the scan releases the
        // gate, so without this the button stayed enabled with no feedback for the
        // whole scan. SyncStream reflects the state; SyncDeepAnalyzeControls clears
        // the latch once the command leaves flight.
        if (EngineClient.Instance.DeepAnalyzeCommandInFlight)
        {
            _cancelRequested = true;
            SyncStream();
        }
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
