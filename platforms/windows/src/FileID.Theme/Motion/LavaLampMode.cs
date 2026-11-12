using System.ComponentModel;

namespace FileID.Theme.Motion;

/// <summary>Settings ▸ Appearance ▸ Lava lamp. Off freezes the wax in its static glow, as on macOS.</summary>
public sealed class LavaLampMode : INotifyPropertyChanged
{
    public static LavaLampMode Instance { get; } = new();

    private bool _isOff;

    public bool IsOff
    {
        get => _isOff;
        set
        {
            if (_isOff == value) return;
            _isOff = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsOff)));
        }
    }

    public event PropertyChangedEventHandler? PropertyChanged;
}
