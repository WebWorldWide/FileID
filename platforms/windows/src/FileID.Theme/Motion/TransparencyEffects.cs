using System.ComponentModel;
using System.Diagnostics;
using Windows.UI.ViewManagement;

namespace FileID.Theme.Motion;

/// <summary>
/// The OS "Transparency effects" toggle (UISettings.AdvancedEffectsEnabled)
/// and high-contrast mode as one observable flag. When <see cref="IsSolid"/>
/// is true, the lava lamp renders a solid Bg1 instead of drifting wax (macOS
/// Reduce Transparency parity). Changes arrive off the UI thread; consumers
/// marshal through their DispatcherQueue.
/// </summary>
public sealed class TransparencyEffects : INotifyPropertyChanged
{
    public static TransparencyEffects Instance { get; } = new();

    private readonly UISettings _settings;
    private readonly AccessibilitySettings? _accessibility;
    private bool _isSolid;

    private TransparencyEffects()
    {
        _settings = new UISettings();
        try
        {
            _accessibility = new AccessibilitySettings();
            _accessibility.HighContrastChanged += (_, _) => Refresh();
        }
        catch (Exception ex)
        {
            Trace.TraceWarning($"TransparencyEffects: AccessibilitySettings unavailable: {ex.Message}");
        }
        _settings.AdvancedEffectsEnabledChanged += (_, _) => Refresh();
        _isSolid = Compute();
    }

    /// <summary>True when transparency effects are off or high contrast is on.</summary>
    public bool IsSolid
    {
        get => _isSolid;
        private set
        {
            if (_isSolid == value) return;
            _isSolid = value;
            RaisePropertyChangedIsolated(nameof(IsSolid));
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private bool Compute()
    {
        bool highContrast = false;
        try { highContrast = _accessibility?.HighContrast ?? false; }
        catch (Exception ex) { Trace.TraceWarning($"TransparencyEffects: HighContrast read failed: {ex.Message}"); }
        return !_settings.AdvancedEffectsEnabled || highContrast;
    }

    private void Refresh() => IsSolid = Compute();

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
                Trace.TraceError($"TransparencyEffects.PropertyChanged subscriber failed: {ex}");
            }
        }
    }
}
