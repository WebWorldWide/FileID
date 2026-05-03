// DeepAnalyzeView code-behind. Phase 6 cut: model picker UI; Phase 6.x
// wires the IPC `prewarmModel` round-trip + the actual VLM session.

using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.DeepAnalyze;

public sealed partial class DeepAnalyzeView : UserControl
{
    public DeepAnalyzeView()
    {
        InitializeComponent();
    }

    private void OnInstallClicked(object sender, RoutedEventArgs e)
    {
        // Phase 6.x: route Tag (model id) into ModelInstallerService +
        // engine `prewarmModel` IPC. Today we no-op so the button is
        // visible but harmless.
    }
}
