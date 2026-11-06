using System.ComponentModel;
using FileID.Services;
using FileID.ViewModels;

namespace FileID.Views.Library;

/// <summary>The title-bar location scope: the grid, its counts and paging list only the chosen
/// location (macOS ReadStore scope).</summary>
public sealed partial class LibraryView
{
    private void StartScope()
    {
        if (_unloaded) return;
        AppViewModel.Instance.PropertyChanged -= OnAppScopeChanged;
        AppViewModel.Instance.PropertyChanged += OnAppScopeChanged;
        ApplyScope();
    }

    private void OnAppScopeChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun(nameof(OnAppScopeChanged), () =>
        {
            if (e.PropertyName != nameof(AppViewModel.Scope)) return;
            DebugLog.Debug($"[ENGINE-SUB:LibraryView] {e.PropertyName}");
            DispatcherQueue.TryEnqueue(ApplyScope);
        });

    private void ApplyScope()
    {
        if (_unloaded) return;
        var scope = AppViewModel.Instance.Scope;
        if (ReferenceEquals(_store.Scope, scope)) return;
        _store.Scope = scope;
        _ = DebugLog.SafeRunAsync("LibraryView.ApplyScope", () => ViewModel.RefreshAsync(CancellationToken.None));
    }
}
