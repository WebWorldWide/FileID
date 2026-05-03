// SuggestedMergesSheet code-behind. Subscribes to EngineClient's
// LastMergeSuggestions, builds one row per pair with side-by-side anchor
// face JPEGs + similarity % + action buttons. Merge fires mergeClusters
// IPC; Different-people writes a face_verifications row so we don't keep
// re-suggesting it.

using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.ComponentModel;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.Data.Sqlite;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.People;

public sealed partial class SuggestedMergesSheet : UserControl
{
    private readonly ObservableCollection<UIElement> _rows = new();

    public SuggestedMergesSheet()
    {
        InitializeComponent();
        PairRepeater.ItemsSource = _rows;
        Loaded += (_, _) =>
        {
            EngineClient.Instance.PropertyChanged += OnEngineChanged;
            // Trigger a fresh suggestion fetch whenever the sheet opens.
            _ = EngineClient.Instance.FindMergeSuggestionsAsync();
            HeaderText.Text = "Looking for similar clusters…";
        };
        Unloaded += (_, _) =>
        {
            EngineClient.Instance.PropertyChanged -= OnEngineChanged;
        };
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName != nameof(EngineClient.LastMergeSuggestions)) return;
        DispatcherQueue.TryEnqueue(Render);
    }

    private void Render()
    {
        var sug = EngineClient.Instance.LastMergeSuggestions;
        _rows.Clear();
        if (sug is null || sug.Pairs.Count == 0)
        {
            HeaderText.Text = "No likely merges found. (Try after a fresh scan + re-cluster.)";
            return;
        }
        HeaderText.Text = $"{sug.Pairs.Count} candidate pair{(sug.Pairs.Count == 1 ? "" : "s")} — review each.";
        foreach (var p in sug.Pairs)
        {
            _rows.Add(BuildRow(p));
        }
    }

    private UIElement BuildRow(MergeSuggestion p)
    {
        var grid = new Grid
        {
            Padding = new Thickness(12, 10, 12, 10),
            CornerRadius = new CornerRadius(10),
            Background = new SolidColorBrush(Color.FromArgb(0x33, 0x14, 0x14, 0x14)),
        };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var imgA = BuildFaceImage(p.SourceAnchorFaceId);
        var imgB = BuildFaceImage(p.DestinationAnchorFaceId);
        Grid.SetColumn(imgA, 0);
        Grid.SetColumn(imgB, 1);
        grid.Children.Add(imgA);
        grid.Children.Add(imgB);

        var info = new StackPanel { Margin = new Thickness(14, 0, 14, 0), VerticalAlignment = VerticalAlignment.Center };
        info.Children.Add(new TextBlock
        {
            Text = $"#{p.SourcePersonId} ({p.SourceMemberCount}) ↔ #{p.DestinationPersonId} ({p.DestinationMemberCount})",
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
        });
        info.Children.Add(new TextBlock
        {
            Text = $"Similarity {p.Similarity:F2}",
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
        });
        Grid.SetColumn(info, 2);
        grid.Children.Add(info);

        var differentBtn = new Button
        {
            Content = "Different people",
            Margin = new Thickness(0, 0, 8, 0),
        };
        differentBtn.Click += async (_, _) => await MarkDifferentAsync(p);
        Grid.SetColumn(differentBtn, 3);
        grid.Children.Add(differentBtn);

        var mergeBtn = new Button
        {
            Content = "Merge",
            Style = (Style)Application.Current.Resources["AccentButtonStyle"],
        };
        mergeBtn.Click += async (_, _) => await MergeAsync(p, grid);
        Grid.SetColumn(mergeBtn, 4);
        grid.Children.Add(mergeBtn);

        return grid;
    }

    private static Border BuildFaceImage(long faceId)
    {
        var border = new Border
        {
            Width = 80,
            Height = 80,
            CornerRadius = new CornerRadius(80),
            Background = (Brush)Application.Current.Resources["SubtleFillColorTertiaryBrush"],
        };
        var path = Path.Combine(AppPaths.Root, "face_crops", $"{faceId}.jpg");
        if (File.Exists(path))
        {
            border.Child = new Image
            {
                Source = new Microsoft.UI.Xaml.Media.Imaging.BitmapImage(new Uri(path)),
                Stretch = Stretch.UniformToFill,
                Width = 80,
                Height = 80,
            };
        }
        return border;
    }

    private async Task MergeAsync(MergeSuggestion p, FrameworkElement row)
    {
        try
        {
            await EngineClient.Instance.MergeClustersAsync(p.SourcePersonId, p.DestinationPersonId);
            row.Opacity = 0.4;
            row.IsHitTestVisible = false;
            StatusText.Text = $"Merged #{p.SourcePersonId} into #{p.DestinationPersonId}.";
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Merge failed: {ex.Message}";
        }
    }

    private async Task MarkDifferentAsync(MergeSuggestion p)
    {
        // Persist into face_verifications so findMergeSuggestions never
        // re-suggests this pair. Direct DB write via Microsoft.Data.Sqlite
        // — tiny + idempotent, no need for an IPC roundtrip.
        try
        {
            var connStr = new SqliteConnectionStringBuilder
            {
                DataSource = AppPaths.DbPath,
                Mode = SqliteOpenMode.ReadWrite,
            }.ToString();
            using var conn = new SqliteConnection(connStr);
            await conn.OpenAsync();
            using var cmd = conn.CreateCommand();
            cmd.CommandText = """
                INSERT OR REPLACE INTO face_verifications
                  (person_a, person_b, same_person, confidence, vlm_model, verified_at)
                VALUES (@a, @b, 0, 1.0, 'user-verified', @ts)
                """;
            var (a, b) = p.SourcePersonId < p.DestinationPersonId
                ? (p.SourcePersonId, p.DestinationPersonId)
                : (p.DestinationPersonId, p.SourcePersonId);
            cmd.Parameters.AddWithValue("@a", a);
            cmd.Parameters.AddWithValue("@b", b);
            cmd.Parameters.AddWithValue("@ts", DateTimeOffset.UtcNow.ToUnixTimeSeconds());
            await cmd.ExecuteNonQueryAsync();
            StatusText.Text = $"Marked #{p.SourcePersonId} ↔ #{p.DestinationPersonId} as different people.";
        }
        catch (Exception ex)
        {
            StatusText.Text = $"Couldn't save: {ex.Message}";
        }
    }
}
