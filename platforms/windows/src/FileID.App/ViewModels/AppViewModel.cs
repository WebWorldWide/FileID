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
        try
        {
            await engine.StartScanAsync(path, FolderDisplay);
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Scan after choosing a folder failed: " + ex.Message);
        }
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
