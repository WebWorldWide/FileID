// SidebarQueueList code-behind. Builds rows for each pending job.
// Hidden when the queue is empty.

using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Runtime.CompilerServices;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;
using Windows.UI.Text;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarQueueList : UserControl
{
    private readonly ObservableCollection<QueueRow> _visibleRows = new();
    private readonly Dictionary<(JobCategory Category, string Title, int Occurrence), QueueRow> _rows = new();

    private static readonly SolidColorBrush RunningBackground =
        new(Color.FromArgb(0x14, 0xFF, 0xFF, 0xFF));
    private static readonly SolidColorBrush TransparentBackground =
        new(Colors.Transparent);

    public SidebarQueueList()
    {
        InitializeComponent();
        JobsRepeater.ItemsSource = _visibleRows;
        Loaded += (_, _) =>
        {
            EngineClient.Instance.PropertyChanged -= OnEngineChanged;
            EngineClient.Instance.PropertyChanged += OnEngineChanged;
            Sync();
        };
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
        var state = EngineClient.Instance.QueueState;
        if (state is null || (state.Running is null && state.Pending.Count == 0))
        {
            Root.Visibility = Visibility.Collapsed;
            _visibleRows.Clear();
            _rows.Clear();
            return;
        }
        Root.Visibility = Visibility.Visible;
        TotalEtaText.Text = state.TotalEtaSeconds is { } eta && eta > 0
            ? "≈ " + FormatDuration(eta)
            : string.Empty;

        var desired = new List<((JobCategory Category, string Title, int Occurrence) Key, QueuedJob Job, bool Running)>();
        var occurrences = new Dictionary<(JobCategory Category, string Title), int>();
        void Add(QueuedJob job, bool running)
        {
            var identity = (job.Category, job.Title);
            occurrences.TryGetValue(identity, out var occurrence);
            occurrences[identity] = occurrence + 1;
            desired.Add(((job.Category, job.Title, occurrence), job, running));
        }
        if (state.Running is { } running) Add(running, true);
        foreach (var job in state.Pending) Add(job, false);

        var desiredKeys = desired.Select(item => item.Key).ToHashSet();
        foreach (var stale in _rows.Keys.Where(key => !desiredKeys.Contains(key)).ToArray())
        {
            _rows.Remove(stale);
        }

        for (var index = 0; index < desired.Count; index++)
        {
            var item = desired[index];
            if (!_rows.TryGetValue(item.Key, out var row))
            {
                row = new QueueRow(item.Job, item.Running);
                _rows.Add(item.Key, row);
            }
            else
            {
                row.Update(item.Job, item.Running);
            }

            var currentIndex = _visibleRows.IndexOf(row);
            if (currentIndex < 0)
            {
                _visibleRows.Insert(index, row);
            }
            else if (currentIndex != index)
            {
                _visibleRows.Move(currentIndex, index);
            }
        }
        while (_visibleRows.Count > desired.Count)
        {
            _visibleRows.RemoveAt(_visibleRows.Count - 1);
        }
    }

    public sealed class QueueRow : INotifyPropertyChanged
    {
        private string _iconGlyph = string.Empty;
        private double _iconOpacity;
        private string _title = string.Empty;
        private FontWeight _titleWeight;
        private string _eta = string.Empty;
        private SolidColorBrush _background = TransparentBackground;
        private string _automationName = string.Empty;

        internal QueueRow(QueuedJob job, bool isRunning) => Update(job, isRunning);

        public string IconGlyph { get => _iconGlyph; private set => Set(ref _iconGlyph, value); }
        public double IconOpacity { get => _iconOpacity; private set => Set(ref _iconOpacity, value); }
        public string Title { get => _title; private set => Set(ref _title, value); }
        public FontWeight TitleWeight { get => _titleWeight; private set => Set(ref _titleWeight, value); }
        public string Eta { get => _eta; private set => Set(ref _eta, value); }
        public SolidColorBrush Background { get => _background; private set => Set(ref _background, value); }
        public string AutomationName { get => _automationName; private set => Set(ref _automationName, value); }

        internal void Update(QueuedJob job, bool isRunning)
        {
            IconGlyph = job.Category switch
            {
                JobCategory.Scan => "",
                JobCategory.FaceCluster => "",
                JobCategory.DeepAnalyze => "",
                _ => "",
            };
            IconOpacity = isRunning ? 1.0 : 0.55;
            Title = job.Title;
            TitleWeight = isRunning
                ? Microsoft.UI.Text.FontWeights.SemiBold
                : Microsoft.UI.Text.FontWeights.Normal;
            Eta = job.EtaSeconds is { } seconds && seconds > 0
                ? FormatDuration(seconds)
                : string.Empty;
            Background = isRunning ? RunningBackground : TransparentBackground;
            AutomationName = (isRunning ? "Running: " : "Queued: ") + job.Title
                + (Eta.Length > 0 ? $", {Eta} remaining" : string.Empty);
        }

        public event PropertyChangedEventHandler? PropertyChanged;

        private void Set<T>(ref T field, T value, [CallerMemberName] string? propertyName = null)
        {
            if (EqualityComparer<T>.Default.Equals(field, value)) return;
            field = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
        }
    }

    private static string FormatDuration(double seconds)
    {
        if (seconds < 60) return $"{seconds:F0}s";
        if (seconds < 3600) return $"{seconds / 60:F0}m";
        return $"{seconds / 3600:F1}h";
    }
}
