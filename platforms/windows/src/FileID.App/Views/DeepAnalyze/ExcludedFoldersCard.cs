using FileID.Services;
using FileID.ViewModels;
using FileID.Theme.Controls;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views.DeepAnalyze;

/// <summary>Describe Files ▸ Folders not to describe (macOS DeepAnalyzeExclusionsCard): folders a
/// whole-library Describe run skips. Their files stay in the library and searchable; describing a
/// single file ignores the list.</summary>
internal sealed class ExcludedFoldersCard : UserControl
{
    private readonly Brush _text2 = ThemeHelper.GetBrushSafe("Text2Brush");
    private readonly Brush _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
    private readonly StackPanel _rows = new() { Spacing = 6 };
    private readonly TextBlock _message = new() { TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
    private readonly Button _add = new() { Content = "Add folder…" };

    public ExcludedFoldersCard()
    {
        var title = new TextBlock { Text = "Folders not to describe", Style = ThemeHelper.GetStyleSafe("SubtitleTextBlockStyle") };
        AutomationProperties.SetHeadingLevel(title, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level2);
        var caption = new TextBlock
        {
            Text = "Describe Files skips these folders when it describes the whole library. Their files still show up and can be searched. Describing a single file ignores this list.",
            TextWrapping = TextWrapping.Wrap,
            Foreground = _text3,
            Style = ThemeHelper.GetStyleSafe("CaptionTextBlockStyle"),
        };
        _message.Foreground = _text2;
        _add.Click += (_, _) => DebugLog.SafeRun("ExcludedFoldersCard.Add", () => _ = AddAsync());

        var stack = new StackPanel { Spacing = 12 };
        stack.Children.Add(title);
        stack.Children.Add(caption);
        stack.Children.Add(_message);
        stack.Children.Add(_rows);
        stack.Children.Add(_add);
        Content = new GlassCard { Content = stack, HorizontalContentAlignment = HorizontalAlignment.Stretch };
        Render();
    }

    private async Task AddAsync()
    {
        var hwnd = App.HostWindow is { } window ? WinRT.Interop.WindowNative.GetWindowHandle(window) : IntPtr.Zero;
        var picked = await FolderPickerService.PickFolderAsync(hwnd, "Exclude");
        if (picked.Path is not { } path)
        {
            if (picked.FailureReason is { } reason) Say(reason);
            return;
        }
        var settings = AppViewModel.Instance.Settings;
        var cleaned = AppSettings.SanitizeExcludedFolders([path]);
        if (cleaned.Count == 0)
        {
            Say("Couldn't skip that folder. Choose a folder on this PC.");
            return;
        }
        if (settings.DeepAnalyzeExcludedFolders.Contains(cleaned[0], StringComparer.OrdinalIgnoreCase))
        {
            Say("That folder is already skipped.");
            return;
        }
        settings.DeepAnalyzeExcludedFolders = AppSettings.SanitizeExcludedFolders([.. settings.DeepAnalyzeExcludedFolders, cleaned[0]]);
        settings.Save();
        Say("Done. Describe Files will skip this folder from the next run.");
        Render();
    }

    private void Remove(string folder)
    {
        var settings = AppViewModel.Instance.Settings;
        settings.DeepAnalyzeExcludedFolders = settings.DeepAnalyzeExcludedFolders
            .Where(f => !string.Equals(f, folder, StringComparison.OrdinalIgnoreCase)).ToList();
        settings.Save();
        Render();
    }

    private void Say(string text)
    {
        _message.Text = text;
        _message.Visibility = Visibility.Visible;
    }

    private void Render()
    {
        _rows.Children.Clear();
        var folders = AppViewModel.Instance.Settings.DeepAnalyzeExcludedFolders;
        if (folders.Count == 0)
        {
            _rows.Children.Add(new TextBlock { Text = "No folders are skipped.", Foreground = _text3 });
            return;
        }
        foreach (var folder in folders)
        {
            var path = new TextBlock
            {
                Text = folder,
                FontFamily = new FontFamily("Consolas"),
                TextTrimming = TextTrimming.CharacterEllipsis,
                VerticalAlignment = VerticalAlignment.Center,
            };
            ToolTipService.SetToolTip(path, folder);
            var remove = new Button { Content = new FontIcon { Glyph = "", FontSize = 12 } };
            AutomationProperties.SetName(remove, $"Describe {folder} again");
            ToolTipService.SetToolTip(remove, "Describe this folder again");
            remove.Click += (_, _) => DebugLog.SafeRun("ExcludedFoldersCard.Remove", () => Remove(folder));
            var row = new Grid { ColumnSpacing = 8 };
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            Grid.SetColumn(remove, 1);
            row.Children.Add(path);
            row.Children.Add(remove);
            _rows.Children.Add(row);
        }
    }
}
