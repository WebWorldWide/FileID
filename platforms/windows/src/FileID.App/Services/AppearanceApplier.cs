using FileID.Theme.Motion;

namespace FileID.Services;

/// <summary>Pushes Settings ▸ Appearance (lava lamp, text size) into the live UI.</summary>
internal static class AppearanceApplier
{
    public static void Apply(AppSettings settings)
    {
        LavaLampMode.Instance.IsOff = !settings.BackgroundMotionOn;
        TextScaler.Instance.Size = settings.TextSize;
    }
}
