// GlassCard — translucent acrylic card with a 1px white-8% stroke and rounded
// corners. A templated control so callers can place content directly inside it.
// Where acrylic is unavailable (Win10 22H2) the AcrylicBrush falls back to a flat FallbackColor.

using Microsoft.UI.Xaml.Controls;

namespace FileID.Theme.Controls;

public sealed class GlassCard : ContentControl
{
    public GlassCard()
    {
        DefaultStyleKey = typeof(GlassCard);
    }
}
