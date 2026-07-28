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
// Every motion primitive (Shimmer, LavaLamp, springs) checks
// `ReducedMotion.IsReduced` before kicking off animation.

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
    /// motion primitives gate on this — Shimmer freezes and LavaLamp
    /// halves its rate.
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
            RaiseIsReducedChanged();
        }
    }

    // Subscribers are invoked from OnAnimationsEnabledChanged, which runs on a
    // threadpool thread. A plain multicast Invoke there is a process-kill: one
    // subscriber that throws (a null DispatcherQueue on a torn-down control is
    // the realistic case) escapes as an unhandled exception on a thread with no
    // handler, AND every later subscriber is skipped, silently freezing their
    // motion. Isolate each subscriber so neither can happen.
    private void RaiseIsReducedChanged()
    {
        var handler = PropertyChanged;
        if (handler is null)
        {
            return;
        }
        var args = new PropertyChangedEventArgs(nameof(IsReduced));
        foreach (var target in handler.GetInvocationList())
        {
            try
            {
                ((PropertyChangedEventHandler)target)(this, args);
            }
            catch
            {
                // A motion primitive failing to react to the accessibility
                // toggle must never take the app down or block its siblings.
            }
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
