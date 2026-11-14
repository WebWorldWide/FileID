using FileID.IpcSchema;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Workbenches;

/// <summary>Best Takes "Goal (optional)" panel (macOS TakeReview.goalPanel): the description model looks at
/// all the tries together (rankTakesWithGoal) and the top try becomes the pick. Photos only.</summary>
internal sealed partial class TakesWorkbench
{
    private readonly TextBox _rankGoalBox = new() { PlaceholderText = TakeReviewPolicy.GoalPlaceholder, MaxLength = 500, MinHeight = 28 };
    private readonly Button _goalRank = Secondary(new Button { Content = "Rank with AI", Height = 28, MinHeight = 28, IsEnabled = false });
    private readonly ProgressBar _goalBar = new() { IsIndeterminate = true, Width = 60, VerticalAlignment = VerticalAlignment.Center, Visibility = Visibility.Collapsed };
    private readonly TextBlock _goalSummary = Meta(TakeReviewPolicy.GoalIdleHint);
    private StackPanel? _goalPanel;
    private bool _goalRanking;

    private StackPanel BuildGoalPanel()
    {
        var header = new Grid { ColumnSpacing = 10 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var title = SectionLabel("Goal (optional)");
        title.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(_goalBar, 1);
        header.Children.Add(title);
        header.Children.Add(_goalBar);

        _goalRank.Foreground = _ai;
        _goalRank.BorderBrush = _ai;
        _goalRank.Background = _aiWash;
        ToolTipService.SetToolTip(_goalRank, TakeReviewPolicy.GoalHelp);
        AutomationProperties.SetName(_rankGoalBox, "Goal");
        _goalSummary.FontSize = 12;
        _rankGoalBox.TextChanged += (_, _) => UpdateGoalControls();
        _rankGoalBox.KeyDown += (_, e) =>
        {
            if (e.Key != Windows.System.VirtualKey.Enter) return;
            e.Handled = true;
            RankWithGoal();
        };
        _goalRank.Click += (_, _) => RankWithGoal();

        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        Grid.SetColumn(_goalRank, 1);
        row.Children.Add(_rankGoalBox);
        row.Children.Add(_goalRank);

        _goalPanel = new StackPanel { Spacing = 10, Visibility = Visibility.Collapsed };
        _goalPanel.Children.Add(header);
        _goalPanel.Children.Add(row);
        _goalPanel.Children.Add(_goalSummary);
        return _goalPanel;
    }

    private void ResetGoal()
    {
        _goalRanking = false;
        _goalBar.Visibility = Visibility.Collapsed;
        _goalSummary.Text = TakeReviewPolicy.GoalIdleHint;
        _goalSummary.Foreground = _text3;
        UpdateGoalControls();
    }

    private void UpdateGoalControls()
    {
        _goalRank.IsEnabled = !_goalRanking && TakeReviewPolicy.CanRank(_rankGoalBox.Text, _qualities);
        _goalBar.Visibility = _goalRanking ? Visibility.Visible : Visibility.Collapsed;
        if (_goalPanel is not null)
        {
            _goalPanel.Visibility = _qualities.Count > 0 && _current is { } group && group.Kind != "videos" ? Visibility.Visible : Visibility.Collapsed;
        }
    }

    private void RankWithGoal()
    {
        var goal = _rankGoalBox.Text.Trim();
        if (_goalRanking || _current is not { } group || !TakeReviewPolicy.CanRank(goal, _qualities)) return;
        var key = group.GroupKey;
        var order = TakeReviewPolicy.GoalOrder(_qualities);
        _goalRanking = true;
        UpdateGoalControls();
        Run(async ct =>
        {
            var request = Request("rankTakesWithGoal", order) with { Query = goal };
            var response = await Engine.CatalogAsync(request, ct);
            await UI(() =>
            {
                _goalRanking = false;
                if (_current?.GroupKey != key)
                {
                    UpdateGoalControls();
                    return;
                }
                if (response.Status != "ok" || response.TakeGoalRanking is not { } result)
                {
                    UpdateGoalControls();
                    ShowToast(response.Message ?? TakeReviewPolicy.GoalFailed, null, PlainToastWindow);
                    return;
                }
                _goalSummary.Text = result.Summary;
                _goalSummary.Foreground = result.Status == "ranked" ? _text2 : _text3;
                _qualities = TakeReviewPolicy.MarkGoal(_qualities, result);
                if (TakeReviewPolicy.AdoptGoal(_qualities, _keep, result) is { } adopted)
                {
                    _keep.Clear();
                    foreach (var id in adopted.Keep) _keep.Add(id);
                    _focusedIndex = adopted.Focus;
                }
                RebuildStrip();
                UpdateAll();
                UpdateGoalControls();
                ScrollToFocused();
            }, ct);
        });
    }
}
