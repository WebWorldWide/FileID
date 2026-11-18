// Double passthrough with optional additive offset (via ConverterParameter
// as an invariant-culture double string).

using System;
using System.Globalization;
using Microsoft.UI.Xaml.Data;

namespace FileID.Converters;

public sealed class IdentityDoubleConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        double v = value switch
        {
            double d => d,
            float f => f,
            int i => i,
            _ => double.NaN,
        };
        if (double.IsNaN(v)) return value;
        if (parameter is string s && double.TryParse(s, NumberStyles.Float, CultureInfo.InvariantCulture, out var offset))
        {
            v += offset;
        }
        return v;
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language)
    {
        return value;
    }
}
