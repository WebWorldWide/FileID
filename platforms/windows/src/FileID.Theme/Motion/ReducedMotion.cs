// Surfaces the OS "minimize animations" preference (Settings > Accessibility > Visual effects > Animation effects)
// as one observable bool that every motion primitive subscribes to.

using System.ComponentModel;
using System.Diagnostics;
using Windows.UI.ViewManagement;

namespace FileID.Theme.Motion;

public sealed class ReducedMotion : INotifyPropertyChanged
{
    /// <summary>Process-wide singleton.</summary>
    public static ReducedMotion Instance { get; } = new();

    private readonly UISettings _settings;
    private bool _isReduced;

    private ReducedMotion()
    {
        _settings = new UISettings();
        _isReduced = !_settings.AnimationsEnabled;
        _settings.AnimationsEnabledChanged += OnAnimationsEnabledChanged;
    }

    /// <summary>True when the user has asked Windows to minimize animations; every motion primitive gates on it.</summary>
    public bool IsReduced
    {
        get => _isReduced;
        private set
        {
            if (_isReduced == value)
            {
                return;
            }
            _isReduced = value;
            RaisePropertyChangedIsolated(nameof(IsReduced));
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void RaisePropertyChangedIsolated(string propertyName)
    {
        var handlers = PropertyChanged;
        if (handlers is null) return;

        var args = new PropertyChangedEventArgs(propertyName);
        foreach (PropertyChangedEventHandler handler in handlers.GetInvocationList())
        {
            try
            {
                handler(this, args);
            }
            catch (Exception ex)
            {
                Trace.TraceError($"ReducedMotion.PropertyChanged subscriber failed: {ex}");
            }
        }
    }

    private void OnAnimationsEnabledChanged(UISettings sender, UISettingsAnimationsEnabledChangedEventArgs args)
    {
        // Fires off the UI thread and is not marshalled here, so XAML bindings and view-models do not pay
        // the dispatcher hop twice; subscribers that touch UI marshal themselves.
        IsReduced = !sender.AnimationsEnabled;
    }
}
