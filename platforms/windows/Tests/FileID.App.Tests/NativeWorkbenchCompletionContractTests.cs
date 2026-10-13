using Xunit;

namespace FileID.App.Tests;

public sealed class NativeWorkbenchCompletionContractTests
{
    [Fact]
    public void StopControlsRemainOutsideDisabledInputsAndOnlySendCanonicalCancel()
    {
        var panel = ReadSource("Views/Workbenches/WorkbenchPanel.cs");
        Assert.Contains("Body.Children.Add(button)", panel, StringComparison.Ordinal);
        var chat = ReadSource("Views/Workbenches/ChatWorkbench.cs");
        Assert.Contains("ControlButton(\"Stop response\", StopAsync)", chat, StringComparison.Ordinal);
        Assert.Contains("UseModel: action == \"send\" && ChatFrontendPolicy.UseModelForSend(_useModel.IsChecked == true)", chat, StringComparison.Ordinal);
        Assert.Contains("IsChecked = false, IsEnabled = ChatFrontendPolicy.SupportsModelGeneration", chat, StringComparison.Ordinal);
        Assert.Contains("_stop.IsEnabled = ChatFrontendPolicy.SupportsCancellation &&", chat, StringComparison.Ordinal);
        var chatStop = chat[chat.IndexOf("private async Task StopAsync", StringComparison.Ordinal)..chat.IndexOf("private void Apply", StringComparison.Ordinal)];
        Assert.True(chatStop.IndexOf("if (!ChatFrontendPolicy.SupportsCancellation)", StringComparison.Ordinal) < chatStop.IndexOf("SynchronizeSend();", StringComparison.Ordinal));
        Assert.Contains("Engine.PropertyChanged -= OnEngineChanged", chat, StringComparison.Ordinal);
        Assert.Contains("_request.Apply(response, generation)", chat, StringComparison.Ordinal);
        Assert.DoesNotContain("File.WriteAll", chat, StringComparison.Ordinal);
        var tools = ReadSource("Views/Workbenches/ToolsWorkbench.cs");
        Assert.Contains("ControlButton(\"Cancel current export\", CancelAsync)", tools, StringComparison.Ordinal);
        Assert.Contains("RequireCancellationOwner(generation)", tools, StringComparison.Ordinal);
        Assert.Contains("\"cancel\", OperationID: id", tools, StringComparison.Ordinal);
        Assert.Contains("cancellation acknowledgement is not a completion receipt", tools, StringComparison.Ordinal);
        Assert.Contains("ToolExecutionState.SupportsCancellation(response.Capabilities)", tools, StringComparison.Ordinal);
        Assert.Contains("capability.Id != \"cancel\"", tools, StringComparison.Ordinal);
        Assert.DoesNotContain("_execution.Complete(response.OperationID!", tools, StringComparison.Ordinal);
        var cancelHandler = tools[tools.IndexOf("private async Task CancelAsync", StringComparison.Ordinal)..tools.IndexOf("private async Task SendAsync", StringComparison.Ordinal)];
        Assert.DoesNotContain("_execution.Complete", cancelHandler, StringComparison.Ordinal);
        Assert.DoesNotContain("_plan.Apply", cancelHandler, StringComparison.Ordinal);
    }

    [Fact]
    public void ExecuteOwnershipIsClientOwnedBeyondPanelLifetimeAndUsesItsOwnTerminal()
    {
        var engine = ReadSource("ViewModels/EngineClient.Workbenches.cs");
        Assert.Contains("public ToolExecutionState ToolExecution { get; } = new()", engine, StringComparison.Ordinal);
        Assert.Contains("ToolExecution.ApplyTerminal(tool.Response, generation)", engine, StringComparison.Ordinal);
        var execute = engine[engine.IndexOf("private async Task<ToolResponse> SendToolExecuteAsync", StringComparison.Ordinal)..engine.IndexOf("public async Task<ChatResponse> ChatAsync", StringComparison.Ordinal)];
        Assert.DoesNotContain("_workbenchRequests.Remove", execute, StringComparison.Ordinal);
        Assert.Contains("completion.WaitAsync(deadline.Token)", execute, StringComparison.Ordinal);
        var tools = ReadSource("Views/Workbenches/ToolsWorkbench.cs");
        Assert.Contains("_execution = Engine.ToolExecution", tools, StringComparison.Ordinal);
        Assert.Contains("OnClosed() => Engine.PropertyChanged -= OnEngineChanged", tools, StringComparison.Ordinal);
        Assert.Contains("ApplyExecutionSnapshot", tools, StringComparison.Ordinal);
        Assert.DoesNotContain("InvalidateGeneration", tools, StringComparison.Ordinal);
        Assert.Contains("ToolExecution.InvalidateGeneration(retiringGeneration)", ReadSource("ViewModels/EngineClient.cs"), StringComparison.Ordinal);
    }

    [Fact]
    public void DispatchAndChatSendObserversSurviveOnlyUiAwaitCancellation()
    {
        var source = ReadSource("ViewModels/EngineClient.Workbenches.cs");
        Assert.Contains("WorkbenchDispatch.ObserveAsync", source, StringComparison.Ordinal);
        Assert.Contains("ToolExecution.ConfirmUnsent(request.RequestID, operationID, generation, error)", source, StringComparison.Ordinal);
        Assert.Contains("ChatSend.Apply(chat.Response, generation)", source, StringComparison.Ordinal);
        var chatSend = source[source.IndexOf("public async Task<ChatResponse> ChatAsync", StringComparison.Ordinal)..source.IndexOf("public async Task<CatalogResponse> CatalogSearchAsync", StringComparison.Ordinal)];
        Assert.DoesNotContain("_workbenchRequests.Remove", chatSend, StringComparison.Ordinal);
        var view = ReadSource("Views/Workbenches/ChatWorkbench.cs");
        Assert.Contains("action == \"history\" && _request.HasActiveSend", view, StringComparison.Ordinal);
        Assert.Contains("Send remains unconfirmed", view, StringComparison.Ordinal);
        Assert.DoesNotContain("ChatSend.InvalidateGeneration", view, StringComparison.Ordinal);
    }

    [Fact]
    public void ChatAcknowledgementDoesNotRenderOrCancelSendObservation()
    {
        var source = ReadSource("Views/Workbenches/ChatWorkbench.cs");
        var apply = source[source.IndexOf("private void Apply", StringComparison.Ordinal)..source.IndexOf("private void Render", StringComparison.Ordinal)];
        Assert.True(apply.IndexOf("var owner = _request.GetResponseOwner(response, generation)", StringComparison.Ordinal) < apply.IndexOf("_request.Apply(response, generation)", StringComparison.Ordinal));
        var acknowledgement = apply[apply.IndexOf("if (owner == ChatResponseOwner.CancellationAcknowledgement)", StringComparison.Ordinal)..apply.IndexOf("if (action != \"history\")", StringComparison.Ordinal)];
        Assert.Contains("Waiting for the send's own completion", acknowledgement, StringComparison.Ordinal);
        Assert.Contains("return;", acknowledgement, StringComparison.Ordinal);
        Assert.DoesNotContain("Render(", acknowledgement, StringComparison.Ordinal);
        Assert.DoesNotContain("_presentation.Invalidate", acknowledgement, StringComparison.Ordinal);
        Assert.DoesNotContain("_sendObservation?.Cancel", acknowledgement, StringComparison.Ordinal);
        Assert.DoesNotContain("_input.Text", acknowledgement, StringComparison.Ordinal);
        Assert.Contains("action == \"send\" && !WorkbenchRequests.IsChatBusy(response.Status)", apply, StringComparison.Ordinal);
        Assert.Contains("Render(snapshot, \"history\")", source, StringComparison.Ordinal);
        Assert.Contains("if (ChatFrontendPolicy.ShouldReplaceCitations(action, response))", source, StringComparison.Ordinal);
    }

    [Fact]
    public void TimelineExposesMultipleSelectionRealJobActionsAndCompletionDrivenChapters()
    {
        var source = ReadSource("Views/Workbenches/CatalogWorkbench.cs");
        Assert.Contains("SelectionMode = ListViewSelectionMode.Multiple", source, StringComparison.Ordinal);
        Assert.Contains("CatalogJobActions.TimelineRequest", source, StringComparison.Ordinal);
        Assert.Contains("CatalogJobActions.ControlRequest(action, item.Job)", source, StringComparison.Ordinal);
        Assert.Contains("job.State == \"completed\" && job.FileIDs.Contains(file)", source, StringComparison.Ordinal);
        Assert.Contains("await RefreshDetailAsync(ct)", source, StringComparison.Ordinal);
        Assert.Contains("not event-detection coverage", source, StringComparison.Ordinal);
    }

    [Fact]
    public void StartupAndGenerationHaveSeparateBoundedWaitsAndStreamsAreNonTerminal()
    {
        var source = ReadSource("ViewModels/EngineClient.Workbenches.cs");
        Assert.Contains("await WaitForReadyAsync(TimeSpan.FromSeconds(30), ct)", source, StringComparison.Ordinal);
        Assert.Contains("TimeSpan.FromMinutes(10)", source, StringComparison.Ordinal);
        Assert.Contains("return await response.WaitAsync(deadline.Token)", source, StringComparison.Ordinal);
        Assert.Contains("!WorkbenchRequests.IsChatBusy(chat.Response.Status)", source, StringComparison.Ordinal);
        Assert.Contains("generation != expected", source, StringComparison.Ordinal);
    }

    private static string ReadSource(string relativePath)
    {
        for (var directory = new DirectoryInfo(AppContext.BaseDirectory); directory is not null; directory = directory.Parent)
        {
            var path = Path.Combine(directory.FullName, "platforms/windows/src/FileID.App", relativePath);
            if (File.Exists(path)) return File.ReadAllText(path);
        }
        throw new DirectoryNotFoundException("Windows app sources were not found.");
    }
}
