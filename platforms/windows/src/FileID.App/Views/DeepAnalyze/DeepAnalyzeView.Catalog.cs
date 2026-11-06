using System.Collections.Concurrent;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using FileID.Views.ModelBrowser;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.DeepAnalyze;

/// <summary>Describe Files ▸ Browse all models (macOS DeepAnalyze model browser): runnable vision
/// models from the curated set and the pinned catalog, filtered to what fits this PC.</summary>
public sealed partial class DeepAnalyzeView
{
    private static readonly ConcurrentDictionary<string, string> s_catalogNames = new(StringComparer.Ordinal);
    private ContentDialog? _catalogDialog;

    private async void OnBrowseModelsClicked(object sender, RoutedEventArgs e)
    {
        try
        {
            if (_catalogDialog is not null || XamlRoot is null) return;
            var dialog = new ContentDialog
            {
                XamlRoot = XamlRoot,
                Title = "Find a vision model",
                CloseButtonText = "Close",
                DefaultButton = ContentDialogButton.Close,
            };
            dialog.Resources["ContentDialogMaxWidth"] = 920.0;
            var browser = new ModelCatalogBrowser(describeFilesOnly: true, _activeModel, model =>
            {
                RememberCatalogName(model);
                if (SelectModel(model.Id)) dialog.Hide();
            });
            dialog.Content = new ScrollViewer
            {
                Content = browser,
                MaxHeight = 560,
                MinWidth = 640,
                VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            };
            _catalogDialog = dialog;
            try { await dialog.ShowAsync(); }
            finally { _catalogDialog = null; }
            if (!_unloaded) SyncCards();
        }
        catch (Exception ex)
        {
            DebugLog.Warn("Describe Files model browser failed: " + ex.Message);
        }
    }

    private static void RememberCatalogName(ModelCatalogItem model)
    {
        if (ModelCatalogPresentation.IsCatalogModelKind(model.Id)) s_catalogNames[model.Id] = model.Name;
    }

    private void SyncCatalogModelText()
    {
        if (_unloaded) return;
        CatalogModelText.Text = ModelCatalogPresentation.IsCatalogModelKind(_activeModel)
            ? $"Using {ModelDisplayName(_activeModel)} from the catalog."
            : "More vision models from the pinned catalog, filtered to what fits this PC.";
    }

    private void ResolveCatalogModelName()
    {
        var kind = _activeModel;
        if (!ModelCatalogPresentation.IsCatalogModelKind(kind) || s_catalogNames.ContainsKey(kind)) return;
        _ = DebugLog.SafeRunAsync("DeepAnalyzeView.ResolveCatalogModelName", async () =>
        {
            try
            {
                var catalog = await EngineClient.Instance.ListModelsAsync("vision");
                if (catalog.Models.FirstOrDefault(m => m.Id == kind) is not { } model) return;
                RememberCatalogName(model);
                if (!_unloaded && _activeModel == kind)
                {
                    UpdateActiveModelLabel();
                    SyncDeepAnalyzeControls();
                }
            }
            catch (Exception ex)
            {
                DebugLog.Warn("Describe Files: catalog model name unavailable: " + ex.Message);
            }
        });
    }
}
