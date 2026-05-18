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
                _settings.ActiveTab = value.Id;
                _settings.Save();
            }
        }
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
