// RecentScansSheet code-behind. Calls FetchRecentScansAsync on load,
// renders one row per scan_sessions row with a Re-scan button.

using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace FileID.Views.Settings;

public sealed partial class RecentScansSheet : UserControl
{
    public RecentScansSheet()
    {
        InitializeComponent();
        Loaded += async (_, _) =>
        {
            EngineClient.Instance.PropertyChanged += OnEngineChanged;
            try { await EngineClient.Instance.FetchRecentScansAsync(50); } catch { /* swallow */ }
            Render();
        };
        Unloaded += (_, _) => EngineClient.Instance.PropertyChanged -= OnEngineChanged;
    }

    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(EngineClient.LastRecentScans))
        {
            DispatcherQueue.TryEnqueue(Render);
        }
    }

    private void Render()
    {
        var rs = EngineClient.Instance.LastRecentScans;
        if (rs is null || rs.Items.Count == 0)
        {
            HeaderText.Text = "No scans yet — pick a folder + Start Scan from the sidebar.";
            ScansRepeater.ItemsSource = null;
            return;
        }
        HeaderText.Text = $"{rs.Items.Count} past scan{(rs.Items.Count == 1 ? "" : "s")}";
        var rows = new List<UIElement>();
        foreach (var item in rs.Items)
        {
            rows.Add(BuildRow(item));
        }
        ScansRepeater.ItemsSource = rows;
    }

    private FrameworkElement BuildRow(RecentScanItem s)
    {
        var grid = new Grid
        {
            Padding = new Thickness(12, 10, 12, 10),
            CornerRadius = new CornerRadius(10),
            Background = (Brush)Application.Current.Resources["SubtleFillColorSecondaryBrush"],
            ColumnSpacing = 10,
        };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var stack = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        stack.Children.Add(new TextBlock
        {
            Text = s.RootPath,
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
            TextTrimming = TextTrimming.CharacterEllipsis,
        });
        var ts = DateTimeOffset.FromUnixTimeMilliseconds((long)(s.StartedAt * 1000)).LocalDateTime;
        var statusGloss = s.Status switch
        {
            "completed" => $"completed · {s.TotalFiles ?? 0} files",
            "running"   => "still running",
            "cancelled" => "cancelled",
            _ => s.Status,
        };
        stack.Children.Add(new TextBlock
        {
            Text = $"{ts:g} · {statusGloss}",
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            Foreground = (Brush)Application.Current.Resources["TextFillColorSecondaryBrush"],
        });
        Grid.SetColumn(stack, 0);
        grid.Children.Add(stack);

        var revealBtn = new Button { Content = "Open folder" };
        revealBtn.Click += (_, _) =>
        {
            // SEC-9: scan roots are folders, not files — OpenFolder is
            // safe (no execution path). Don't fall through to TryOpenFile,
            // which would reject a folder.
            FileID.Services.SafeOpen.OpenFolder(s.RootPath);
        };
        Grid.SetColumn(revealBtn, 1);
        grid.Children.Add(revealBtn);

        var rescanBtn = new Button
        {
            Content = "Re-scan",
            Style = (Style)Application.Current.Resources["AccentButtonStyle"],
        };
        rescanBtn.Click += async (_, _) =>
        {
            try
            {
                await EngineClient.Instance.StartScanAsync(s.RootPath);
                StatusText.Text = $"Started new scan of {s.RootPath}.";
            }
            catch (Exception ex)
            {
                StatusText.Text = $"Couldn't start scan: {ex.Message}";
            }
        };
        Grid.SetColumn(rescanBtn, 2);
        grid.Children.Add(rescanBtn);
        return grid;
    }
}
