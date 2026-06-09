// SidebarQueueList code-behind. Builds rows for each pending job.
// Hidden when the queue is empty.

using System.ComponentModel;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarQueueList : UserControl
{
    // Stable per-job-rows container, created once and reused. Mutating the
    // parent's Children mid-event-burst (the old rebuild-on-every-QueueState
    // design) races the layout pass and fast-fails the renderer — so only this
    // container's own children ever change.
    private StackPanel? _rowsContainer;

    // Two brushes per BuildRow (running vs idle background) used to be
    // allocated fresh on every QueueState event — 10 Hz × 50 rows = 500
    // SolidColorBrush allocations/sec, each a DispatcherObject. Cache them
    // once on the UI thread at first use and reuse.
    private static readonly SolidColorBrush RunningBackground =
        new(Color.FromArgb(0x14, 0xFF, 0xFF, 0xFF));
    private static readonly SolidColorBrush TransparentBackground =
        new(Colors.Transparent);
    private static readonly FontFamily FluentIconsFont =
        new("Segoe Fluent Icons");

    public SidebarQueueList()
    {
        InitializeComponent();
        Loaded += (_, _) => Sync();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += (_, _) => EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("SidebarQueueList.OnEngineChanged", () =>
        {
            if (e.PropertyName is nameof(EngineClient.QueueState))
            {
                DebugLog.Debug($"[ENGINE-SUB:SidebarQueueList] {e.PropertyName}");
                DispatcherQueue.TryEnqueue(Sync);
            }
        });

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

        // replace the ItemsRepeater with a stable container exactly
        // ONCE (lazy). Subsequent syncs only mutate that container's
        // Children — never the parent panel — so the renderer never sees
        // a sibling list change. See _rowsContainer field comment.
        if (_rowsContainer is null)
        {
            _rowsContainer = new StackPanel { Spacing = 4 };
            if (JobsRepeater.Parent is StackPanel parent)
            {
                int idx = parent.Children.IndexOf(JobsRepeater);
                // Remove the unused ItemsRepeater + any leftover panels
                // from earlier imperative-rebuild paths.
                parent.Children.Remove(JobsRepeater);
                while (parent.Children.Count > idx)
                {
                    parent.Children.RemoveAt(idx);
                }
                parent.Children.Add(_rowsContainer);
            }
        }

        // Off-tree build, then in-place swap. WinUI 3 tolerates Children
        // mutation on a panel that's not currently being measured; the
        // single Clear+AddRange is one Reset notification rather than N.
        _rowsContainer.Children.Clear();
        if (qs.Running is { } running)
        {
            _rowsContainer.Children.Add(BuildRow(running, isRunning: true));
        }
        foreach (var job in qs.Pending)
        {
            _rowsContainer.Children.Add(BuildRow(job, isRunning: false));
        }
    }

    private static UIElement BuildRow(QueuedJob job, bool isRunning)
    {
        var icon = new FontIcon
        {
            FontFamily = FluentIconsFont,
            Glyph = job.Category switch
            {
                JobCategory.Scan => "",
                JobCategory.FaceCluster => "",
                JobCategory.DeepAnalyze => "",
                _ => "",
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
            Background = isRunning ? RunningBackground : TransparentBackground,
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

        // Screen-reader name for the queue row: running/queued + title + ETA,
        // so each pending job announces as one coherent line.
        var name = (isRunning ? "Running: " : "Queued: ") + job.Title;
        if (eta.Text.Length > 0) name += $", {eta.Text} remaining";
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(grid, name);
        return grid;
    }

    private static string FormatDuration(double seconds)
    {
        if (seconds < 60) return $"{seconds:F0}s";
        if (seconds < 3600) return $"{seconds / 60:F0}m";
        return $"{seconds / 3600:F1}h";
    }
}
