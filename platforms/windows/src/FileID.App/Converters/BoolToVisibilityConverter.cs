// Tiny IValueConverter so XAML can bind a bool → Visibility without
// needing a code-behind triggered each time. WinUI 3 doesn't ship one
// out of the box; this is the smallest workable implementation.

using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace FileID.Converters;

public sealed class BoolToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        bool flag = value switch
        {
            bool b => b,
            null => false,
            _ => true,
        };
        if (parameter is string s && s.Equals("invert", StringComparison.OrdinalIgnoreCase))
        {
            flag = !flag;
        }
        return flag ? Visibility.Visible : Visibility.Collapsed;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        bool result = value is Visibility v && v == Visibility.Visible;
        if (parameter is string s && s.Equals("invert", StringComparison.OrdinalIgnoreCase))
        {
            result = !result;
        }
        return result;
    }
}
