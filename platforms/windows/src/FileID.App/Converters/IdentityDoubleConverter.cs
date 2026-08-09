// Double passthrough with optional additive offset (via ConverterParameter
// as an invariant-culture double string). Lets the Library tile force its
// own Height = ActualWidth + caption-row-height so the image area is a
// perfect square and the tile is taller than wide by exactly the caption
// row. Without the offset path, the image area would be Width × (Width −
// caption), close-to-square but not exactly square — captions of slightly
// different heights would still introduce tile-shape variance.
//
// Example:
//   <Grid Height="{Binding ActualWidth, RelativeSource={RelativeSource Self},
//                  Converter={StaticResource IdentityDouble},
//                  ConverterParameter=68}"/>
// computes Height = ActualWidth + 68.

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
