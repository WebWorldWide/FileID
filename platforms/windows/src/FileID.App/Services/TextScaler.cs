using System.Runtime.CompilerServices;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace FileID.Services;

/// <summary>Settings ▸ Appearance ▸ Text size, as on macOS `TextSize`: scales page text (file names,
/// details, descriptions, chat) in half-point steps; Default leaves every size untouched. The title bar
/// and sidebar keep their designed size because only the page host is attached.</summary>
internal sealed class TextScaler
{
    public static TextScaler Instance { get; } = new();

    private sealed class Entry
    {
        public double Base;
        public double Applied;
    }

    private readonly ConditionalWeakTable<DependencyObject, Entry> _entries = new();
    private readonly List<WeakReference<FrameworkElement>> _roots = [];
    private DispatcherQueueTimer? _debounce;
    private string _size = "standard";

    public static double FactorFor(string size) => size switch
    {
        "small" => 0.9,
        "large" => 1.15,
        "larger" => 1.3,
        _ => 1,
    };

    public static double Scaled(double size, string textSize) =>
        textSize is "standard" ? size : Math.Round(size * FactorFor(textSize) * 2, MidpointRounding.AwayFromZero) / 2;

    public string Size
    {
        get => _size;
        set
        {
            if (_size == value) return;
            _size = value;
            foreach (var root in LiveRoots()) Walk(root);
        }
    }

    /// <summary>Scales everything under <paramref name="root"/> now and again after each layout pass that adds new text.</summary>
    public void Attach(FrameworkElement root)
    {
        _roots.Add(new WeakReference<FrameworkElement>(root));
        root.LayoutUpdated += (_, _) => Schedule(root);
        Walk(root);
    }

    private IEnumerable<FrameworkElement> LiveRoots()
    {
        _roots.RemoveAll(r => !r.TryGetTarget(out _));
        return _roots.Select(r => r.TryGetTarget(out var t) ? t : null).OfType<FrameworkElement>().ToArray();
    }

    private void Schedule(FrameworkElement root)
    {
        if (_size == "standard" && !_hasScaled) return;
        _debounce ??= root.DispatcherQueue.CreateTimer();
        _debounce.Interval = TimeSpan.FromMilliseconds(150);
        _debounce.IsRepeating = false;
        _debounce.Tick -= OnDebounce;
        _debounce.Tick += OnDebounce;
        _debounce.Start();
    }

    private bool _hasScaled;

    private void OnDebounce(DispatcherQueueTimer sender, object args)
    {
        sender.Stop();
        foreach (var root in LiveRoots()) Walk(root);
    }

    private void Walk(DependencyObject node)
    {
        Visit(node);
        var count = VisualTreeHelper.GetChildrenCount(node);
        for (var i = 0; i < count; i++) Walk(VisualTreeHelper.GetChild(node, i));
    }

    private void Visit(DependencyObject node)
    {
        switch (node)
        {
            case TextBlock text:
                Scale(text, text.FontSize, v => text.FontSize = v);
                break;
            case RichTextBlock rich:
                Scale(rich, rich.FontSize, v => rich.FontSize = v);
                break;
            case Control control:
                Scale(control, control.FontSize, v => control.FontSize = v);
                break;
        }
    }

    private void Scale(DependencyObject element, double current, Action<double> set)
    {
        if (!_entries.TryGetValue(element, out var entry))
        {
            if (_size == "standard") return;
            if (InheritsScaledSize(element, current)) return;
            entry = new Entry { Base = current, Applied = current };
            _entries.Add(element, entry);
        }
        else if (Math.Abs(current - entry.Applied) > 0.01)
        {
            entry.Base = current;
        }

        var target = Scaled(entry.Base, _size);
        if (Math.Abs(target - current) > 0.01)
        {
            entry.Applied = target;
            set(target);
            if (_size != "standard") _hasScaled = true;
        }
        else
        {
            entry.Applied = target;
        }
    }

    private bool InheritsScaledSize(DependencyObject element, double current)
    {
        var parent = VisualTreeHelper.GetParent(element);
        while (parent is not null)
        {
            if (parent is Control or TextBlock or RichTextBlock)
            {
                var size = parent switch
                {
                    Control c => c.FontSize,
                    TextBlock t => t.FontSize,
                    RichTextBlock r => r.FontSize,
                    _ => current,
                };
                return _entries.TryGetValue(parent, out var e) && Math.Abs(size - e.Applied) < 0.01
                    && Math.Abs(current - e.Applied) < 0.01 && element.ReadLocalValue(FontSizeProperty(element)) == DependencyProperty.UnsetValue;
            }
            parent = VisualTreeHelper.GetParent(parent);
        }
        return false;
    }

    private static DependencyProperty FontSizeProperty(DependencyObject element) => element switch
    {
        TextBlock => TextBlock.FontSizeProperty,
        RichTextBlock => RichTextBlock.FontSizeProperty,
        _ => Control.FontSizeProperty,
    };
}
