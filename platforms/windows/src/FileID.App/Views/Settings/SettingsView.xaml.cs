// SettingsView code-behind.

using System;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Settings;

public sealed partial class SettingsView : UserControl, INotifyPropertyChanged
{
    private bool _unloaded;
    private bool _initializingToggles;

    /// <summary>The shared installer singleton, so the model cards x:Bind to its slots and stay current when another sheet installs a model.</summary>
    internal Services.ModelInstallerService Svc => Services.ModelInstallerService.Instance;

    public SettingsView()
    {
        InitializeComponent();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Svc.PropertyChanged += OnInstallerChanged;
        Svc.Clip.PropertyChanged += OnSlotChanged;
        Svc.Arcface.PropertyChanged += OnSlotChanged;
        Svc.RamPlus.PropertyChanged += OnSlotChanged;
        Svc.FaceMesh.PropertyChanged += OnSlotChanged;
        Svc.DeepVlm.PropertyChanged += OnSlotChanged;
        Unloaded += (_, _) =>
        {
            _unloaded = true;
            EngineClient.Instance.PropertyChanged -= OnEngineChanged;
            Svc.PropertyChanged -= OnInstallerChanged;
            Svc.Clip.PropertyChanged -= OnSlotChanged;
            Svc.Arcface.PropertyChanged -= OnSlotChanged;
            Svc.RamPlus.PropertyChanged -= OnSlotChanged;
            Svc.FaceMesh.PropertyChanged -= OnSlotChanged;
            Svc.DeepVlm.PropertyChanged -= OnSlotChanged;
        };
        Loaded += (_, _) =>
        {
            HydrateToggles();
            // A model may have been installed from another sheet while this tab was inactive.
            try { Svc.Refresh(); } catch { }
            try { SyncInstallButtonStates(); } catch { }
            // Set()'s equality short-circuit can swallow PropertyChanged when Refresh() writes an unchanged status, so force the bindings.
            try { DispatcherQueue.TryEnqueue(() => Bindings.Update()); } catch { }
            try { _ = PopulateRecentScansAsync(); } catch { }
        };
    }

    private async System.Threading.Tasks.Task PopulateRecentScansAsync()
    {
        var rows = await System.Threading.Tasks.Task.Run(() =>
        {
            var list = new System.Collections.Generic.List<(double started, double? completed, long total, string root, string? status)>();
            try
            {
                if (!System.IO.File.Exists(AppPaths.DbPath)) return list;
                using var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                    new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                    {
                        DataSource = AppPaths.DbPath,
                        Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
                        Pooling = false,
                    }.ToString());
                conn.Open();
                using var cmd = conn.CreateCommand();
                cmd.CommandText = """
                    SELECT started_at, completed_at, COALESCE(total_files, 0), COALESCE(root_path, ''), COALESCE(status, '')
                    FROM scan_sessions
                    ORDER BY started_at DESC
                    LIMIT 5
                    """;
                using var rdr = cmd.ExecuteReader();
                while (rdr.Read())
                {
                    list.Add((
                        rdr.IsDBNull(0) ? 0 : rdr.GetDouble(0),
                        rdr.IsDBNull(1) ? (double?)null : rdr.GetDouble(1),
                        rdr.IsDBNull(2) ? 0L : rdr.GetInt64(2),
                        rdr.IsDBNull(3) ? string.Empty : rdr.GetString(3),
                        rdr.IsDBNull(4) ? null : rdr.GetString(4)));
                }
            }
            catch { /* DB unavailable — leave list empty */ }
            return list;
        }).ConfigureAwait(true);

        if (_unloaded) return;
        // The view may have unloaded during the DB read; a throw here would fast-fail the dispatcher.
        try
        {
            if (rows.Count == 0)
            {
                RecentScansEmptyText.Visibility = Visibility.Visible;
                RecentScansList.ItemsSource = null;
                return;
            }
            RecentScansEmptyText.Visibility = Visibility.Collapsed;
            var items = new System.Collections.Generic.List<string>();
            foreach (var (started, completed, total, root, status) in rows)
            {
                var startedDt = DateTimeOffset.FromUnixTimeSeconds((long)started).LocalDateTime;
                var when = startedDt.ToString("yyyy-MM-dd HH:mm");
                var dur = completed.HasValue && completed.Value > started
                    ? FormatDuration(completed.Value - started)
                    : (status == "running" ? "running…" : "—");
                // Failed and cancelled sessions must not read like clean completions.
                var statusNote = status switch
                {
                    "failed" => "  ·  failed",
                    "cancelled" => "  ·  cancelled",
                    _ => string.Empty,
                };
                var rootShort = string.IsNullOrEmpty(root)
                    ? "(unknown root)"
                    : System.IO.Path.GetFileName(root.TrimEnd('\\', '/'));
                items.Add($"• {when}  ·  {total:N0} files  ·  {dur}{statusNote}  ·  {rootShort}");
            }
            RecentScansList.ItemsSource = items;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("PopulateRecentScans UI update threw (view unloaded?): " + ex.Message);
        }
    }

    private static string FormatDuration(double seconds)
    {
        if (seconds < 1) return "<1s";
        if (seconds < 60) return $"{seconds:F0}s";
        var ts = TimeSpan.FromSeconds(seconds);
        if (ts.TotalHours >= 1) return $"{(int)ts.TotalHours}h{ts.Minutes:00}m";
        return $"{ts.Minutes}m{ts.Seconds:00}s";
    }

    /// <summary>Probes the engine's install sentinels (written atomically after a successful prewarm) and marks the CUDA llama.cpp button and the ArcFace, CLIP and VLM slots installed when present.</summary>
    private void SyncInstallButtonStates()
    {
        if (SentinelExists("llama_runtime_cuda_x64"))
        {
            InstallCudaLlamaButton.Content = "Installed";
            InstallCudaLlamaButton.IsEnabled = false;
            CudaLlamaStatusText.Text = "✓ CUDA llama.cpp is installed. Describe Files uses it on the next run.";
        }
        // The engine installs SCRFD and ArcFace together under the "arcface" sentinel.
        if (SentinelExists("arcface") && Svc.Arcface.Status != Services.ModelInstallStatus.Installed)
        {
            DispatcherQueue.TryEnqueue(() =>
            {
                Svc.Arcface.Status = Services.ModelInstallStatus.Installed;
                Svc.Arcface.Fraction = 1.0;
            });
        }
        // CLIP needs both the image and the text encoder on disk.
        if (SentinelExists("mobileclip_s2") && SentinelExists("clip_text")
            && Svc.Clip.Status != Services.ModelInstallStatus.Installed)
        {
            DispatcherQueue.TryEnqueue(() =>
            {
                Svc.Clip.Status = Services.ModelInstallStatus.Installed;
                Svc.Clip.Fraction = 1.0;
            });
        }
        // Any supported VLM marks the shared onboarding slot installed.
        if ((SentinelExists("qwen2_5_vl_7b") || SentinelExists("qwen3_vl_4b")
             || SentinelExists("qwen3_vl_8b") || SentinelExists("gemma_3_4b")
             || SentinelExists("mistral_small_3_2"))
            && Svc.DeepVlm.Status != Services.ModelInstallStatus.Installed)
        {
            DispatcherQueue.TryEnqueue(() =>
            {
                Svc.DeepVlm.Status = Services.ModelInstallStatus.Installed;
                Svc.DeepVlm.Fraction = 1.0;
            });
        }
    }

    private static bool SentinelExists(string modelId) => SentinelProbe.Installed(modelId);

    private void OnInstallerChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_unloaded) return;
        if (e.PropertyName is nameof(Services.ModelInstallerService.AllInstalled)
                           or nameof(Services.ModelInstallerService.IsBusy))
        {
            DispatcherQueue.TryEnqueue(() => { if (!_unloaded) Bindings.Update(); });
        }
    }

    private void OnSlotChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_unloaded) return;
        DispatcherQueue.TryEnqueue(() => { if (!_unloaded) Bindings.Update(); });
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => Services.DebugLog.SafeRun("SettingsView.OnEngineChanged", () =>
        {
            if (_unloaded) return;
            if (e.PropertyName is nameof(EngineClient.Info)
                or nameof(EngineClient.State))
            {
                Services.DebugLog.Info($"[ENGINE-SUB:SettingsView] {e.PropertyName}");
                OnPropertyChanged(nameof(EngineVersionText));
                OnPropertyChanged(nameof(WorkerCapText));
                OnPropertyChanged(nameof(GpuSummaryText));
                OnPropertyChanged(nameof(ExecutionProviderText));
                OnPropertyChanged(nameof(RecommendationText));
                OnPropertyChanged(nameof(RecommendationVisibility));
                OnPropertyChanged(nameof(CpuTopologyText));
                OnPropertyChanged(nameof(MemoryDiagnosticsText));
                OnPropertyChanged(nameof(GpuDiagnosticsText));
                OnPropertyChanged(nameof(PowerDiagnosticsText));
                OnPropertyChanged(nameof(ThumbnailDiagnosticsText));
                DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncNvidiaSection(); });
            }
        });

    private void SyncNvidiaSection()
    {
        var hw = EngineClient.Instance.Info?.Hardware;
        var isNvidia = (hw?.GpuVendor ?? "").Equals("nvidia", StringComparison.OrdinalIgnoreCase);
        NvidiaAccelerationSection.Visibility = Visibility.Collapsed;

        if (isNvidia)
        {
            NvidiaAccelerationSection.Visibility = Visibility.Visible;
        }
    }

    private void HydrateToggles()
    {
        _initializingToggles = true;
        try
        {
            var s = AppViewModel.Instance.Settings;
            CleanupAutoTagToggle.IsOn = s.CleanupAutoTagKept;
            SelectRadio(LavaModeRadios, s.LavaMode);
            SelectRadio(TextSizeRadios, s.TextSize);
            SelectRadio(GranularityRadios, s.RestructureGranularity);
            string current = s.GpuExecutionProviderOverride ?? "auto";
            for (int i = 0; i < ProviderCombo.Items.Count; i++)
            {
                if (ProviderCombo.Items[i] is ComboBoxItem item && item.Tag is string tag && tag == current)
                {
                    ProviderCombo.SelectedIndex = i;
                    break;
                }
            }
        }
        finally
        {
            _initializingToggles = false;
        }
        SyncNvidiaSection();
    }

    private static void SelectRadio(RadioButtons radios, string tag)
    {
        for (int i = 0; i < radios.Items.Count; i++)
        {
            if (radios.Items[i] is RadioButton { Tag: string t } && t == tag)
            {
                radios.SelectedIndex = i;
                return;
            }
        }
    }

    private string? PickedRadio(RadioButtons radios)
        => _initializingToggles ? null : (radios.SelectedItem as RadioButton)?.Tag as string;

    private void OnLavaModeChanged(object sender, SelectionChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnLavaModeChanged), () =>
        {
            if (PickedRadio(LavaModeRadios) is not { } mode) return;
            var s = AppViewModel.Instance.Settings;
            s.LavaMode = mode;
            AppearanceApplier.Apply(s);
            s.Save();
        });

    private void OnTextSizeChanged(object sender, SelectionChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnTextSizeChanged), () =>
        {
            if (PickedRadio(TextSizeRadios) is not { } size) return;
            var s = AppViewModel.Instance.Settings;
            s.TextSize = size;
            AppearanceApplier.Apply(s);
            s.Save();
        });

    private void OnGranularityChanged(object sender, SelectionChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnGranularityChanged), () =>
        {
            if (PickedRadio(GranularityRadios) is not { } granularity) return;
            var s = AppViewModel.Instance.Settings;
            s.RestructureGranularity = granularity;
            s.Save();
        });

    private void OnCleanupAutoTagToggled(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnCleanupAutoTagToggled), () =>
        {
            if (_initializingToggles) return;
            var s = AppViewModel.Instance.Settings;
            s.CleanupAutoTagKept = CleanupAutoTagToggle.IsOn;
            s.Save();
        });
    private void OnProviderOverrideChanged(object sender, SelectionChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnProviderOverrideChanged), () =>
        {
            // SelectedIndex="0" raises SelectionChanged during InitializeComponent, before the saved value is seeded; persisting then would reset the override to "auto" on every open.
            if (!IsLoaded || _initializingToggles) return;
            if (ProviderCombo.SelectedItem is not ComboBoxItem item || item.Tag is not string tag) return;
            var s = AppViewModel.Instance.Settings;
            s.GpuExecutionProviderOverride = (tag == "auto") ? null : tag;
            s.Save();
        });

    public string EngineVersionText
    {
        get
        {
            // Info is null briefly after spawn even when State is Ready.
            var ec = EngineClient.Instance;
            var info = ec.Info;
            if (info is not null) return $"Engine v{info.Version} - PID {info.Pid}";
            return ec.State switch
            {
                EngineClient.LifecycleState.Ready => "Engine ready",
                EngineClient.LifecycleState.Starting => "Engine starting...",
                EngineClient.LifecycleState.Crashed => "Engine stopped (manual restart required)",
                _ => "Engine state unknown",
            };
        }
    }

    public string WorkerCapText
    {
        get
        {
            var info = EngineClient.Instance.Info;
            if (info is null) return "Worker pool: pending";
            var cores = info.Hardware?.PhysicalCpuCores;
            var coreText = cores.HasValue ? $" - {cores.Value} physical cores" : string.Empty;
            return $"Worker cap: {info.WorkerCap}{coreText} - {info.PhysicalMemoryGB:0.#} GB RAM";
        }
    }

    public string GpuSummaryText
    {
        get
        {
            var hw = EngineClient.Instance.Info?.Hardware;
            if (hw is null) return "GPU detection pending…";
            var vendor = hw.GpuVendor switch
            {
                "nvidia" => "NVIDIA",
                "amd" => "AMD",
                "intel" => "Intel",
                "qualcomm" => "Qualcomm Snapdragon",
                "none" => "No discrete GPU detected",
                _ => "Other / generic GPU",
            };
            return string.IsNullOrEmpty(hw.AdapterName)
                ? vendor
                : $"{vendor} · {hw.AdapterName}";
        }
    }

    public string ExecutionProviderText
    {
        get
        {
            var hw = EngineClient.Instance.Info?.Hardware;
            if (hw is null) return string.Empty;
            return hw.ExecutionProvider switch
            {
                "tract-cpu" => "CPU — tract, the same on Windows, macOS and Linux",
                _ => hw.ExecutionProvider,
            };
        }
    }

    public string RecommendationText =>
        EngineClient.Instance.Info?.Hardware?.Recommendation ?? string.Empty;

    public Visibility RecommendationVisibility =>
        string.IsNullOrEmpty(RecommendationText) ? Visibility.Collapsed : Visibility.Visible;

    /// <summary>P/E core split when hybrid, plus logical thread count and the worker cap in effect.</summary>
    public string CpuTopologyText
    {
        get
        {
            var hw = EngineClient.Instance.Info?.Hardware;
            if (hw is null) return "Detection pending…";
            string topo = (hw.PCores > 0 && hw.ECores > 0)
                ? $"{hw.PCores}P + {hw.ECores}E (hybrid)"
                : (hw.PCores > 0 ? $"{hw.PCores} cores" : $"{hw.PhysicalCpuCores} cores");
            string logical = hw.LogicalCpuCores > 0 ? $" · {hw.LogicalCpuCores} logical threads" : string.Empty;
            string worker = hw.WorkerCap > 0 ? $" · worker cap {hw.WorkerCap}" : string.Empty;
            return $"{topo}{logical}{worker}";
        }
    }

    public string MemoryDiagnosticsText
    {
        get
        {
            var hw = EngineClient.Instance.Info?.Hardware;
            if (hw is null) return "Detection pending…";
            if (hw.RamTotalMb == 0 && hw.RamAvailableMb == 0)
            {
                return "(this engine does not report utilization metrics)";
            }
            string tier = string.IsNullOrEmpty(hw.MemoryTier) ? "unknown" : hw.MemoryTier;
            return $"{hw.RamAvailableMb / 1024.0:0.#} GB available of {hw.RamTotalMb / 1024.0:0.#} GB · tier: {tier}";
        }
    }

    public string GpuDiagnosticsText
    {
        get
        {
            var hw = EngineClient.Instance.Info?.Hardware;
            if (hw is null) return "Detection pending…";
            string vramStr = hw.VramMb > 0 ? $" · {hw.VramMb / 1024.0:0.#} GB VRAM" : string.Empty;
            string npuStr = hw.NpuPresent ? " · NPU present" : string.Empty;
            return $"{GpuSummaryText}{vramStr}{npuStr}";
        }
    }

    public string PowerDiagnosticsText
    {
        get
        {
            var hw = EngineClient.Instance.Info?.Hardware;
            if (hw is null) return "Detection pending…";
            if (string.IsNullOrEmpty(hw.PowerSource))
            {
                return "(this engine does not report utilization metrics)";
            }
            string source = hw.PowerSource switch
            {
                "ac" => "AC power",
                "battery" => "Battery",
                _ => "Unknown",
            };
            string battery = hw.BatteryPercent.HasValue ? $" · {hw.BatteryPercent.Value}% charge" : string.Empty;
            string profile = string.IsNullOrEmpty(hw.ActiveProfile) ? string.Empty : $" · profile: {hw.ActiveProfile}";
            return $"{source}{battery}{profile}";
        }
    }

    public string ThumbnailDiagnosticsText
    {
        get
        {
            var s = Services.ThumbnailService.Stats;
            return $"ok={s.RenderedOk} failed={s.RenderedFailed} fallback={s.FallbackUsed} dropped={s.DroppedDispatcher}\n" +
                   $"disk: hits={s.DiskHits} writes={s.DiskWrites} sweeps={s.DiskSweeps} cached={FileID.Services.ByteFormat.File(s.DiskBytes)}";
        }
    }

    public string SceneTaggingDiagnosticsText =>
        "Tags: CLIP zero-shot (source='auto', scan-time, fast) + manual VLM refinement.";

    private void OnRefreshDiagnosticsClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnRefreshDiagnosticsClicked), () =>
        {
            OnPropertyChanged(nameof(CpuTopologyText));
            OnPropertyChanged(nameof(MemoryDiagnosticsText));
            OnPropertyChanged(nameof(GpuDiagnosticsText));
            OnPropertyChanged(nameof(PowerDiagnosticsText));
            OnPropertyChanged(nameof(SceneTaggingDiagnosticsText));
            OnPropertyChanged(nameof(ThumbnailDiagnosticsText));
        });

    // An incremental rescan skips up-to-date files, so re-tagging needs rescan=true on the current root.
    private async void OnForceRetagClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnForceRetagClicked), async () =>
        {
            var vm = FileID.ViewModels.AppViewModel.Instance;
            var root = vm.FolderPath;
            if (string.IsNullOrWhiteSpace(root))
            {
                DebugLog.Info("[RETAG] no library folder scanned yet; nothing to re-tag.");
                return;
            }
            DebugLog.Info("[RETAG] force re-scan (re-tag) requested for the current library root.");
            await FileID.ViewModels.EngineClient.Instance
                .StartScanAsync(root!, vm.FolderDisplay, rescan: true)
                .ConfigureAwait(true);
        });

    public string AppVersionText
    {
        get
        {
            var version = typeof(SettingsView).Assembly.GetName().Version?.ToString(3) ?? "0.0.0";
            return $"FileID for Windows · v{version}";
        }
    }

    public string DbPathText => AppPaths.DbPath;
    public string ThumbCachePathText => AppPaths.ThumbsDir;
    public string ModelsPathText => AppPaths.ModelsDir;

    private void OnOpenLogsClicked(object sender, RoutedEventArgs e) => RevealInExplorer(AppPaths.LogsDir);
    private void OnShowDbFolderClicked(object sender, RoutedEventArgs e)
        => RevealInExplorer(System.IO.Path.GetDirectoryName(DbPathText) ?? AppPaths.LogsDir);
    private void OnShowThumbCacheClicked(object sender, RoutedEventArgs e) => RevealInExplorer(ThumbCachePathText);
    private void OnShowModelsFolderClicked(object sender, RoutedEventArgs e) => RevealInExplorer(ModelsPathText);

    private void OnOpenPrivacyDocClicked(object sender, RoutedEventArgs e)
    {
        // Prefer the shipped document, with the repository copy available in dev builds.
        var candidates = new[]
        {
            System.IO.Path.Combine(AppContext.BaseDirectory, "Docs", "PRIVACY.md"),
            System.IO.Path.Combine(AppContext.BaseDirectory, "..", "..", "..", "..", "..", "..", "shared", "docs", "PRIVACY.md"),
        };
        foreach (var c in candidates)
        {
            try
            {
                var full = System.IO.Path.GetFullPath(c);
                if (System.IO.File.Exists(full))
                {
                    Process.Start(new ProcessStartInfo { FileName = full, UseShellExecute = true });
                    return;
                }
            }
            catch { /* try next */ }
        }
        DebugLog.Warn("The local privacy document is unavailable.");
    }

    private static void RevealInExplorer(string path)
    {
        try
        {
            if (!System.IO.Directory.Exists(path))
            {
                try { System.IO.Directory.CreateDirectory(path); } catch { /* fall through */ }
            }
            Process.Start(new ProcessStartInfo
            {
                FileName = "explorer.exe",
                Arguments = $"\"{path}\"",
                UseShellExecute = true,
            });
        }
        catch
        {
            // The path is shown next to the button, so a failed reveal is harmless.
        }
    }

    // PrewarmAsync flips Status to Downloading only after WaitForReadyAsync, so a rapid double-click would otherwise send a duplicate prewarm. UI-thread-confined.
    private readonly System.Collections.Generic.HashSet<Services.ModelSlot> _installInFlight = new();

    private void OnInstallModelClicked(object sender, RoutedEventArgs e)
    {
        if (sender is not Button button || button.Tag is not string modelKind || string.IsNullOrWhiteSpace(modelKind))
            return;
        var slot = modelKind switch
        {
            "arcface_buffalo" => Svc.Arcface,
            "mobileclip_s2" => Svc.Clip,
            "ram_plus" => Svc.RamPlus,
            "face_mesh" => Svc.FaceMesh,
            _ => null,
        };
        if (slot is null) return;
        // Cancel this row's in-flight model (the engine now supports per-model cancel).
        if (slot.Status == Services.ModelInstallStatus.Downloading)
        {
            // Engine confirmation takes 1-5 s, so show "Cancelling…" immediately.
            slot.Message = "Cancelling…";
            slot.BytesPerSecond = 0;
            slot.EtaSeconds = 0;
            _ = CancelSlotAsync(slot);
            return;
        }
        if (!_installInFlight.Add(slot))
        {
            DebugLog.Info($"[SETTINGS] {slot.DisplayLabel} install already in flight; ignoring duplicate click.");
            return;
        }
        _ = SafeRunAsync(async () =>
        {
            try { await slot.InstallAsync().ConfigureAwait(true); }
            finally { _installInFlight.Remove(slot); }
        }, "Install " + slot.DisplayLabel);
    }

    // If the engine is dead the cancel never lands, so the optimistic "Cancelling…" caption must be reverted and the user told.
    private async Task CancelSlotAsync(Services.ModelSlot slot)
    {
        try
        {
            await Svc.CancelModelAsync(slot.CurrentModelKind).ConfigureAwait(true);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[SETTINGS] Cancel {slot.DisplayLabel} threw: {ex}");
            if (_unloaded) return;
            DispatcherQueue.TryEnqueue(() =>
            {
                if (slot.Status == Services.ModelInstallStatus.Downloading
                    && slot.Message == "Cancelling…")
                {
                    slot.Message = null;
                }
            });
            await ShowAlertAsync(
                "Couldn't cancel the download",
                $"FileID couldn't reach the engine to cancel {slot.DisplayLabel}. "
                + "The download may still be running.\n\n"
                + "Try again in a moment. If it persists, restart FileID from the Engine card in Settings.\n\n"
                + "Details: " + ex.Message).ConfigureAwait(true);
        }
    }

    private static async Task SafeRunAsync(Func<Task> action, string label)
    {
        try
        {
            await action().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"[SETTINGS] {label} threw: {ex}");
        }
    }

    private async Task ShowAlertAsync(string title, string body)
    {
        // ShowAsync throws on a broken XamlRoot (mid-shutdown, tab re-host); a failed alert must not reach App.UnhandledException.
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

    internal Visibility VisibleIfDownloading(Services.ModelInstallStatus s) =>
        s == Services.ModelInstallStatus.Downloading ? Visibility.Visible : Visibility.Collapsed;

    internal Visibility VisibleIfInstalled(Services.ModelInstallStatus s) =>
        s == Services.ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;

    internal Visibility VisibleIfFailed(Services.ModelInstallStatus s) =>
        s == Services.ModelInstallStatus.Failed ? Visibility.Visible : Visibility.Collapsed;

    // Indeterminate until the first byte. Gated on the sticky HasStarted rather than fraction<=0 so a multi-pack fraction rewind doesn't flap the bar back to its marquee.
    internal bool IsStarting(Services.ModelInstallStatus s, bool hasStarted) =>
        s == Services.ModelInstallStatus.Downloading && !hasStarted;

    internal Visibility ShowActionButton(Services.ModelInstallStatus s) =>
        s != Services.ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;

    internal string ButtonLabel(Services.ModelInstallStatus s) => s switch
    {
        Services.ModelInstallStatus.Downloading => "Cancel",
        Services.ModelInstallStatus.Failed => "Retry",
        _ => "Download",
    };

    // Sticky once started so one transient stall sample doesn't flap the row; RateEtaLabel renders empty while stalled.
    internal Visibility ShowRateEta(Services.ModelInstallStatus status, bool hasStarted) =>
        status == Services.ModelInstallStatus.Downloading && hasStarted
            ? Visibility.Visible : Visibility.Collapsed;

    internal string ProgressLabel(string? message, double fraction, ulong? bytesDone, ulong? totalBytes)
    {
        // At 0% prefer the engine's caption (e.g. "Queued — starting download…") over "Starting…".
        string pct;
        if (fraction > 0) pct = $"{fraction * 100:0}%";
        else if (!string.IsNullOrEmpty(message)) pct = message;
        else pct = "Starting…";
        var bytes = string.Empty;
        if (bytesDone is { } done && totalBytes is { } total && total > 0)
        {
            bytes = $" · {FormatBytes(done)} of {FormatBytes(total)}";
        }
        else if (totalBytes is { } total2)
        {
            bytes = $" · of {FormatBytes(total2)}";
        }
        return pct + bytes;
    }

    internal string RateEtaLabel(double bytesPerSecond, double etaSeconds)
    {
        if (bytesPerSecond <= 0) return string.Empty;
        var rate = $"{FormatBytes((ulong)bytesPerSecond)}/s";
        var eta = etaSeconds > 0 ? " · " + FormatEta(etaSeconds) + " remaining" : string.Empty;
        return rate + eta;
    }

    internal string ErrorLabel(string? lastError) =>
        "Failed: " + (lastError ?? "unknown error");

    private static string FormatBytes(ulong b) => FileID.Services.ByteFormat.File(b);

    private static string FormatEta(double seconds)
    {
        if (seconds < 60) return $"{seconds:0}s";
        if (seconds < 3600) return $"{seconds / 60:0}m {seconds % 60:00}s";
        return $"{seconds / 3600:0}h {(seconds % 3600) / 60:00}m";
    }
    private void OnInstallCudaLlamaClicked(object sender, RoutedEventArgs e)
    {
        var pack = Path.Combine(AppPaths.ModelsDir, "llama.cpp-cuda");
        CudaLlamaStatusText.Text = $"Place a compatible CUDA llama.cpp runtime in {pack}, then restart the engine. FileID does not download vendor runtimes.";
    }

    private async void OnRestartEngineCardClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            RestartEngineCardButton.IsEnabled = false;
            StopEngineCardButton.IsEnabled = false;
            RestartEngineCardButton.Content = "Restarting…";
            await EngineClient.Instance.RestartAsync();
            DebugLog.Info("Engine restart requested by user from the Engine card.");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Engine restart (card) failed: " + ex.Message);
        }
        finally
        {
            RestartEngineCardButton.Content = "Restart engine";
            RestartEngineCardButton.IsEnabled = true;
            StopEngineCardButton.IsEnabled = true;
        }
    }

    private async void OnStopEngineCardClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            StopEngineCardButton.IsEnabled = false;
            StopEngineCardButton.Content = "Stopping…";
            await EngineClient.Instance.ShutdownAsync();
            DebugLog.Info("Engine shutdown requested by user from the Engine card.");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Engine shutdown (card) failed: " + ex.Message);
        }
        finally
        {
            StopEngineCardButton.Content = "Stop engine";
            StopEngineCardButton.IsEnabled = true;
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
