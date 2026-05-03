// SidebarQueueList code-behind. Builds rows for each pending job.
// Hidden when the queue is empty.

using System.ComponentModel;
using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarQueueList : UserControl
{
    public SidebarQueueList()
    {
        InitializeComponent();
        Loaded += (_, _) => Sync();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is nameof(EngineClient.QueueState))
        {
            DispatcherQueue.TryEnqueue(Sync);
        }
    }

    private void Sync()
    {
        var qs = EngineClient.Instance.QueueState;
        if (qs is null || (qs.Running is null && qs.Pending.Count == 0))
        {
            Root.Visibility = Visibility.Collapsed;
            return;
        }
        Root.Visibility = Visibility.Visible;

        TotalEtaText.Text = qs.TotalEtaSeconds is { } eta && eta > 0
            ? "≈ " + FormatDuration(eta)
            : "";

        var stack = new StackPanel { Spacing = 4 };
        if (qs.Running is { } running)
        {
            stack.Children.Add(BuildRow(running, isRunning: true));
        }
        foreach (var job in qs.Pending)
        {
            stack.Children.Add(BuildRow(job, isRunning: false));
        }
        JobsRepeater.ItemsSource = null;
        // Use a simple StackPanel rather than fighting ItemsRepeater for a
        // tiny list. Replace JobsRepeater's parent's child with the panel.
        if (JobsRepeater.Parent is StackPanel parent)
        {
            int idx = parent.Children.IndexOf(JobsRepeater);
            // Clear any previously built panel after JobsRepeater (a sibling
            // we own).
            for (int i = parent.Children.Count - 1; i > idx; i--)
            {
                parent.Children.RemoveAt(i);
            }
            parent.Children.Add(stack);
        }
    }

    private static UIElement BuildRow(QueuedJob job, bool isRunning)
    {
        var icon = new FontIcon
        {
            FontFamily = new FontFamily("Segoe Fluent Icons"),
            Glyph = job.Category switch
            {
                JobCategory.Scan        => "",
                JobCategory.FaceCluster => "",
                JobCategory.DeepAnalyze => "",
                _                        => "",
            },
            FontSize = 11,
            Opacity = isRunning ? 1.0 : 0.55,
        };
        var title = new TextBlock
        {
            Text = job.Title,
            FontSize = 11,
            FontWeight = isRunning ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal,
            VerticalAlignment = VerticalAlignment.Center,
            TextTrimming = TextTrimming.CharacterEllipsis,
        };
        var eta = new TextBlock
        {
            Text = job.EtaSeconds is { } e && e > 0 ? FormatDuration(e) : "",
            FontSize = 10,
            Opacity = 0.5,
            VerticalAlignment = VerticalAlignment.Center,
        };

        var grid = new Grid
        {
            Padding = new Thickness(8, 6, 8, 6),
            CornerRadius = new CornerRadius(8),
            Background = isRunning
                ? new SolidColorBrush(Color.FromArgb(0x14, 0xFF, 0xFF, 0xFF))
                : new SolidColorBrush(Colors.Transparent),
        };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        Grid.SetColumn(icon, 0);
        icon.Margin = new Thickness(0, 0, 8, 0);
        Grid.SetColumn(title, 1);
        Grid.SetColumn(eta, 2);

        grid.Children.Add(icon);
        grid.Children.Add(title);
        grid.Children.Add(eta);
        return grid;
    }

    private static string FormatDuration(double seconds)
    {
        if (seconds < 60) return $"{seconds:F0}s";
        if (seconds < 3600) return $"{seconds / 60:F0}m";
        return $"{seconds / 3600:F1}h";
    }
}
