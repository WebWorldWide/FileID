// ReducedMotion — surfaces the OS "minimize animations" preference as a
// single observable bool that every motion primitive in this library
// subscribes to.
//
// On Windows the preference lives at:
//   Settings → Accessibility → Visual effects → Animation effects (toggle)
// surfaced via Windows.UI.ViewManagement.UISettings.AnimationsEnabled.
//
// We poll on construction + listen to the AnimationsEnabledChanged event,
// so toggles surfaced while the app is open take effect immediately.
//
// Every motion primitive (Shimmer, Ripple, IridescentBorder, LavaLamp,
// springs) checks `ReducedMotion.IsReduced` before kicking off animation.

using System.ComponentModel;
using Windows.UI.ViewManagement;

namespace FileID.Theme.Motion;

public sealed class ReducedMotion : INotifyPropertyChanged
{
    /// <summary>
    /// Process-wide singleton. Subscribe to PropertyChanged + read IsReduced.
    /// </summary>
    public static ReducedMotion Instance { get; } = new();

    private readonly UISettings _settings;
    private bool _isReduced;

    private ReducedMotion()
    {
        _settings = new UISettings();
        _isReduced = !_settings.AnimationsEnabled;
        _settings.AnimationsEnabledChanged += OnAnimationsEnabledChanged;
    }

    /// <summary>
    /// True when the user has asked the OS to minimize animations. All
    /// motion primitives gate on this — Shimmer / IridescentBorder freeze,
    /// CompletionRipple skips the pulse, LavaLamp halves its rate.
    /// </summary>
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
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsReduced)));
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;

    private void OnAnimationsEnabledChanged(UISettings sender, UISettingsAnimationsEnabledChangedEventArgs args)
    {
        // The event fires off the UI thread; consumers that touch UI in
        // their PropertyChanged handler need to marshal back themselves.
        // We deliberately do NOT marshal here so multiple consumers (XAML
        // bindings + view-models) don't pay the dispatcher hop twice.
        IsReduced = !sender.AnimationsEnabled;
    }
}
