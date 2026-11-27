using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;

namespace FileID.Views.Library;

public sealed partial class LibraryView
{
    private void OnAskChatSelectedClicked(object sender, RoutedEventArgs e)
        => DebugLog.SafeRun(nameof(OnAskChatSelectedClicked), () =>
        {
            var selected = ViewModel.SelectedItems;
            AppViewModel.Instance.RequestChatScope(ChatSelectionPolicy.Selected(selected.Select(tile => tile.Id), selected.Select(tile => tile.Kind).ToArray()));
        });

    private void SyncAskChat(int count)
    {
        AskChatText.Text = count == 1 ? "Ask about this file" : $"Ask about {count} files";
        AutomationProperties.SetName(AskChatButton, $"Ask Chat about the {count} selected files");
    }
}
