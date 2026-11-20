// Sidebar code-behind. The composition root has no logic of its own: each subcomponent owns its piece of state.

using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Sidebar;

public sealed partial class Sidebar : UserControl
{
    public Sidebar()
    {
        InitializeComponent();
    }
}
