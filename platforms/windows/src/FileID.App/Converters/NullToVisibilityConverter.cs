// Visible when the bound value is NULL, Collapsed when it has a value.
// Used to show shimmer placeholders / loading states only while a
// thumbnail (or other lazily-bound surface) is missing.

using System;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Data;

namespace FileID.Converters;

public sealed class NullToVisibilityConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        bool isNull = value is null;
        if (parameter is string s && s.Equals("invert", StringComparison.OrdinalIgnoreCase))
        {
            isNull = !isNull;
        }
        return isNull ? Visibility.Visible : Visibility.Collapsed;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
        => throw new NotImplementedException();
}
