// WorkflowAutoTabRouter — auto-switches the active sidebar tab when
// pipeline milestones complete. Matches macOS MainWindow.swift:95-109's
// two `.task(id:)` watchers:
//   1. Face clustering finishes → switch to People (if user was on Library)
//   2. Deep Analyze finishes    → switch to Library (if user was on Deep Analyze)
//
// Guarded so we don't override the user's manual navigation: only flip if
// the active tab is the one we'd EXPECT them to be on for that workflow.
// If they've already moved somewhere else, leave them alone.

using System.ComponentModel;
using FileID.ViewModels;

namespace FileID.Services;

internal static class WorkflowAutoTabRouter
{
    public static void Hook()
    {
        EngineClient.Instance.PropertyChanged += OnEngineChanged;
    }

    private static void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        try
        {
            if (e.PropertyName == nameof(EngineClient.LastFaceClustering))
            {
                if (EngineClient.Instance.LastFaceClustering is null) return;
                var active = AppViewModel.Instance.ActiveTab;
                if (active.Id == SidebarTab.Library.Id)
                {
                    DebugLog.Info("[AUTOTAB] face clustering complete; Library → People");
                    AppViewModel.Instance.ActiveTab = SidebarTab.People;
                }
            }
            else if (e.PropertyName == nameof(EngineClient.DeepAnalyzeComplete))
            {
                if (EngineClient.Instance.DeepAnalyzeComplete is null) return;
                var active = AppViewModel.Instance.ActiveTab;
                if (active.Id == SidebarTab.DeepAnalyze.Id)
                {
                    DebugLog.Info("[AUTOTAB] Deep Analyze complete; DeepAnalyze → Library");
                    AppViewModel.Instance.ActiveTab = SidebarTab.Library;
                }
            }
        }
        catch (Exception ex)
        {
            DebugLog.Warn("[AUTOTAB] router threw: " + ex.Message);
        }
    }
}
