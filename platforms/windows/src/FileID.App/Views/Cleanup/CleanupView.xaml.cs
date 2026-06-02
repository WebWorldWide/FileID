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
    {
        if (_unloaded) return;
        OnPropertyChanged(nameof(StatusText));
        OnPropertyChanged(nameof(FooterVisibility));
    }

    private void OnGroupsCollectionChanged(object? sender, System.Collections.Specialized.NotifyCollectionChangedEventArgs e)
    {
        if (_unloaded) return;
        OnPropertyChanged(nameof(StatusText));
        OnPropertyChanged(nameof(FooterVisibility));
        OnPropertyChanged(nameof(HeaderStats));
        // Wire HeaderStats live updates to every keeper-radio toggle.
        // The DataTemplate's RadioButton TwoWay-binds IsKeeper which
        // fires DuplicateMember.PropertyChanged; we listen once per
        // member added to keep the header counter in sync.
        if (e.NewItems != null)
        {
            foreach (var added in e.NewItems)
            {
                if (added is DuplicateGroup g)
                {
                    g.PropertyChanged += OnGroupOrMemberChanged;
                    foreach (var m in g.Members)
                    {
                        m.PropertyChanged += OnGroupOrMemberChanged;
                    }
                }
            }
        }
        if (e.OldItems != null)
        {
            foreach (var removed in e.OldItems)
            {
                if (removed is DuplicateGroup g)
                {
                    g.PropertyChanged -= OnGroupOrMemberChanged;
                    foreach (var m in g.Members)
                    {
                        m.PropertyChanged -= OnGroupOrMemberChanged;
                    }
                }
            }
        }
    }

    private void OnGroupOrMemberChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_unloaded) return;
        if (e.PropertyName is nameof(DuplicateMember.IsKeeper) or nameof(DuplicateGroup.IsSkipped))
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
        try { ViewModels.EngineClient.Instance.PropertyChanged -= OnEngineChanged; } catch { /* swallow */ }
        foreach (var (_, cts) in _inflightThumbs) { try { cts.Cancel(); } catch { /* swallow */ } cts.Dispose(); }
        _inflightThumbs.Clear();
        try { _thumbnails.Dispose(); } catch { /* swallow */ }
        try { ViewModel.Dispose(); } catch { /* swallow */ }
    }

    // ─── Lazy thumbnail loading (macOS CopyTile parity) ───────────────────
    // Same pattern as LibraryView: load on ElementPrepared, cancel + release on
    // ElementClearing so off-screen tiles don't pin BitmapImages.
    private void OnMemberPrepared(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                  Microsoft.UI.Xaml.Controls.ItemsRepeaterElementPreparedEventArgs args)
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
        _ = LoadMemberThumbAsync(member, cts.Token);
    }

    private void OnMemberClearing(Microsoft.UI.Xaml.Controls.ItemsRepeater sender,
                                  Microsoft.UI.Xaml.Controls.ItemsRepeaterElementClearingEventArgs args)
    {
        if (args.Element is not FrameworkElement el || el.DataContext is not DuplicateMember member) return;
        member.IsDetached = true;
        member.ClearThumbnailForRecycle();
        if (_inflightThumbs.TryRemove(member, out var cts)) { try { cts.Cancel(); } catch { /* swallow */ } cts.Dispose(); }
    }

    private async System.Threading.Tasks.Task LoadMemberThumbAsync(DuplicateMember member, CancellationToken ct)
    {
        try
        {
            var bmp = await _thumbnails.RequestAsync(member.Path, null, ct).ConfigureAwait(false);
            if (bmp == null || ct.IsCancellationRequested || _unloaded) return;
            DispatcherQueue.TryEnqueue(() =>
            {
                if (_unloaded || member.IsDetached) return;
                member.Thumbnail = bmp;
            });
        }
        catch { /* best-effort thumbnail */ }
        finally { _inflightThumbs.TryRemove(member, out _); }
    }

    public string StatusText
    {
        get
        {
            if (!string.IsNullOrEmpty(ViewModel.ErrorMessage)) return ViewModel.ErrorMessage!;
            if (ViewModel.IsLoading) return "Scanning for duplicates…";
            if (ViewModel.Groups.Count == 0) return "No duplicates found yet — run a scan first.";
            return $"{ViewModel.Groups.Count} duplicate groups";
        }
    }

    public Visibility FooterVisibility =>
        ViewModel.IsLoading
        || !string.IsNullOrEmpty(ViewModel.ErrorMessage)
        || ViewModel.Groups.Count == 0
            ? Visibility.Visible : Visibility.Collapsed;

    public string HeaderStats
    {
        get
        {
            if (ViewModel.Groups.Count == 0) return string.Empty;
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

    private async void OnRefreshClicked(object sender, RoutedEventArgs e)
        => await ViewModel.RefreshAsync(CancellationToken.None);

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
                    g.Members[i].IsKeeper = (i == 0);
                }
            }
            OnPropertyChanged(nameof(HeaderStats));
        });

    private async void OnTrashNonKeepersClicked(object sender, RoutedEventArgs e)
    {
        var ids = new List<long>();
        long bytes = 0;
        foreach (var grp in ViewModel.Groups)
        {
            // FEAT-CRIT-2: skipped groups are excluded from the global
            // "Trash non-keepers" run.
            if (grp.IsSkipped) continue;
            foreach (var m in grp.Members)
            {
                if (!m.IsKeeper)
                {
                    ids.Add(m.Id);
                    bytes += m.SizeBytes;
                }
            }
        }
        if (ids.Count == 0)
        {
            return;
        }
        var sizeDisplay = FormatSize(bytes);
        var confirm = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Trash duplicates?",
            Content = $"{ids.Count} non-keeper file{(ids.Count == 1 ? "" : "s")} ({sizeDisplay}) will move to the Recycle Bin. They stay recoverable from there.",
            PrimaryButtonText = "Move to Recycle Bin",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        var choice = await confirm.ShowAsync();
        if (choice != ContentDialogResult.Primary) return;

        try
        {
            Services.UndoStack.CaptureNextBulkResult(
                "trashFiles:",
                $"trash {ids.Count} duplicate{(ids.Count == 1 ? "" : "s")}",
                async batchId =>
                {
                    if (string.IsNullOrEmpty(batchId)) return false;
                    try
                    {
                        await ViewModels.EngineClient.Instance.RestoreFromTrashAsync(batchId);
                        return true;
                    }
                    catch { return false; }
                });
            await ViewModels.EngineClient.Instance.TrashFilesAsync(ids);
        }
        catch
        {
            // Result surfaces via BulkActionResultEvent.
        }

        await ViewModel.RefreshAsync(CancellationToken.None);
    }

    private static string FormatSize(long bytes)
    {
        if (bytes < 1024) return $"{bytes} B";
        if (bytes < 1024 * 1024) return $"{bytes / 1024.0:0.#} KB";
        if (bytes < 1024L * 1024 * 1024) return $"{bytes / (1024.0 * 1024):0.#} MB";
        return $"{bytes / (1024.0 * 1024 * 1024):0.##} GB";
    }

    // ─── FEAT-CRIT-2: Per-group action menu handlers ─────────────────

    // WinUI 3 MenuFlyoutItem inside a Grid.ContextFlyout does NOT
    // inherit the parent Grid's DataContext, so the prior version's
    // `item.DataContext as DuplicateGroup` always returned null and every
    // per-group action silently no-op'd. Fix: cache the right-tapped group
    // here at the moment the context menu is invoked.
    private DuplicateGroup? _lastRightTappedGroup;

    private void OnGroupRightTapped(object sender, Microsoft.UI.Xaml.Input.RightTappedRoutedEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.DataContext is DuplicateGroup g)
        {
            _lastRightTappedGroup = g;
        }
    }

    private DuplicateGroup? GroupFromFlyoutItem(object sender) => _lastRightTappedGroup;

    private void OnGroupKeepFirst(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null || grp.Members.Count == 0) return;
        for (int i = 0; i < grp.Members.Count; i++)
        {
            grp.Members[i].IsKeeper = (i == 0);
        }
    }

    private void OnGroupKeepShallowest(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null || grp.Members.Count == 0) return;
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
    }

    private void OnGroupInvert(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null || grp.Members.Count == 0) return;
        var currentIdx = -1;
        for (int i = 0; i < grp.Members.Count; i++)
        {
            if (grp.Members[i].IsKeeper) { currentIdx = i; break; }
        }
        var nextIdx = (currentIdx + 1) % grp.Members.Count;
        for (int i = 0; i < grp.Members.Count; i++)
        {
            grp.Members[i].IsKeeper = (i == nextIdx);
        }
    }

    private void OnGroupSkip(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp != null) grp.IsSkipped = true;
    }

    private void OnGroupUnskip(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp != null) grp.IsSkipped = false;
    }

    private async void OnGroupTrashNow(object sender, RoutedEventArgs e)
    {
        var grp = GroupFromFlyoutItem(sender);
        if (grp == null) return;
        var ids = new List<long>();
        long bytes = 0;
        foreach (var m in grp.Members)
        {
            if (!m.IsKeeper) { ids.Add(m.Id); bytes += m.SizeBytes; }
        }
        if (ids.Count == 0) return;
        var confirm = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = "Trash this group?",
            Content = $"{ids.Count} non-keeper file{(ids.Count == 1 ? "" : "s")} ({FormatSize(bytes)}) will move to the Recycle Bin.",
            PrimaryButtonText = "Move to Recycle Bin",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await confirm.ShowAsync() != ContentDialogResult.Primary) return;
        try
        {
            Services.UndoStack.CaptureNextBulkResult(
                "trashFiles:",
                $"trash {ids.Count} duplicate{(ids.Count == 1 ? "" : "s")}",
                async batchId =>
                {
                    if (string.IsNullOrEmpty(batchId)) return false;
                    try { await ViewModels.EngineClient.Instance.RestoreFromTrashAsync(batchId); return true; }
                    catch { return false; }
                });
            await ViewModels.EngineClient.Instance.TrashFilesAsync(ids);
        }
        catch { }
        await ViewModel.RefreshAsync(CancellationToken.None);
    }

    public event PropertyChangedEventHandler? PropertyChanged;
    private void OnPropertyChanged(string name)
        => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(name));
}
