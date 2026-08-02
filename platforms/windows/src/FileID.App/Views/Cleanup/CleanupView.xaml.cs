// CleanupView code-behind. Trash-non-keepers walks every group, gathers
// the file_ids for members where IsKeeper == false, confirms with the
// user, then sends one big trashFiles IPC.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using System.Threading;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Cleanup;

public sealed partial class CleanupView : UserControl, INotifyPropertyChanged
{
    internal CleanupViewModel ViewModel { get; }

    private bool _unloaded;
    // Live duplicate-group streaming during a scan. Mirrors macOS
    // CleanupView's .onChange(of: engine.lastBatch?.batchIndex) — refresh
    // the group list whenever a new BatchSummary lands, throttled at 1s
    // so a fast scan doesn't issue 30+ DB reads per second.
    private long _lastSeenBatchIndex = -1;
    private DateTime _lastReloadAt = DateTime.MinValue;
    private static readonly TimeSpan CleanupReloadThrottle = TimeSpan.FromSeconds(1);
    // Per-tile shell thumbnails (macOS CopyTile parity) — loaded lazily via the
    // members repeater's ElementPrepared, cancelled on recycle, like LibraryView.
    private readonly ThumbnailService _thumbnails = new();
    private readonly System.Collections.Concurrent.ConcurrentDictionary<DuplicateMember, CancellationTokenSource> _inflightThumbs = new();

    public CleanupView()
    {
        ViewModel = new CleanupViewModel(AppPaths.DbPath, Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread());
        InitializeComponent();
        // Named handlers (not inline lambdas) so OnUnloaded can detach
        // them. Inline lambdas leak the view + VM graph every tab swap
        // and can fire after the view is detached, touching disposed
        // XAML — a known cause of the "click sidebar mid-scan → app crash"
        // symptom.
        ViewModel.PropertyChanged += OnViewModelPropertyChanged;
        ViewModel.Groups.CollectionChanged += OnGroupsCollectionChanged;
        ViewModels.EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Loaded += OnLoadedAsync;
        Unloaded += OnUnloaded;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => Services.DebugLog.SafeRun("CleanupView.OnEngineChanged", () =>
        {
            if (_unloaded) return;
            switch (e.PropertyName)
            {
                case nameof(ViewModels.EngineClient.Phase):
                    if (ViewModels.EngineClient.Instance.Phase == FileID.IpcSchema.ScanPhase.Completed)
                    {
                        Services.DebugLog.Debug($"[ENGINE-SUB:CleanupView] {e.PropertyName}=Completed");
                        RequestCleanupRefresh();
                    }
                    break;
                case nameof(ViewModels.EngineClient.LastBatch):
                    var summary = ViewModels.EngineClient.Instance.LastBatch;
                    if (summary is null) return;
                    long batchIndex = summary.BatchIndex;
                    if (batchIndex == _lastSeenBatchIndex) return;
                    _lastSeenBatchIndex = batchIndex;
                    if (DateTime.UtcNow - _lastReloadAt < CleanupReloadThrottle) return;
                    Services.DebugLog.Debug($"[ENGINE-SUB:CleanupView] {e.PropertyName} batch={batchIndex}");
                    RequestCleanupRefresh();
                    break;
            }
        });

    // debounce refresh requests. A fast scan emits dozens of
    // BatchSummary events per second; the time throttle above limits us
    // to one refresh per second, but rapid Phase transitions or a tab
    // re-enter while the throttle window is hot can still enqueue
    // multiple RefreshAsync tasks before any of them complete. The flag
    // ensures only one refresh is ever pending at a time.
    private int _refreshPending; // 0 = idle, 1 = refresh queued

    // Re-entrancy guard shared by OnTrashNonKeepersClicked + OnGroupTrashNow.
    // Both are async void and stay re-clickable during the multi-second engine
    // BulkActionResult wait (the confirm dialog is dismissed before that await).
    // WaitForBulkActionResultAsync and the UndoStack "once" handlers match only
    // on the "trashFiles" prefix with no per-command correlation, so a second
    // confirm makes both ops resolve on the FIRST reply (wrong counts) and both
    // push an undo entry for the first batchId (second op gets none). 0 = idle.
    private int _trashInFlight;
    private readonly CancellationTokenSource _lifetimeCts = new();

    private void RequestCleanupRefresh()
    {
        _lastReloadAt = DateTime.UtcNow;
        if (System.Threading.Interlocked.CompareExchange(ref _refreshPending, 1, 0) != 0)
        {
            return; // refresh already queued — coalesce
        }
        DispatcherQueue.TryEnqueue(async () =>
        {
            if (_unloaded) { System.Threading.Interlocked.Exchange(ref _refreshPending, 0); return; }
            try { await ViewModel.RefreshAsync(CancellationToken.None); }
            catch (Exception ex) { DebugLog.Warn("Cleanup refresh failed: " + ex.Message); }
            finally { System.Threading.Interlocked.Exchange(ref _refreshPending, 0); }
        });
    }

    private async void OnLoadedAsync(object sender, RoutedEventArgs e)
    {
        if (_unloaded) return;
        try { await ViewModel.RefreshAsync(CancellationToken.None); }
        catch (Exception ex) { DebugLog.Warn("CleanupView.OnLoaded refresh threw: " + ex.Message); }
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("CleanupView.OnViewModelPropertyChanged", () =>
    {
        if (_unloaded) return;
        OnPropertyChanged(nameof(StatusText));
        OnPropertyChanged(nameof(FooterVisibility));
    });

    // Groups (+ their members) we've wired OnGroupOrMemberChanged on. Tracked
    // explicitly so a CollectionChanged.Reset — which carries neither OldItems
    // nor NewItems — can still unsubscribe the prior handlers instead of leaking
    // them (and double-counting in HeaderStats). The identity-stable merge
    // (CleanupViewModel.MergeByContentHash) normally emits granular Add/Remove,
    // but any residual Clear()/Reset path must not leave dangling subscriptions.
    private readonly System.Collections.Generic.HashSet<DuplicateGroup> _wiredGroups = new();

    private void OnGroupsCollectionChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        if (_unloaded) return;
        OnPropertyChanged(nameof(StatusText));
        OnPropertyChanged(nameof(FooterVisibility));
        OnPropertyChanged(nameof(HeaderStats));

        // Reset (Clear) surfaces no Old/NewItems — unsubscribe everything we've
        // tracked, then re-wire whatever the collection now holds.
        if (e.Action == System.Collections.Specialized.NotifyCollectionChangedAction.Reset)
        {
            foreach (var g in new System.Collections.Generic.List<DuplicateGroup>(_wiredGroups))
            {
                g.PropertyChanged -= OnGroupOrMemberChanged;
                foreach (var m in g.Members) m.PropertyChanged -= OnGroupOrMemberChanged;
            }
            _wiredGroups.Clear();
            foreach (var g in ViewModel.Groups) WireGroup(g);
            return;
        }

        // Wire HeaderStats live updates to every keeper-radio toggle.
        // The DataTemplate's RadioButton TwoWay-binds IsKeeper which
        // fires DuplicateMember.PropertyChanged; we listen once per
        // member added to keep the header counter in sync.
        if (e.NewItems != null)
        {
            foreach (var added in e.NewItems)
            {
                if (added is DuplicateGroup g) WireGroup(g);
            }
        }
        if (e.OldItems != null)
        {
            foreach (var removed in e.OldItems)
            {
                if (removed is DuplicateGroup g) UnwireGroup(g);
            }
        }
    }

    private void WireGroup(DuplicateGroup g)
    {
        if (!_wiredGroups.Add(g)) return;
        g.PropertyChanged += OnGroupOrMemberChanged;
        foreach (var m in g.Members) m.PropertyChanged += OnGroupOrMemberChanged;
    }

    private void UnwireGroup(DuplicateGroup g)
    {
        _wiredGroups.Remove(g);
        g.PropertyChanged -= OnGroupOrMemberChanged;
        foreach (var m in g.Members) m.PropertyChanged -= OnGroupOrMemberChanged;
    }

    private void OnGroupOrMemberChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_unloaded) return;
        if (e.PropertyName is nameof(DuplicateMember.IsKeeper)
            or nameof(DuplicateMember.IsSelectedForTrash)
            or nameof(DuplicateGroup.IsSkipped))
        {
            DispatcherQueue.TryEnqueue(() => OnPropertyChanged(nameof(HeaderStats)));
        }
    }

    private void OnUnloaded(object sender, RoutedEventArgs e)
    {
        _unloaded = true;
        Unloaded -= OnUnloaded;
        Loaded -= OnLoadedAsync;
        try { ViewModel.PropertyChanged -= OnViewModelPropertyChanged; } catch { /* swallow */ }
        try { ViewModel.Groups.CollectionChanged -= OnGroupsCollectionChanged; } catch { /* swallow */ }
        // Detach the per-group/member handlers tracked in _wiredGroups. The
        // identity-stable merge keeps DuplicateGroup instances alive across
        // refreshes, so a still-subscribed group would pin this view after unload.
        // Snapshot first — UnwireGroup mutates _wiredGroups.
        try
        {
            foreach (var g in new System.Collections.Generic.List<DuplicateGroup>(_wiredGroups)) UnwireGroup(g);
            _wiredGroups.Clear();
        }
        catch { /* swallow */ }
        try { ViewModels.EngineClient.Instance.PropertyChanged -= OnEngineChanged; } catch { /* swallow */ }
        try { _lifetimeCts.Cancel(); } catch { /* swallow */ }
        foreach (var (_, cts) in _inflightThumbs) { try { cts.Cancel(); } catch { /* swallow */ } cts.Dispose(); }
        _inflightThumbs.Clear();
        try { _thumbnails.Dispose(); } catch { /* swallow */ }
        try { ViewModel.Dispose(); } catch { /* swallow */ }
        try { _lifetimeCts.Dispose(); } catch { /* swallow */ }
    }

    // ─── Lazy thumbnail loading (macOS CopyTile parity) ───────────────────
    // Same pattern as LibraryView: load on ElementPrepared, cancel + release on
    // ElementClearing so off-screen tiles don't pin BitmapImages.
    private void OnMemberPrepared(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                  Microsoft.UI.Xaml.Controls.ItemsRepeaterElementPreparedEventArgs args)
        => DebugLog.SafeRun(nameof(OnMemberPrepared), () =>
    {
        if (args.Element is not FrameworkElement el) return;
        // x:Bind doesn't populate a realized element's DataContext, so resolve
        // the member from the repeater's bound Members by index, then set
        // DataContext so OnMemberClearing can read it.
        DuplicateMember? member =
            (sender.ItemsSource is IReadOnlyList<DuplicateMember> list
                && args.Index >= 0 && args.Index < list.Count)
                ? list[args.Index]
                : el.DataContext as DuplicateMember;
        if (member is null) return;
        el.DataContext = member;
        member.IsDetached = false;
        if (member.Thumbnail != null) return;
        var cts = new CancellationTokenSource();
        if (!_inflightThumbs.TryAdd(member, cts)) { cts.Dispose(); return; }
        _ = LoadMemberThumbAsync(member, cts);
    });

    private void OnMemberClearing(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                  Microsoft.UI.Xaml.Controls.ItemsRepeaterElementClearingEventArgs args)
        => DebugLog.SafeRun(nameof(OnMemberClearing), () =>
    {
        if (args.Element is not FrameworkElement el || el.DataContext is not DuplicateMember member) return;
        member.IsDetached = true;
        member.ClearThumbnailForRecycle();
        if (_inflightThumbs.TryRemove(member, out var cts)) { try { cts.Cancel(); } catch { /* swallow */ } cts.Dispose(); }
    });

    private async System.Threading.Tasks.Task LoadMemberThumbAsync(DuplicateMember member, CancellationTokenSource ownCts)
    {
        // Capture the token before the first await: OnMemberClearing can cancel +
        // dispose ownCts while we're suspended, after which ownCts.Token throws.
        var ct = ownCts.Token;
        try
        {
            var bmp = await _thumbnails.RequestAsync(member.Path, member.ModifiedAt, ct).ConfigureAwait(false);
            if (bmp == null || ct.IsCancellationRequested || _unloaded) return;
            DispatcherQueue.TryEnqueue(() =>
            {
                if (_unloaded || member.IsDetached) return;
                member.Thumbnail = bmp;
            });
        }
        catch { /* best-effort thumbnail */ }
        finally
        {
            // Remove only OUR entry (key+value): a clear+re-prepare of the same
            // member can replace ours with a newer live CTS, and a key-only remove
            // would drop that one — orphaning the live load (uncancellable on
            // recycle) and leaking its CTS. Dispose only the CTS we still own.
            if (_inflightThumbs.TryRemove(
                    new System.Collections.Generic.KeyValuePair<DuplicateMember, CancellationTokenSource>(member, ownCts)))
            {
                ownCts.Dispose();
            }
        }
    }

    public string StatusText
    {
        get
        {
            if (!string.IsNullOrEmpty(ViewModel.ErrorMessage)) return ViewModel.ErrorMessage!;
            if (ViewModel.IsLoading) return "Scanning for duplicates…";
            if (ViewModel.Groups.Count == 0)
            {
                return IsSimilarMode
                    ? "No visually similar images to review. Scan again after adding files."
                    : "No exact duplicates to review. Scan again after adding files.";
            }
            return $"{ViewModel.Groups.Count} duplicate groups";
        }
    }

    public Visibility FooterVisibility =>
        ViewModel.IsLoading
        || !string.IsNullOrEmpty(ViewModel.ErrorMessage)
        || ViewModel.Groups.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;

    // ─── Cleanup mode (Exact | Similar) — macOS parity ──────────────────────
    public bool IsSimilarMode => ViewModel.Mode == CleanupMode.Similar;

    /// <summary>The "review — not identical" warning banner shows only in Similar
    /// mode.</summary>
    public Visibility SimilarWarningVisibility =>
        IsSimilarMode ? Visibility.Visible : Visibility.Collapsed;

    /// <summary>The global "Trash non-keepers" bulk action is hidden in Similar
    /// mode: those copies are NOT byte-identical, so one-click mass deletion would
    /// be unsafe (macOS parity — "Select all non-keepers" is hidden there too). The
    /// per-group right-click trash stays available for explicit, reviewed deletes.</summary>
    public Visibility TrashNonKeepersVisibility =>
        IsSimilarMode ? Visibility.Collapsed : Visibility.Visible;

    public string HeaderStats
    {
        get
        {
            if (ViewModel.Groups.Count == 0) return string.Empty;
            // Similar mode never stages files for the (hidden) bulk delete — present
            // a review-first summary instead of a "reclaimable" figure so nothing
            // reads as pre-selected for deletion (macOS parity).
            if (IsSimilarMode)
            {
                int skippedSimilar = 0;
                int selectedSimilar = 0;
                foreach (var g in ViewModel.Groups) if (g.IsSkipped) skippedSimilar++;
                foreach (var g in ViewModel.Groups)
                {
                    foreach (var member in g.Members)
                    {
                        if (member.IsSelectedForTrash) selectedSimilar++;
                    }
                }
                int activeSimilar = ViewModel.Groups.Count - skippedSimilar;
                var msg = $"{activeSimilar} similar group{(activeSimilar == 1 ? "" : "s")} • review each before deleting — NOT byte-identical";
                if (selectedSimilar > 0)
                {
                    msg += $" • {selectedSimilar} file{(selectedSimilar == 1 ? "" : "s")} explicitly selected";
                }
                return skippedSimilar > 0 ? $"{msg} • {skippedSimilar} skipped" : msg;
            }
            long files = 0;
            long bytes = 0;
            int eligibleGroups = 0;
            foreach (var g in ViewModel.Groups)
            {
                if (g.IsSkipped) continue;
                eligibleGroups++;
                foreach (var m in g.Members)
                {
                    if (!m.IsKeeper) { files++; bytes += m.SizeBytes; }
                }
            }
            if (files == 0) return $"{eligibleGroups} group{(eligibleGroups == 1 ? "" : "s")} • no non-keepers selected";
            return $"{eligibleGroups} group{(eligibleGroups == 1 ? "" : "s")} • {files} non-keeper file{(files == 1 ? "" : "s")} • {FormatSize(bytes)} reclaimable";
        }
    }

    // ─── Mode toggle (Exact | Similar) ──────────────────────────────────────
    // The two RadioButtons share GroupName="CleanupMode"; Checked fires only on
    // the newly-selected one. The Exact radio's IsChecked="True" fires once during
    // XAML init — SwitchMode no-ops because the VM already defaults to Exact, so
    // the initial OnLoaded refresh stays the single first load.
    private void OnExactModeChecked(object sender, RoutedEventArgs e)
        => SwitchMode(CleanupMode.Exact);

    private void OnSimilarModeChecked(object sender, RoutedEventArgs e)
        => SwitchMode(CleanupMode.Similar);

    private void SwitchMode(CleanupMode mode)
        => DebugLog.SafeRun(nameof(SwitchMode), () =>
        {
            if (_unloaded) return;
            if (ViewModel.Mode == mode) return;
            ViewModel.Mode = mode;
            OnPropertyChanged(nameof(IsSimilarMode));
            OnPropertyChanged(nameof(SimilarWarningVisibility));
            OnPropertyChanged(nameof(TrashNonKeepersVisibility));
            OnPropertyChanged(nameof(HeaderStats));
            // Reload for the new mode. RequestCleanupRefresh coalesces with any
            // in-flight scan-driven refresh; the VM's generation guard discards a
            // superseded result so the just-selected mode wins.
            RequestCleanupRefresh();
        });

    private async void OnRefreshClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnRefreshClicked), async () =>
            await ViewModel.RefreshAsync(CancellationToken.None));

    // Resets every group's keeper back to the first member. Useful after
    // the user has been clicking around and wants to start over without
    // re-scanning. Matches macOS CleanupView "Reset" affordance.
    private void OnResetKeepersClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnResetKeepersClicked), () =>
        {
            foreach (var g in ViewModel.Groups)
            {
                g.IsSkipped = false;
                for (int i = 0; i < g.Members.Count; i++)
                {
                    g.Members[i].IsKeeper = !g.IsSimilar && i == 0;
                    g.Members[i].IsSelectedForTrash = false;
                }
            }
            OnPropertyChanged(nameof(HeaderStats));
        });

    private async void OnTrashNonKeepersClicked(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnTrashNonKeepersClicked), async () =>
    {
        if (System.Threading.Interlocked.CompareExchange(ref _trashInFlight, 1, 0) != 0) return;
        try { await TrashNonKeepersAsync(); }
        finally { System.Threading.Interlocked.Exchange(ref _trashInFlight, 0); }
    });

    private System.Threading.Tasks.Task TrashNonKeepersAsync()
    {
        var groups = ViewModel.Groups.Where(group => !group.IsSkipped).ToArray();
        return TrashGroupsAsync(groups, "Trash duplicates?", recoverable: true);
    }

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:0.#} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):0.#} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):0.##} GB";
    }

    // ─── FEAT-CRIT-2: Per-group action menu handlers ─────────────────

    private void OnGroupFlyoutOpening(object sender, object e)
        => DebugLog.SafeRun(nameof(OnGroupFlyoutOpening), () =>
    {
        if (sender is not MenuFlyout flyout) return;
        var contentHash = (flyout.Target as FrameworkElement)?.DataContext is DuplicateGroup group
            ? group.ContentHash
            : null;
        foreach (var item in flyout.Items)
        {
            if (item is FrameworkElement element) element.Tag = contentHash;
        }
    });

    private DuplicateGroup? GroupFromFlyoutItem(object sender) =>
        sender is FrameworkElement { Tag: string contentHash }
            ? ViewModel.Groups.FirstOrDefault(group => group.ContentHash == contentHash)
            : null;

    private void OnGroupKeepFirst(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnGroupKeepFirst), () =>
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null || grp.IsSimilar || grp.Members.Count == 0) return;
        for (int i = 0; i < grp.Members.Count; i++)
        {
            grp.Members[i].IsKeeper = (i == 0);
        }
    });

    private void OnGroupKeepShallowest(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnGroupKeepShallowest), () =>
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null || grp.IsSimilar || grp.Members.Count == 0) return;
        // Within a byte-identical group every member is the same size, so "keep
        // largest" was always a no-op (kept index 0). Keep the copy in the
        // least-nested / most-canonical location instead: fewest path
        // separators, then shortest path, then ordinal (#19).
        static int Depth(string p)
        {
            int n = 0;
            foreach (var c in p) if (c == '\\' || c == '/') n++;
            return n;
        }
        var bestIdx = 0;
        for (int i = 1; i < grp.Members.Count; i++)
        {
            string a = grp.Members[i].Path, b = grp.Members[bestIdx].Path;
            int da = Depth(a), db = Depth(b);
            bool better = da < db
                || (da == db && a.Length < b.Length)
                || (da == db && a.Length == b.Length && string.CompareOrdinal(a, b) < 0);
            if (better) bestIdx = i;
        }
        for (int i = 0; i < grp.Members.Count; i++)
        {
            grp.Members[i].IsKeeper = (i == bestIdx);
        }
    });

    private void OnGroupInvert(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnGroupInvert), () =>
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null || grp.IsSimilar || grp.Members.Count == 0) return;
        var currentIdx = -1;
        for (int i = 0; i < grp.Members.Count; i++)
        {
            if (grp.Members[i].IsKeeper) { currentIdx = i; break; }
        }
        // No keeper marked (currentIdx == -1): start the cycle deterministically
        // at index 0 instead of relying on the (-1 + 1) % count wrap coincidence.
        if (currentIdx == -1) currentIdx = grp.Members.Count - 1;
        var nextIdx = (currentIdx + 1) % grp.Members.Count;
        for (int i = 0; i < grp.Members.Count; i++)
        {
            grp.Members[i].IsKeeper = (i == nextIdx);
        }
    });

    private void OnGroupSkip(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnGroupSkip), () =>
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp != null) grp.IsSkipped = true;
    });

    private void OnGroupUnskip(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnGroupUnskip), () =>
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp != null) grp.IsSkipped = false;
    });

    private async void OnGroupTrashNow(object sender, RoutedEventArgs e)
        => await DebugLog.SafeRunAsync(nameof(OnGroupTrashNow), async () =>
    {
        if (System.Threading.Interlocked.CompareExchange(ref _trashInFlight, 1, 0) != 0) return;
        try { await TrashGroupAsync(sender); }
        finally { System.Threading.Interlocked.Exchange(ref _trashInFlight, 0); }
    });

    private System.Threading.Tasks.Task TrashGroupAsync(object sender)
    {
        var group = GroupFromFlyoutItem(sender);
        return group is null
            ? System.Threading.Tasks.Task.CompletedTask
            : TrashGroupsAsync(new[] { group }, "Trash this group?", recoverable: false);
    }

    private async System.Threading.Tasks.Task TrashGroupsAsync(
        IReadOnlyList<DuplicateGroup> groups,
        string confirmTitle,
        bool recoverable)
    {
        if (groups.Count == 0)
        {
            await ShowAlertAsync("Nothing to trash", "There are no active duplicate groups to process.");
            return;
        }
        var similar = groups[0].IsSimilar;
        if (groups.Any(group => group.IsSimilar != similar))
        {
            await ShowAlertAsync("Cleanup changed", "The duplicate view changed while the action was opening. Review the groups and try again.");
            return;
        }

        var requests = new List<ExactCleanupGroupRequest>(groups.Count);
        long selectedBytes = 0;
        foreach (var group in groups)
        {
            var keeper = CleanupSelectionPolicy.RetainedCopy(group);
            var victims = CleanupSelectionPolicy.SelectedVictims(group)
                .Select(member => new ExactCleanupFile(member.Id, member.Path, member.SizeBytes))
                .ToArray();
            if (victims.Length == 0) continue;
            if (keeper is null)
            {
                await ShowAlertAsync(
                    similar ? "Keep at least one copy" : "Choose a keeper",
                    similar
                        ? "A visually similar group cannot trash every visible copy. Clear at least one Trash checkbox, review that retained copy, and try again."
                        : "Every duplicate group must retain one unselected keeper before files can be trashed.");
                return;
            }
            foreach (var victim in victims)
            {
                if (victim.SizeBytes < 0 || selectedBytes > long.MaxValue - victim.SizeBytes)
                {
                    await ShowAlertAsync("Invalid duplicate size", "The selected duplicate sizes are invalid. Re-run the scan before retrying.");
                    return;
                }
                selectedBytes += victim.SizeBytes;
            }
            requests.Add(new ExactCleanupGroupRequest(
                new ExactCleanupFile(keeper.Id, keeper.Path, keeper.SizeBytes),
                victims));
        }
        var selectedCount = requests.Sum(request => request.Victims.Count);
        if (selectedCount == 0)
        {
            await ShowAlertAsync(
                similar ? "Select copies to trash" : "Nothing to trash",
                similar
                    ? "Nothing in Similar mode is pre-selected. Check Trash only on the visually similar copies you reviewed and explicitly want to move to the Recycle Bin."
                    : "Every file in the active groups is marked as a keeper, so there are no non-keepers to move to the Recycle Bin.");
            return;
        }

        var confirmationText = similar
            ? $"{selectedCount} explicitly selected file{(selectedCount == 1 ? "" : "s")} ({FormatSize(selectedBytes)}) will move to the Recycle Bin. These files are visually similar, not byte-identical, and FileID will not byte-verify them. Unchecked copies stay in place."
            : $"{selectedCount} non-keeper file{(selectedCount == 1 ? "" : "s")} ({FormatSize(selectedBytes)}) will move to the Recycle Bin." +
                (recoverable ? " They stay recoverable from there." : string.Empty);
        var confirm = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = similar ? "Move selected similar copies?" : confirmTitle,
            Content = confirmationText,
            PrimaryButtonText = similar ? "Move Selected Copies" : "Move to Recycle Bin",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        ContentDialogResult choice;
        try { choice = await confirm.ShowAsync(); }
        catch (Exception ex)
        {
            DebugLog.Warn("Trash confirm dialog failed (another dialog open?): " + ex.Message);
            return;
        }
        if (choice != ContentDialogResult.Primary) return;

        IReadOnlyList<FileID.IpcSchema.ExactTrashIdentity>? identities = null;
        var preflightRejected = 0;
        var timeout = TimeSpan.FromSeconds(30);
        if (!similar)
        {
            var proof = await BuildExactProofAsync(requests);
            if (proof is null) return;
            identities = proof.Identities;
            preflightRejected = proof.Rejections.Count;
            if (identities.Count == 0)
            {
                await ShowAlertAsync(
                    "No exact copies were trashed",
                    $"All {preflightRejected} selected files changed or could not be byte-verified against their keeper.");
                await ViewModel.RefreshAsync(CancellationToken.None);
                return;
            }
            timeout = ExactCleanupProofBuilder.EngineTimeout(proof.AuthorizationBytes);
        }

        var ids = identities?.Select(identity => identity.FileId).ToArray()
            ?? requests.SelectMany(request => request.Victims).Select(file => file.FileId).Distinct().ToArray();
        IDisposable CaptureUndo() => Services.UndoStack.CaptureNextBulkResult(
            "trashFiles:",
            $"trash {ids.Length} duplicate{(ids.Length == 1 ? "" : "s")}",
            kind: Services.ChangeKind.Trash,
            timeout: Timeout.InfiniteTimeSpan,
            reverse: async batchId =>
            {
                if (string.IsNullOrEmpty(batchId)) return false;
                try { return await ViewModels.EngineClient.Instance.RestoreFromTrashAsync(batchId); }
                catch { return false; }
            });

        try
        {
            var result = await ViewModels.EngineClient.Instance.WaitForBulkActionResultAsync(
                "trashFiles",
                () => identities is null
                    ? ViewModels.EngineClient.Instance.TrashFilesAsync(ids)
                    : ViewModels.EngineClient.Instance.TrashExactFilesAsync(identities),
                timeout,
                beforeSend: CaptureUndo);
            var totalFailed = checked((int)result.Failed + preflightRejected);
            if (totalFailed > 0)
            {
                var first = result.Messages?.FirstOrDefault(message => !message.Ok)?.Message;
                var detail = string.IsNullOrWhiteSpace(first) ? string.Empty : $" — {first}";
                var preflight = preflightRejected == 0
                    ? string.Empty
                    : $" {preflightRejected} changed or failed full-byte verification before the command.";
                await ShowAlertAsync(
                    "Some files weren't trashed",
                    $"Trashed {result.Succeeded}; {totalFailed} failed or were rejected.{preflight}{detail}");
            }
            await ViewModel.RefreshAsync(CancellationToken.None);
        }
        catch (TimeoutException)
        {
            await ShowAlertAsync(
                "Trash didn't confirm",
                $"The engine didn't confirm the operation within {timeout.TotalMinutes:0.#} minutes. It may still be processing or the files may have moved — re-run the scan to check before retrying.");
        }
        catch (Exception ex)
        {
            await ShowAlertAsync("Trash failed", $"Couldn't trash the selected files: {ex.Message}");
        }
    }

    private async System.Threading.Tasks.Task<ExactCleanupProof?> BuildExactProofAsync(
        IReadOnlyList<ExactCleanupGroupRequest> requests)
    {
        using var cancellation = CancellationTokenSource.CreateLinkedTokenSource(_lifetimeCts.Token);
        var progressText = new TextBlock { Text = "Preparing full-file verification…" };
        var panel = new StackPanel { Spacing = 12 };
        panel.Children.Add(new ProgressRing { IsActive = true, Width = 32, Height = 32 });
        panel.Children.Add(progressText);
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Verifying exact copies",
            Content = panel,
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        var verificationFinished = false;
        dialog.Closed += (_, _) =>
        {
            if (!verificationFinished) cancellation.Cancel();
        };
        var progress = new Progress<ExactCleanupProgress>(value =>
        {
            if (!_unloaded)
            {
                progressText.Text = $"Verified {value.CompletedFiles:N0} of {value.TotalFiles:N0} files…";
            }
        });

        Windows.Foundation.IAsyncOperation<ContentDialogResult> operation;
        try { operation = dialog.ShowAsync(); }
        catch (Exception ex)
        {
            DebugLog.Warn("Exact verification dialog failed: " + ex.Message);
            return null;
        }
        ExactCleanupProof? proof = null;
        string? failure = null;
        var wasCancelled = false;
        try
        {
            proof = await ExactCleanupProofBuilder.BuildAsync(requests, progress, cancellation.Token);
            if (cancellation.IsCancellationRequested)
            {
                wasCancelled = true;
                failure = "No Trash command was sent.";
                proof = null;
            }
        }
        catch (OperationCanceledException)
        {
            wasCancelled = true;
            failure = "No Trash command was sent.";
        }
        catch (Exception ex)
        {
            failure = ex.Message;
        }
        finally
        {
            verificationFinished = true;
            try { dialog.Hide(); } catch { /* already closed */ }
            try { await operation; } catch { /* teardown */ }
        }
        if (failure is not null && !_unloaded)
        {
            await ShowAlertAsync(
                wasCancelled ? "Verification cancelled" : "Exact verification failed",
                failure);
        }
        return proof;
    }

    // Dismissible alert mirroring SidebarProcessingControl.ShowAlertAsync —
    // surfaces a partial/failed bulk op so the user is never left thinking a
    // trash succeeded when some (or all) of it didn't.
    private async System.Threading.Tasks.Task ShowAlertAsync(string title, string body)
    {
        try
        {
            if (_unloaded || XamlRoot is null)
            {
                DebugLog.Warn($"CleanupView.ShowAlertAsync: XamlRoot null/unloaded ({title}); skipping dialog.");
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
            DebugLog.Warn($"CleanupView.ShowAlertAsync({title}) threw: " + ex.Message);
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
