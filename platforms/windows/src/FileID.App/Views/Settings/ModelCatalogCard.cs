using FileID.Services;
using FileID.Views.ModelBrowser;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Settings;

/// <summary>Settings ▸ Model catalog: every curated and pinned catalog model for this PC
/// (vision, text, embedding and speech), filtered by memory and graphics memory, with download
/// and on-device benchmarks. The list loads the first time the card is opened.</summary>
public sealed class ModelCatalogCard : UserControl
{
    private readonly Expander _expander;

    public ModelCatalogCard()
    {
        var subtitle = new TextBlock
        {
            Text = "Optional local models, pinned to exact Hugging Face revisions and permissively licensed. Nothing downloads until you choose Download.",
            TextWrapping = TextWrapping.Wrap,
            Foreground = ThemeHelper.GetBrushSafe("Text3Brush"),
        };
        if (ThemeHelper.GetStyleSafe("CaptionTextBlockStyle") is { } caption) subtitle.Style = caption;
        subtitle.Foreground = ThemeHelper.GetBrushSafe("Text3Brush");
        var title = new TextBlock { Text = "Model catalog" };
        if (ThemeHelper.GetStyleSafe("SubtitleTextBlockStyle") is { } style) title.Style = style;
        AutomationProperties.SetHeadingLevel(title, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level2);
        var header = new StackPanel { Spacing = 2, Padding = new Thickness(0, 6, 0, 6) };
        header.Children.Add(title);
        header.Children.Add(subtitle);

        _expander = new Expander
        {
            Header = header,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
        };
        AutomationProperties.SetName(_expander, "Model catalog");
        _expander.Expanding += (_, _) => DebugLog.SafeRun("ModelCatalogCard.Expanding", () =>
        {
            _expander.Content ??= new ModelCatalogBrowser(describeFilesOnly: false);
        });
        Content = _expander;
    }
}
