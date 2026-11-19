// WorkflowAutoTabRouter — auto-switches the active sidebar tab when pipeline milestones complete (face clustering → People, Deep Analyze → Library), only when the user is still on the tab we'd expect for that workflow, so manual navigation is never overridden.

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
        => DebugLog.SafeRun("WorkflowAutoTabRouter.OnEngineChanged", () =>
        {
            if (e.PropertyName == nameof(EngineClient.LastFaceClustering))
            {
                if (EngineClient.Instance.LastFaceClustering is null) return;
                DebugLog.Info($"[ENGINE-SUB:WorkflowAutoTabRouter] {e.PropertyName}");
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
                DebugLog.Info($"[ENGINE-SUB:WorkflowAutoTabRouter] {e.PropertyName}");
                var active = AppViewModel.Instance.ActiveTab;
                if (active.Id == SidebarTab.DeepAnalyze.Id)
                {
                    DebugLog.Info("[AUTOTAB] Deep Analyze complete; DeepAnalyze → Library");
                    AppViewModel.Instance.ActiveTab = SidebarTab.Library;
                }
            }
        });
}
