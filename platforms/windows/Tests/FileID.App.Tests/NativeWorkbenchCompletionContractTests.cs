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
    }

    [Fact]
    public void DispatchAndChatSendObserversSurviveOnlyUiAwaitCancellation()
    {
        var source = ReadSource("ViewModels/EngineClient.Workbenches.cs");
        Assert.Contains("WorkbenchDispatch.ObserveAsync", source, StringComparison.Ordinal);
        Assert.Contains("ChatSend.Apply(chat.Response, generation)", source, StringComparison.Ordinal);
        var chatSendStart = source.IndexOf("public async Task<ChatResponse> ChatAsync", StringComparison.Ordinal);
        var chatSendEnd = source.IndexOf("private async Task<T> SendChatWorkbenchAsync", chatSendStart, StringComparison.Ordinal);
        Assert.True(chatSendStart >= 0 && chatSendEnd > chatSendStart);
        var chatSend = source[chatSendStart..chatSendEnd];
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
