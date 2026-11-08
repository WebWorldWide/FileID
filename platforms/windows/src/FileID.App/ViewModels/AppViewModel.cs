// AppViewModel — the shell-level state the MainWindow + sidebar bind to.
//
// One instance lives on the UI thread (created at App startup). It owns:
//   - The active SidebarTab (persisted via AppSettings.ActiveTab)
//   - Sidebar visibility (persisted)
//   - The currently-picked library folder (path + display label)
//   - Auto-tab-switch reactions to engine events (matches macOS:
//     face clustering done → People; deep analyze done → Library)
//
// Settings persistence: any property setter that maps to a persisted key
// calls AppSettings.Save() right after raising PropertyChanged. Cheap on
// modern SSD; debouncing is a future polish if profiling demands it.

using System.ComponentModel;
using System.Runtime.CompilerServices;
using FileID.IpcSchema;
using FileID.Services;

namespace FileID.ViewModels;

internal sealed class AppViewModel : INotifyPropertyChanged
{
    public static AppViewModel Instance { get; } = new();

    private readonly AppSettings _settings;

    private AppViewModel()
    {
        _settings = AppSettings.Load();
        _activeTab = SidebarTab.ById(_settings.ActiveTab);
        _sidebarVisible = _settings.SidebarVisible;
        _folderPath = _settings.LastFolderPath;
        _folderDisplay = _settings.LastFolderDisplay;
        // auto-tab-switch subscription removed. WorkflowAutoTabRouter
        // owns that policy (single source of truth) so the tab doesn't flip
        // twice on the same engine event.
    }

    public AppSettings Settings => _settings;

    private SidebarTab _activeTab;
    public SidebarTab ActiveTab
    {
        get => _activeTab;
        set
        {
            if (Set(ref _activeTab, value))
            {
                if (value.Area is { } area)
                {
                    _lastTabByArea[area] = value.Id;
                }
                _settings.ActiveTab = value.Id;
                _settings.Save();
            }
        }
    }

    private readonly Dictionary<string, string> _lastTabByArea = new();

    /// <summary>The page last used in <paramref name="area"/>, or the area's
    /// first page (macOS Area.remembered).</summary>
    public SidebarTab RememberedTab(string area)
    {
        if (_activeTab.Area == area) return _activeTab;
        var pages = SidebarTab.InArea(area);
        if (_lastTabByArea.TryGetValue(area, out var id))
        {
            foreach (var page in pages)
            {
                if (page.Id == id) return page;
            }
        }
        return pages.Count > 0 ? pages[0] : SidebarTab.Library;
    }

    private string? _pendingChatDraft;
    private string? _pendingLibraryQuery;

    /// <summary>Raised after the title-bar search hands a draft to Chat.</summary>
    public event EventHandler? ChatDraftRequested;

    /// <summary>Raised after the title-bar search hands a query to Library.</summary>
    public event EventHandler? LibraryQueryRequested;

    public void RequestChatDraft(string draft)
    {
        _pendingChatDraft = draft;
        ActiveTab = SidebarTab.Chat;
        ChatDraftRequested?.Invoke(this, EventArgs.Empty);
    }

    public void RequestLibraryQuery(string query)
    {
        _pendingLibraryQuery = query;
        ActiveTab = SidebarTab.Library;
        LibraryQueryRequested?.Invoke(this, EventArgs.Empty);
    }

    private IReadOnlyList<CatalogHit>? _pendingTakeCandidates;

    /// <summary>Opens Best Takes with these search results as its clip pool
    /// (the macOS "Best takes" sheet from Search &amp; Moments).</summary>
    public void RequestBestTakes(IReadOnlyList<CatalogHit> candidates)
    {
        _pendingTakeCandidates = candidates;
        ActiveTab = SidebarTab.Takes;
    }

    public IReadOnlyList<CatalogHit>? TakeBestTakesCandidates()
    {
        var candidates = _pendingTakeCandidates;
        _pendingTakeCandidates = null;
        return candidates;
    }

    private string? _pendingMomentsQuery;
    private IReadOnlyList<CatalogHit>? _pendingToolFiles;

    /// <summary>Raised after Chat hands a search to Search &amp; Moments.</summary>
    public event EventHandler? MomentsQueryRequested;

    /// <summary>Raised after Chat hands chosen files to File Tools.</summary>
    public event EventHandler? ToolFilesRequested;

    public void RequestMomentsQuery(string query)
    {
        _pendingMomentsQuery = query;
        ActiveTab = SidebarTab.Moments;
        MomentsQueryRequested?.Invoke(this, EventArgs.Empty);
    }

    public string? TakeMomentsQuery()
    {
        var query = _pendingMomentsQuery;
        _pendingMomentsQuery = null;
        return query;
    }

    public void RequestToolFiles(IReadOnlyList<CatalogHit> files)
    {
        _pendingToolFiles = files;
        ActiveTab = SidebarTab.Tools;
        ToolFilesRequested?.Invoke(this, EventArgs.Empty);
    }

    public IReadOnlyList<CatalogHit>? TakeToolFiles()
    {
        var files = _pendingToolFiles;
        _pendingToolFiles = null;
        return files;
    }

    private ChatScope? _chatScope;

    /// <summary>Raised when the files attached to the next chat question change.</summary>
    public event EventHandler? ChatScopeChanged;

    /// <summary>Files attached to the next chat question, or null for the whole library.</summary>
    public ChatScope? ChatScope
    {
        get => _chatScope;
        set
        {
            if (Equals(_chatScope, value)) return;
            _chatScope = value;
            ChatScopeChanged?.Invoke(this, EventArgs.Empty);
        }
    }

    /// <summary>Opens Chat with these files attached as the next question's scope.</summary>
    public void RequestChatScope(ChatScope? scope)
    {
        if (scope is null) return;
        ChatScope = scope;
        ActiveTab = SidebarTab.Chat;
    }

    public string? TakeChatDraft()
    {
        var draft = _pendingChatDraft;
        _pendingChatDraft = null;
        return draft;
    }

    public string? TakeLibraryQuery()
    {
        var query = _pendingLibraryQuery;
        _pendingLibraryQuery = null;
        return query;
    }

    private bool _sidebarVisible;
    public bool SidebarVisible
    {
        get => _sidebarVisible;
        set
        {
            if (Set(ref _sidebarVisible, value))
            {
                _settings.SidebarVisible = value;
                _settings.Save();
            }
        }
    }

    private string? _folderPath;
    public string? FolderPath
    {
        get => _folderPath;
        set
        {
            if (Set(ref _folderPath, value))
            {
                _settings.LastFolderPath = value;
                if (!string.IsNullOrEmpty(value))
                {
                    _folderDisplay = System.IO.Path.GetFileName(value.TrimEnd('\\'));
                    if (string.IsNullOrEmpty(_folderDisplay))
                    {
                        _folderDisplay = value;
                    }
                    _settings.LastFolderDisplay = _folderDisplay;
                    PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(FolderDisplay)));
                    PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasFolder)));
                }
                else
                {
                    _folderDisplay = null;
                    _settings.LastFolderDisplay = null;
                    PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(FolderDisplay)));
                    PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(HasFolder)));
                }
                _settings.Save();
            }
        }
    }

    private string? _folderDisplay;
    public string? FolderDisplay => _folderDisplay;

    public bool HasFolder => !string.IsNullOrEmpty(_folderPath);

    public async Task ChooseFolderAsync(string path)
    {
        FolderPath = path;
        var engine = EngineClient.Instance;
        var phase = engine.Phase ?? engine.LastProgress?.Phase;
        if (phase is ScanPhase.Discovering or ScanPhase.Tagging or ScanPhase.PostScan) return;
        if (engine.GpuDeviceRemoved || engine.State == EngineClient.LifecycleState.Crashed) return;
        try
        {
            if (engine.State != EngineClient.LifecycleState.Ready)
            {
                await engine.WaitForReadyAsync(TimeSpan.FromSeconds(30));
            }
            if (!string.Equals(FolderPath, path, StringComparison.Ordinal)) return;
            await engine.StartScanAsync(path, FolderDisplay);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Scan after choosing a folder failed: " + ex.Message);
        }
    }

    private IReadOnlyList<LibraryRoot> _libraryRoots = Array.Empty<LibraryRoot>();

    /// <summary>The scanned locations from the last listRoots reply.</summary>
    public IReadOnlyList<LibraryRoot> LibraryRoots => _libraryRoots;

    private LibraryScope? _scope;

    /// <summary>The location Library, Search &amp; Moments and Chat show; null for "All locations".</summary>
    public LibraryScope? Scope => _scope;

    /// <summary>Picks a location, or "All locations" for null, and remembers it.</summary>
    public void ChooseScope(long? rootId)
    {
        _settings.LibraryScopeRootId = rootId;
        _settings.Save();
        ApplyScope();
    }

    /// <summary>Takes a fresh listRoots reply: keeps the stored choice while its location exists.</summary>
    public void ApplyRoots(IReadOnlyList<LibraryRoot> roots)
    {
        _libraryRoots = roots;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(LibraryRoots)));
        var kept = LibraryScope.Reconciled(_settings.LibraryScopeRootId, roots);
        if (kept != _settings.LibraryScopeRootId)
        {
            _settings.LibraryScopeRootId = kept;
            _settings.Save();
        }
        ApplyScope();
    }

    public async Task RefreshRootsAsync(CancellationToken ct = default)
    {
        try
        {
            var list = await EngineClient.Instance.ListRootsAsync(ct);
            ApplyRoots(list.Roots);
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            DebugLog.Warn("Location list refresh failed: " + ex.Message);
        }
    }

    private void ApplyScope()
    {
        var next = LibraryScope.For(_settings.LibraryScopeRootId, _libraryRoots);
        var same = next?.Root.Id == _scope?.Root.Id && next?.Param == _scope?.Param;
        _scope = next;
        if (!same) PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Scope)));
    }

    /// <summary>Toggles the sidebar visible/hidden state. Invoked by Ctrl+Shift+S.</summary>
    public void ToggleSidebar() => SidebarVisible = !SidebarVisible;

    public event PropertyChangedEventHandler? PropertyChanged;

    // OnEngineClientPropertyChanged removed entirely. Auto-tab-switch
    // policy lives in Services/WorkflowAutoTabRouter.cs as the single owner.

    private bool Set<T>(ref T field, T value, [CallerMemberName] string? propertyName = null)
    {
        if (EqualityComparer<T>.Default.Equals(field, value))
        {
            return false;
        }
        field = value;
        PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
        return true;
    }
}
