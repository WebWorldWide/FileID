using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;

namespace FileID.Services;

internal static class KeyboardFocusGuard
{
    internal static bool IsTextEditing(XamlRoot? xamlRoot)
    {
        if (xamlRoot is null) return false;
        var current = FocusManager.GetFocusedElement(xamlRoot) as DependencyObject;
        while (current is not null)
        {
            if (current is TextBox or RichEditBox or PasswordBox) return true;
            current = VisualTreeHelper.GetParent(current);
        }
        return false;
    }
}
