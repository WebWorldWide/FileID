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
using Microsoft.UI.Text;
using Windows.UI;

namespace FileID.Views.Sidebar;

public sealed partial class SidebarQueueList : UserControl
{
    private readonly ObservableCollection<QueueRow> _visibleRows = new();
    private static readonly SolidColorBrush RunningBackground =
        new(Color.FromArgb(0x14, 0xFF, 0xFF, 0xFF));
    private static readonly SolidColorBrush TransparentBackground = new(Colors.Transparent);
    private static readonly FontFamily FluentIconsFont = new("Segoe Fluent Icons");

    public SidebarQueueList()
    {
        InitializeComponent();
        JobsRepeater.ItemsSource = _visibleRows;
        Loaded += (_, _) => Sync();
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
        Unloaded += (_, _) => EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
        => DebugLog.SafeRun("SidebarQueueList.OnEngineChanged", () =>
        {
            if (e.PropertyName != nameof(EngineClient.QueueState)) return;
            DebugLog.Debug($"[ENGINE-SUB:SidebarQueueList] {e.PropertyName}");
            DispatcherQueue.TryEnqueue(Sync);
        });

    private void Sync()
    {
        var state = EngineClient.Instance.QueueState;
        var desired = new List<(QueuedJob Job, bool Running)>();
        if (state?.Running is { } running) desired.Add((running, true));
        if (state is not null) desired.AddRange(state.Pending.Select(job => (job, false)));

        Root.Visibility = desired.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        TotalEtaText.Text = state?.TotalEtaSeconds is { } eta && eta > 0
            ? "≈ " + FormatDuration(eta)
            : string.Empty;

        for (var index = 0; index < desired.Count; index++)
        {
            var (job, isRunning) = desired[index];
            var currentIndex = -1;
            for (var candidate = index; candidate < _visibleRows.Count; candidate++)
            {
                if (_visibleRows[candidate].Id == job.Id)
                {
                    currentIndex = candidate;
                    break;
                }
            }

            if (currentIndex < 0)
            {
                _visibleRows.Insert(index, new QueueRow(job, isRunning));
                continue;
            }

            var row = _visibleRows[currentIndex];
            row.Update(job, isRunning);
            if (currentIndex != index) _visibleRows.Move(currentIndex, index);
        }

        while (_visibleRows.Count > desired.Count)
            _visibleRows.RemoveAt(_visibleRows.Count - 1);
    }

    private static string FormatDuration(double seconds)
    {
        if (seconds < 60) return $"{seconds:F0}s";
        if (seconds < 3600) return $"{seconds / 60:F0}m";
        return $"{seconds / 3600:F1}h";
    }

    private sealed class QueueRow : INotifyPropertyChanged
    {
        private string _title = string.Empty;
        private string _eta = string.Empty;
        private bool _isRunning;

        public QueueRow(QueuedJob job, bool isRunning)
        {
            Id = job.Id;
            Update(job, isRunning);
        }

        public string Id { get; }
        public string Title => _title;
        public string Eta => _eta;
        public string Glyph => string.Empty;
        public double IconOpacity => _isRunning ? 1.0 : 0.55;
        public Windows.UI.Text.FontWeight TitleWeight => _isRunning
            ? Microsoft.UI.Text.FontWeights.SemiBold
            : Microsoft.UI.Text.FontWeights.Normal;
        public Brush RowBackground => _isRunning ? RunningBackground : TransparentBackground;
        public string AutomationName => (_isRunning ? "Running: " : "Queued: ") + _title
            + (_eta.Length == 0 ? string.Empty : $", {_eta} remaining");

        public event PropertyChangedEventHandler? PropertyChanged;

        public void Update(QueuedJob job, bool isRunning)
        {
            if (job.Id != Id) throw new InvalidOperationException("Queue row identity cannot change.");
            _title = job.Title;
            _eta = job.EtaSeconds is { } seconds && seconds > 0 ? FormatDuration(seconds) : string.Empty;
            _isRunning = isRunning;
            OnPropertyChanged(string.Empty);
        }

        private void OnPropertyChanged([CallerMemberName] string? propertyName = null)
            => PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(propertyName));
    }
}
