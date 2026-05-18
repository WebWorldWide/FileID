// GlassCard — translucent acrylic card with a 1px white-8% stroke and
// rounded corners.
//
// Templated control (not UserControl) so callers can drop arbitrary content
// inside without going through ContentTemplate ceremony:
//
//     <theme:GlassCard>
//         <StackPanel> ... </StackPanel>
//     </theme:GlassCard>
//
// The acrylic surface is rendered by an AcrylicBrush in the template; on
// Win10 22H2 (no Mica) the AcrylicBrush gracefully falls back to a flat
// FallbackColor.

using Microsoft.UI.Xaml.Controls;

namespace FileID.Theme.Controls;

public sealed class GlassCard : ContentControl
{
    public GlassCard()
    {
        DefaultStyleKey = typeof(GlassCard);
    }
}
