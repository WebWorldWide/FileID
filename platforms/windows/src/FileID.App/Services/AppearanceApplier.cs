using FileID.Theme.Motion;

namespace FileID.Services;

internal static class AppearanceApplier
{
    public static void Apply(AppSettings settings)
    {
        LavaLampMode.Instance.IsOff = !settings.BackgroundMotionOn;
        TextScaler.Instance.Size = settings.TextSize;
    }
}
