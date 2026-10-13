using FileID.Theme.Motion;
using FileID.Views.Workbenches;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Hosting;

namespace FileID;

public sealed partial class MainWindow
{
    private ToolsWorkbench? _toolsWorkbench;

    private void OnCatalogWorkbenchClicked(object sender, RoutedEventArgs e) => OpenWorkbench(new CatalogWorkbench());
    private void OnToolsWorkbenchClicked(object sender, RoutedEventArgs e) =>
        OpenWorkbench(_toolsWorkbench ??= new ToolsWorkbench(WinRT.Interop.WindowNative.GetWindowHandle(this)));
    private void OnChatWorkbenchClicked(object sender, RoutedEventArgs e) => OpenWorkbench(new ChatWorkbench());
    private void OnModelsWorkbenchClicked(object sender, RoutedEventArgs e) => OpenWorkbench(new ModelsWorkbench());
    private void OnCloseWorkbenchClicked(object sender, RoutedEventArgs e)
    {
        WorkbenchHost.Content = null;
        WorkbenchHost.Visibility = Visibility.Collapsed;
        CloseWorkbenchButton.Visibility = Visibility.Collapsed;
        PrimaryDetailView.Visibility = Visibility.Visible;
    }
    private void OpenWorkbench(UserControl panel)
    {
        PrimaryDetailView.Visibility = Visibility.Collapsed;
        WorkbenchHost.Visibility = Visibility.Visible;
        CloseWorkbenchButton.Visibility = Visibility.Visible;
        WorkbenchHost.Content = panel;
        if (!ReducedMotion.Instance.IsReduced)
        {
            ElementCompositionPreview.GetElementVisual(WorkbenchHost).Opacity = 0.65f;
            SpringEasing.AnimateScalar(WorkbenchHost, "Opacity", 1, 0.4, 0.8);
        }
    }
}
