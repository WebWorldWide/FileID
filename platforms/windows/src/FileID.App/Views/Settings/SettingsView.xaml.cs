// SettingsView code-behind.

using System;
using System.ComponentModel;
using System.Diagnostics;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Settings;

public sealed partial class SettingsView : UserControl, INotifyPropertyChanged
{
    private bool _unloaded;
    private bool _initializingToggles;

    /// <summary> expose the singleton ModelInstallerService so
    /// the Settings model cards can x:Bind to Svc.Arcface / Svc.Clip the same
    /// way WelcomeSheet does. Without this binding path the cards stayed
    /// stale after Welcome installed a model — the imperative TextBlock
    /// mutation only fired when the user clicked the in-card Install button.
    /// </summary>
    internal Services.ModelInstallerService Svc => Services.ModelInstallerService.Instance;

    public SettingsView()
    {
        InitializeComponent();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Svc.PropertyChanged += OnInstallerChanged;
        Svc.Clip.PropertyChanged += OnSlotChanged;
        Svc.Arcface.PropertyChanged += OnSlotChanged;
        Svc.RamPlus.PropertyChanged += OnSlotChanged;
        Svc.DeepVlm.PropertyChanged += OnSlotChanged;
        Unloaded += (_, _) =>
        {
            _unloaded = true;
            EngineClient.Instance.PropertyChanged -= OnEngineChanged;
            Svc.PropertyChanged -= OnInstallerChanged;
            Svc.Clip.PropertyChanged -= OnSlotChanged;
            Svc.Arcface.PropertyChanged -= OnSlotChanged;
            Svc.RamPlus.PropertyChanged -= OnSlotChanged;
            Svc.DeepVlm.PropertyChanged -= OnSlotChanged;
        };
        Loaded += (_, _) =>
        {
            HydrateToggles();
            // Re-seed from on-disk sentinels in case Welcome / DeepAnalyze
            // installed a model while a different tab was active. The call
            // happens on the UI thread (Loaded handler), so any
            // PropertyChanged events from slot.Status flips land on the
            // dispatcher the bindings live on. Belt-and-suspenders: a
            // direct Bindings.Update() below forces re-evaluation even
            // if a PropertyChanged event was dropped (singleton first-
            // touched off the UI thread, etc.).
            try { Svc.Refresh(); } catch { }
            // sync the CUDA llama.cpp + cuDNN install buttons to
            // reflect already-installed state at page load. Before this
            // the buttons always showed "Install" and the user had to
            // click them just to see the state flip (matching engine's
            // immediate sentinel-check short-circuit).
            try { SyncInstallButtonStates(); } catch { }
            // Force a bindings refresh after sentinel re-seed. Without
            // this, the ArcFace / MobileCLIP install buttons can stay on
            // "Install" at page load even when the sentinels exist on
            // disk — Set()'s equality short-circuit suppresses the
            // PropertyChanged event when Refresh()'s SeedSlot writes a
            // status equal to the cached field. NEXT.md tracked this
            // as the "install-state detection at page load" bug.
            try { DispatcherQueue.TryEnqueue(() => Bindings.Update()); } catch { }
            // Populate the Recent Scans card. Query is cheap (≤5 rows)
            // so we do it inline on the dispatcher.
            try { _ = PopulateRecentScansAsync(); } catch { }
        };
    }

    // Reads up to the 5 most-recent scan_sessions rows and renders one
    // line per row in the Recent Scans card.
    private async System.Threading.Tasks.Task PopulateRecentScansAsync()
    {
        var rows = await System.Threading.Tasks.Task.Run(() =>
        {
            var list = new System.Collections.Generic.List<(double started, double? completed, long total, string root, string? status)>();
            try
            {
                if (!System.IO.File.Exists(AppPaths.DbPath)) return list;
                var conn = new Microsoft.Data.Sqlite.SqliteConnection(
                    new Microsoft.Data.Sqlite.SqliteConnectionStringBuilder
                    {
                        DataSource = AppPaths.DbPath,
                        Mode = Microsoft.Data.Sqlite.SqliteOpenMode.ReadOnly,
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
        // Defensive: view may have unloaded during the async DB read; the
        // XAML element references could be disposed. Wrap in try/catch
        // to keep an in-flight continuation from fast-failing the dispatcher.
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
                var rootShort = string.IsNullOrEmpty(root)
                    ? "(unknown root)"
                    : System.IO.Path.GetFileName(root.TrimEnd('\\', '/'));
                items.Add($"• {when}  ·  {total:N0} files  ·  {dur}  ·  {rootShort}");
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

    /// <summary>probe the engine's install sentinels and flip the
    /// NVIDIA-acceleration card buttons to "Installed" + disabled if the
    /// runtime is already present on disk. Same sentinel path the engine
    /// writes after a successful prewarm (atomic temp+rename) — file
    /// existence is sufficient. Matches <see cref="Services.ModelInstallerService"/>'s
    /// SentinelInstalled probe.
    ///
    /// Also covers the ArcFace + MobileCLIP slots that the model-card x:Binds
    /// observe via Svc.Arcface.Status / Svc.Clip.Status. SeedFromSentinels
    /// already keeps those slots in sync, but if the singleton was first
    /// touched off the UI thread or PropertyChanged was suppressed by Set()'s
    /// equality short-circuit, the bindings stay stale. Directly forcing
    /// the slot Status here (on the UI dispatcher) wakes those bindings up.
    /// </summary>
    private void SyncInstallButtonStates()
    {
        if (SentinelExists("llama_runtime_cuda_x64"))
        {
            InstallCudaLlamaButton.Content = "Installed";
            InstallCudaLlamaButton.IsEnabled = false;
            CudaLlamaStatusText.Text = "✓ CUDA llama.cpp installed. Deep Analyze will use it on next run.";
        }
        if (SentinelExists("cudnn_runtime_x64"))
        {
            InstallCudnnButton.Content = "Installed";
            InstallCudnnButton.IsEnabled = false;
            // Leave CudnnStatusText alone — SyncNvidiaSection owns it and
            // already reflects whether the CUDA EP is actually active in
            // the current engine session (which depends on engine restart,
            // not just sentinel presence).
        }
        // ArcFace requires the "arcface" sentinel (the engine bundles
        // SCRFD + ArcFace as one install in registry.rs).
        if (SentinelExists("arcface") && Svc.Arcface.Status != Services.ModelInstallStatus.Installed)
        {
            DispatcherQueue.TryEnqueue(() =>
            {
                Svc.Arcface.Status = Services.ModelInstallStatus.Installed;
                Svc.Arcface.Fraction = 1.0;
            });
        }
        // CLIP requires BOTH halves on disk (image encoder + text encoder).
        if (SentinelExists("mobileclip_s2") && SentinelExists("clip_text")
            && Svc.Clip.Status != Services.ModelInstallStatus.Installed)
        {
            DispatcherQueue.TryEnqueue(() =>
            {
                Svc.Clip.Status = Services.ModelInstallStatus.Installed;
                Svc.Clip.Fraction = 1.0;
            });
        }
        // Deep Analyze VLM has 3 alternative weights — any one sentinel marks it installed.
        if ((SentinelExists("qwen2_5_vl_7b") || SentinelExists("gemma_3_4b")
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

    private static bool SentinelExists(string modelId)
    {
        try
        {
            return System.IO.File.Exists(System.IO.Path.Combine(
                AppPaths.ModelsDir, ".sentinels", $"{modelId}.installed"));
        }
        catch
        {
            return false;
        }
    }

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
                Services.DebugLog.Debug($"[ENGINE-SUB:SettingsView] {e.PropertyName}");
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
            else if (e.PropertyName == nameof(EngineClient.LastHardwareReprobe))
            {
                Services.DebugLog.Debug($"[ENGINE-SUB:SettingsView] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(() => { if (!_unloaded) SyncReprobeUi(); });
            }
        });

    /// <summary> toggle the NVIDIA acceleration card based on
    /// the detected GPU vendor. The card surfaces two affordances: install
    /// the CUDA-flavored llama.cpp runtime (no cuDNN required) and direct
    /// the user to NVIDIA's cuDNN download for the CUDA ORT EP.</summary>
    private void SyncNvidiaSection()
    {
        var hw = EngineClient.Instance.Info?.Hardware;
        var isNvidia = (hw?.GpuVendor ?? "").Equals("nvidia", StringComparison.OrdinalIgnoreCase);
        NvidiaAccelerationSection.Visibility = isNvidia ? Visibility.Visible : Visibility.Collapsed;

        if (isNvidia)
        {
            var ep = (hw?.ExecutionProvider ?? "").ToLowerInvariant();
            CudnnStatusText.Text = ep == "cuda"
                ? "✓ CUDA execution provider is active. Scanning uses cuDNN."
                : "Install NVIDIA cuDNN to unlock the CUDA execution provider for scanning (10-15% faster than DirectML on RTX-class GPUs). FileID can't redistribute cuDNN — get it from NVIDIA's developer site.";
        }
    }

    private void HydrateToggles()
    {
        _initializingToggles = true;
        try
        {
            var s = AppViewModel.Instance.Settings;
            CleanupAutoTagToggle.IsOn = s.CleanupAutoTagKept;
            // Inverted: AppSettings stores "Disable…" but the UI shows
            // "Auto-install on" (truthy = enabled).
            AutoInstallCudaToggle.IsOn = !s.DisableAutoInstallCuda;

            // Hydrate the EP override picker too.
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

    private void OnCleanupAutoTagToggled(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnCleanupAutoTagToggled), () =>
        {
            if (_initializingToggles) return;
            var s = AppViewModel.Instance.Settings;
            s.CleanupAutoTagKept = CleanupAutoTagToggle.IsOn;
            s.Save();
        });

    private void OnAutoInstallCudaToggled(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnAutoInstallCudaToggled), () =>
        {
            if (_initializingToggles) return;
            var s = AppViewModel.Instance.Settings;
            s.DisableAutoInstallCuda = !AutoInstallCudaToggle.IsOn;
            s.Save();
        });

    private void OnProviderOverrideChanged(object sender, SelectionChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnProviderOverrideChanged), () =>
        {
            // The ComboBox's SelectedIndex="0" raises SelectionChanged during
            // InitializeComponent — before HydrateToggles seeds the saved value
            // and before _initializingToggles is set. Persisting here would
            // clobber the user's GPU EP override to "auto" on every Settings
            // open. Bail until the view is live; HydrateToggles re-selects then.
            if (!IsLoaded || _initializingToggles) return;
            if (ProviderCombo.SelectedItem is not ComboBoxItem item || item.Tag is not string tag) return;
            var s = AppViewModel.Instance.Settings;
            s.GpuExecutionProviderOverride = (tag == "auto") ? null : tag;
            s.Save();
            // The engine reads this value at startup via runtime.rs's
            // read_user_ep_override(); to apply a change live, the user
            // must restart the engine (the Restart button in this view, or
            // the prompt shown after installing a Performance Pack).
        });

    public string EngineVersionText
    {
        get
        {
            // Info can be null even when State==Ready in the brief
            // window between spawn + first ready event. Surface State so
            // the Settings card stops claiming "Engine starting..." for
            // a Ready engine.
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
                "cuda" => "CUDA — NVIDIA-tuned (highest perf on RTX class)",
                "tensorrt" => "TensorRT — NVIDIA optimized graph compilation",
                "directml" => "DirectML — works on every Windows GPU vendor",
                "openvino" => "OpenVINO — Intel-tuned",
                "qnn" => "QNN — Snapdragon Hexagon NPU (most power-efficient on WoA)",
                "cpu" => "CPU — AVX2 / NEON SIMD",
                _ => hw.ExecutionProvider,
            };
        }
    }

    public string RecommendationText =>
        EngineClient.Instance.Info?.Hardware?.Recommendation ?? string.Empty;

    public Visibility RecommendationVisibility =>
        string.IsNullOrEmpty(RecommendationText) ? Visibility.Collapsed : Visibility.Visible;

    /// <summary>P/E core split when hybrid, plus logical thread count
    /// and the worker cap currently in effect. Falls back to physical
    /// cores when running against an older engine that didn't populate
    /// the new fields.</summary>
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
                return "(engine older than V15.9 — metrics not populated)";
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
                return "(engine older than V15.9 — metrics not populated)";
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
            double diskMb = s.DiskBytes / (1024.0 * 1024.0);
            return $"ok={s.RenderedOk} failed={s.RenderedFailed} fallback={s.FallbackUsed} dropped={s.DroppedDispatcher}\n" +
                   $"disk: hits={s.DiskHits} writes={s.DiskWrites} sweeps={s.DiskSweeps} cached={diskMb:0.#} MB";
        }
    }

    // Scan-time scene tagging uses CLIP (source='auto') and is enabled by default.
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

    // Force re-tag: re-scan the current library root with rescan=true so the
    // engine recomputes tags for files it already has (incremental rescan
    // skips up-to-date files). Without this, a tagging change isn't visible
    // unless the user deletes fileid.sqlite. Root comes from the same place
    // the normal scan path uses (AppViewModel.FolderPath).
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

    // ─── Storage card paths ───────────────────────────────────────────
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
        // Try the shipped docs path first, fall back to the repo source
        // path if the user is running from a dev tree, then surface the
        // hosted GitHub URL via the system browser as a last resort.
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
        try
        {
            Process.Start(new ProcessStartInfo
            {
                FileName = "https://github.com/anolle/FileID/blob/main/shared/docs/PRIVACY.md",
                UseShellExecute = true,
            });
        }
        catch { /* nothing else to try */ }
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
            // Settings is low-stakes — silently ignore. The path is already
            // shown next to the button so the user can copy it manually.
        }
    }

    /// <summary> route the Settings install button through the
    /// shared ModelInstallerService slot, same as WelcomeSheet. Single source
    /// of truth for status; the x:Bind paths on the card update automatically
    /// via the slot's PropertyChanged. No more local OnProgress subscription
    /// (which couldn't pick up state changes that originated in Welcome).</summary>
    private void OnInstallModelClicked(object sender, RoutedEventArgs e)
    {
        if (sender is not Button button || button.Tag is not string modelKind || string.IsNullOrWhiteSpace(modelKind))
            return;
        var slot = modelKind switch
        {
            "arcface_buffalo" => Svc.Arcface,
            "mobileclip_s2" => Svc.Clip,
            "ram_plus" => Svc.RamPlus,
            _ => null,
        };
        if (slot is null) return;
        // Cancel = re-trigger CancelAllAsync (engine has no per-model cancel).
        if (slot.Status == Services.ModelInstallStatus.Downloading)
        {
            // Pre-flip caption to "Cancelling…" so the user gets instant
            // feedback — engine confirmation takes 1-5 s. Mirrors macOS
            // WelcomeSheet.swift:74-82's pre-emptive reset.
            slot.Message = "Cancelling…";
            slot.BytesPerSecond = 0;
            slot.EtaSeconds = 0;
            _ = SafeRunAsync(() => Svc.CancelAllAsync(), "Cancel " + slot.DisplayLabel);
            return;
        }
        _ = SafeRunAsync(() => slot.InstallAsync(), "Install " + slot.DisplayLabel);
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

    // ─── x:Bind helper functions (copied verbatim from WelcomeSheet) ───

    internal Visibility VisibleIfDownloading(Services.ModelInstallStatus s) =>
        s == Services.ModelInstallStatus.Downloading ? Visibility.Visible : Visibility.Collapsed;

    internal Visibility VisibleIfInstalled(Services.ModelInstallStatus s) =>
        s == Services.ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;

    internal Visibility VisibleIfFailed(Services.ModelInstallStatus s) =>
        s == Services.ModelInstallStatus.Failed ? Visibility.Visible : Visibility.Collapsed;

    // Single ProgressBar per model card: indeterminate until the first byte,
    // then determinate. Replaces the old ProgressBar↔ProgressRing Visibility
    // swap that flickered each time Fraction crossed 0.
    internal bool IsStarting(Services.ModelInstallStatus s, double frac) =>
        s == Services.ModelInstallStatus.Downloading && frac <= 0;

    internal Visibility ShowActionButton(Services.ModelInstallStatus s) =>
        s != Services.ModelInstallStatus.Installed ? Visibility.Visible : Visibility.Collapsed;

    internal string ButtonLabel(Services.ModelInstallStatus s) => s switch
    {
        Services.ModelInstallStatus.Downloading => "Cancel",
        Services.ModelInstallStatus.Failed => "Retry",
        _ => "Install",
    };

    internal Visibility ShowRateEta(Services.ModelInstallStatus status, double bytesPerSecond) =>
        status == Services.ModelInstallStatus.Downloading && bytesPerSecond > 0
            ? Visibility.Visible : Visibility.Collapsed;

    internal string ProgressLabel(string? message, double fraction, ulong? bytesDone, ulong? totalBytes)
    {
        // Prefer the engine's caption (e.g. "Queued — starting download…") while
        // we're still at 0%, so the row doesn't read "Starting…" forever after
        // the engine has acknowledged the prewarm.
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

    private static string FormatBytes(ulong b)
    {
        const double KB = 1024.0;
        const double MB = 1024.0 * 1024.0;
        const double GB = 1024.0 * 1024.0 * 1024.0;
        if (b >= GB) return $"{b / GB:0.00} GB";
        if (b >= MB) return $"{b / MB:0.0} MB";
        if (b >= KB) return $"{b / KB:0} KB";
        return $"{b} B";
    }

    private static string FormatEta(double seconds)
    {
        if (seconds < 60) return $"{seconds:0}s";
        if (seconds < 3600) return $"{seconds / 60:0}m {seconds % 60:00}s";
        return $"{seconds / 3600:0}h {(seconds % 3600) / 60:00}m";
    }

    /// <summary>F3c install button: kicks off llama_runtime_cuda_x64
    /// prewarm. Engine downloads, extracts, then VlmRunner::find prefers
    /// the CUDA build automatically on the next Deep Analyze.</summary>
    private async void OnInstallCudaLlamaClicked(object sender, RoutedEventArgs e)
    {
        if (sender is not Button button) return;
        var originalContent = button.Content;
        try
        {
            button.IsEnabled = false;
            button.Content = "Installing…";
            CudaLlamaStatusText.Text = "Downloading CUDA llama.cpp build (~200 MB)…";
            await EngineClient.Instance.PrewarmModelAsync("llama_runtime_cuda_x64");
            button.Content = "Installed";
            CudaLlamaStatusText.Text = "✓ CUDA llama.cpp installed. Deep Analyze will use it on next run.";
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Install CUDA llama.cpp failed: {ex.Message}");
            button.Content = originalContent;
            button.IsEnabled = true;
            CudaLlamaStatusText.Text = $"Install failed: {ex.Message}";
        }
    }

    /// <summary>In-app cuDNN install. PrewarmModelAsync asks the engine to
    /// fetch the cuDNN redist (~430 MB) from NVIDIA's CDN, extract it
    /// under Models/cudnn/, and call register_dll_dirs_under so the ORT
    /// CUDA EP can load it on next engine restart. After completion the
    /// user hits "Verify install" (or restarts the engine) to flip the
    /// scanning pipeline from DirectML to CUDA EP.</summary>
    private async void OnInstallCudnnClicked(object sender, RoutedEventArgs e)
    {
        if (sender is not Button button) return;
        var originalContent = button.Content;
        try
        {
            button.IsEnabled = false;
            button.Content = "Installing…";
            CudnnStatusText.Text = "Downloading cuDNN (~430 MB) from NVIDIA's CDN…";
            await EngineClient.Instance.PrewarmModelAsync("cudnn_runtime_x64");
            button.Content = "Installed";
            CudnnStatusText.Text = "✓ cuDNN installed. Click \"Verify install\" or restart FileID to switch scanning to the CUDA EP.";
        }
        catch (Exception ex)
        {
            DebugLog.Warn($"Install cuDNN failed: {ex.Message}");
            button.Content = originalContent;
            button.IsEnabled = true;
            CudnnStatusText.Text = $"Install failed: {ex.Message}";
        }
    }

    /// <summary>F3c "Get cuDNN" link: opens NVIDIA's official cuDNN download
    /// page in the user's default browser. No FileID-owned redistribution.
    /// After install, the engine's system-CUDA probe (runtime.rs) picks up
    /// cuDNN on next launch and the CUDA EP becomes available.</summary>
    private void OnOpenCudnnDownloadsClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            var psi = new ProcessStartInfo
            {
                FileName = "https://developer.nvidia.com/cudnn-downloads",
                UseShellExecute = true,
            };
            Process.Start(psi);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Open cuDNN downloads failed: " + ex.Message);
        }
    }

    /// <summary>ask the engine to re-probe cuDNN availability after
    /// the user manually installs it. The engine replies with a
    /// HardwareReprobed event that lands on
    /// <see cref="EngineClient.LastHardwareReprobe"/>; <see cref="SyncReprobeUi"/>
    /// then renders the success pill or the diagnostics caption.</summary>
    private async void OnVerifyCudnnClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            VerifyCudnnButton.IsEnabled = false;
            VerifyCudnnButton.Content = "Verifying…";
            await EngineClient.Instance.VerifyCudaPackAsync();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Verify cuDNN failed: " + ex.Message);
            CudnnDiagnosticsText.Text = "Couldn't re-probe (engine not ready): " + ex.Message;
            CudnnDiagnosticsText.Visibility = Visibility.Visible;
        }
        finally
        {
            // Restore label even before the event arrives; the SyncReprobeUi
            // hook will update the rest of the row when the engine replies.
            VerifyCudnnButton.IsEnabled = true;
            VerifyCudnnButton.Content = "Verify install";
        }
    }

    /// <summary>render the success pill / diagnostics caption after
    /// the engine emits a HardwareReprobed event. Called from the engine
    /// PropertyChanged hook on LastHardwareReprobe.</summary>
    private void SyncReprobeUi()
    {
        var reprobe = EngineClient.Instance.LastHardwareReprobe;
        if (reprobe is null) return;
        var present = reprobe.Hardware.CudaPackPresent;
        var activeEp = (reprobe.Hardware.ExecutionProvider ?? "").ToLowerInvariant();

        if (present)
        {
            CudnnDiagnosticsText.Visibility = Visibility.Collapsed;
            CudnnSuccessPill.Visibility = Visibility.Visible;
            if (activeEp == "cuda")
            {
                // Already on CUDA in this engine session — no restart needed.
                CudnnSuccessTitle.Text = "✓ cuDNN active — scanning uses CUDA EP.";
                CudnnSuccessDetail.Text = $"Adapter: {reprobe.Hardware.AdapterName ?? "unknown"}. Execution provider: CUDA.";
                RestartEngineButton.Visibility = Visibility.Collapsed;
            }
            else
            {
                // cuDNN reachable but this session loaded DirectML at startup —
                // need a restart to pick CUDA next spawn.
                CudnnSuccessTitle.Text = "✓ cuDNN detected — restart engine to switch to CUDA";
                CudnnSuccessDetail.Text = $"Current session: {activeEp}. Restart so the engine re-picks the execution provider.";
                RestartEngineButton.Visibility = Visibility.Visible;
            }
        }
        else
        {
            CudnnSuccessPill.Visibility = Visibility.Collapsed;
            var diag = reprobe.Diagnostics;
            if (!string.IsNullOrWhiteSpace(diag))
            {
                CudnnDiagnosticsText.Text = diag!;
                CudnnDiagnosticsText.Visibility = Visibility.Visible;
            }
            else
            {
                CudnnDiagnosticsText.Visibility = Visibility.Collapsed;
            }
        }
    }

    /// <summary>shutdown + auto-respawn cycle so the engine re-runs
    /// the EP picker. EngineClient's existing crash-respawn path picks the
    /// new EP on the next spawn.</summary>
    private async void OnRestartEngineClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            RestartEngineButton.IsEnabled = false;
            RestartEngineButton.Content = "Restarting…";
            await EngineClient.Instance.RestartAsync();
            // After RestartAsync returns, Ready has been emitted; SyncReprobeUi
            // will refresh on the next PropertyChanged via Info update.
            DebugLog.Info("Engine restart requested by user after cuDNN install.");
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Engine restart failed: " + ex.Message);
        }
        finally
        {
            RestartEngineButton.IsEnabled = true;
            RestartEngineButton.Content = "Restart engine";
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
