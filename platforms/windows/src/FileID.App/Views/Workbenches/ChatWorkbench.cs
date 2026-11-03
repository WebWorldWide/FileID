using System.ComponentModel;
using System.IO;
using FileID.IpcSchema;
using FileID.Services;
using FileID.ViewModels;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Workbenches;

internal sealed class ChatWorkbench : WorkbenchPanel
{
    private readonly TextBox _input = new() { PlaceholderText = "Ask about your files (up to 2,000 characters)", MaxLength = 2000, MinWidth = 400 };
    private readonly CheckBox _useModel = new() { Content = "Written answers from local AI (not available on Windows yet)", IsChecked = false, IsEnabled = ChatFrontendPolicy.SupportsModelGeneration };
    private readonly ListView _messages = new() { Height = 260, SelectionMode = ListViewSelectionMode.None };
    private readonly ListView _citations = new() { Height = 180 };
    private readonly EvidencePanel _evidence;
    private readonly ChatRequestState _request = new();
    private readonly ChatPresentationRevision _presentation = new();
    private readonly Button _stop;
    private readonly Button _send;
    private readonly Button _clear;
    private CancellationTokenSource? _sendObservation;
    private string? _conversationID;
    private readonly TextBlock _sourcesCount = Meta("");

    public ChatWorkbench() : base("FileID Chat")
    {
        _evidence = new EvidencePanel(message => Status.Text = message);
        var lookingIn = new StackPanel { Spacing = 6 };
        lookingIn.Children.Add(SectionLabel("Looking in"));
        var chips = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        foreach (var source in new[] { "Names", "Tags", "Descriptions", "Chapters", "Text in files" }) chips.Children.Add(Chip(source));
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(chips, "Looking in names, tags, descriptions, chapters and text in files");
        lookingIn.Children.Add(chips);
        var header = new Grid { ColumnSpacing = 8 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        _clear = Ghost(ActionButton("Clear", ClearAsync));
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(_clear, "Delete conversation");
        Grid.SetColumn(_clear, 1);
        header.Children.Add(lookingIn);
        header.Children.Add(_clear);
        SyncClear(hasContent: false);
        Inputs.Children.Add(header);
        _messages.Height = 420;
        _messages.MaxWidth = 760;
        _messages.HorizontalAlignment = HorizontalAlignment.Left;
        Inputs.Children.Add(_messages);
        _input.AcceptsReturn = false;
        _input.MinHeight = 30;
        _send = Primary(ActionButton("Send", SendAsync));
        _input.TextChanged += (_, _) => SyncSend();
        SyncSend();
        Inputs.Children.Add(Row(_input, _send));
        ToolTipService.SetToolTip(_useModel, "Written answers need a local AI chat model, which isn't available on Windows yet");
        Inputs.Children.Add(Row(_useModel, Ghost(ActionButton("Refresh history", HistoryAsync))));
        Inputs.Children.Add(Meta("Answers list matching files only. Written answers from local AI and stopping a running answer aren't available on Windows yet. Check the sources, since FileID can miss things. Wait for an answer to finish before sending again or clearing."));

        var sources = new Grid { Padding = new Thickness(0) };
        sources.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        sources.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        sources.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var sourcesHeader = new Grid { Padding = new Thickness(14, 12, 14, 12), ColumnSpacing = 8 };
        sourcesHeader.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        sourcesHeader.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        sourcesHeader.Children.Add(SectionLabel("Sources"));
        Grid.SetColumn(_sourcesCount, 1);
        sourcesHeader.Children.Add(_sourcesCount);
        _citations.Height = double.NaN;
        _citations.Padding = new Thickness(6, 0, 6, 0);
        Grid.SetRow(_citations, 1);
        var evidenceHost = new StackPanel { Spacing = 6, Padding = new Thickness(14, 10, 14, 14) };
        evidenceHost.Children.Add(Meta("Video frames are not checked, so they may not show the exact moment."));
        evidenceHost.Children.Add(_evidence);
        Grid.SetRow(evidenceHost, 2);
        sources.Children.Add(sourcesHeader);
        sources.Children.Add(_citations);
        sources.Children.Add(evidenceHost);
        UseSidePane(sources, 300, paneOnLeft: false);
        _stop = ControlButton("Stop response", StopAsync);
        _citations.SelectionChanged += (_, _) => _evidence.Select((_citations.SelectedItem as CitationItem)?.Hit);
    }
    protected override void OnOpened()
    {
        Engine.PropertyChanged += OnEngineChanged;
        AppViewModel.Instance.ChatDraftRequested += OnChatDraftRequested;
        UseChatDraft();
        Run(async ct =>
        {
            var id = await ChatConversationStore.LoadOrCreateAsync(Path.Combine(AppPaths.Root, "chat-conversation-id.txt"), ct);
            await UI(() =>
            {
                if (_conversationID != id) _presentation.Invalidate();
                _conversationID = id;
            }, ct);
            await HistoryAsync(ct);
        });
    }
    protected override void OnClosed()
    {
        Engine.PropertyChanged -= OnEngineChanged;
        AppViewModel.Instance.ChatDraftRequested -= OnChatDraftRequested;
        _presentation.Invalidate();
        _request.Reset();
        _sendObservation?.Cancel();
    }
    private void OnChatDraftRequested(object? sender, EventArgs e)
        => DebugLog.SafeRun("ChatWorkbench.OnChatDraftRequested", () => DispatcherQueue.TryEnqueue(UseChatDraft));
    private void UseChatDraft()
    {
        if (AppViewModel.Instance.TakeChatDraft() is { } draft) _input.Text = draft;
    }
    private void SyncSend()
    {
        var text = _input.Text.Trim();
        _send.IsEnabled = text.Length > 0 && _input.Text.Length <= 2000;
        ToolTipService.SetToolTip(_send, text.Length == 0
            ? "Type a question first"
            : _input.Text.Length > 2000 ? "Shorten the question to 2,000 characters" : "Send");
    }
    private void SyncClear(bool hasContent)
    {
        _clear.IsEnabled = hasContent;
        ToolTipService.SetToolTip(_clear, hasContent ? "Start a new conversation. This one is deleted." : "Nothing to clear yet");
    }
    protected override void UpdateControls() => _stop.IsEnabled = ChatFrontendPolicy.SupportsCancellation && _request.CanStop && _request.Generation == Engine.SpawnGeneration;
    private void OnEngineChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName is not (nameof(EngineClient.ChatResponse) or nameof(EngineClient.ChatSend) or nameof(EngineClient.SpawnGeneration))) return;
        DebugLog.SafeRun("ChatWorkbench.OnEngineChanged", () =>
        {
            DebugLog.Info($"[ENGINE-SUB:ChatWorkbench] {e.PropertyName}");
            var response = Engine.ChatResponse;
            var generation = Engine.SpawnGeneration;
            var ct = Lifetime;
            _ = DebugLog.SafeRunAsync("ChatWorkbench.ApplySnapshot", () => UI(() =>
            {
                if (generation != Engine.SpawnGeneration) return;
                if (e.PropertyName == nameof(EngineClient.SpawnGeneration) && _request.Generation != generation)
                {
                    _presentation.Invalidate();
                    _request.Reset();
                    _sendObservation?.Cancel();
                    Status.Text = "FileID restarted. Press Refresh history before sending again.";
                    UpdateControls();
                }
                else if (e.PropertyName == nameof(EngineClient.ChatSend))
                {
                    SynchronizeSend();
                }
                else if (e.PropertyName == nameof(EngineClient.ChatResponse) && response is not null)
                {
                    Apply(response, generation);
                }
            }, ct));
        });
    }
    private void SynchronizeSend()
    {
        var pending = Engine.ChatSend.Request;
        if (pending is not null && pending.ConversationID == _conversationID)
        {
            if (_request.RequestID != pending.RequestID || _request.Generation != Engine.ChatSend.Generation)
            {
                _presentation.Invalidate();
                _request.Reset();
                _request.Begin(pending, Engine.ChatSend.Generation);
            }
        }
        else if (_request.Action == "send" && Engine.ChatSend.LastResponse?.RequestID != _request.RequestID)
        {
            _presentation.Invalidate();
            _request.Reset();
        }

        UpdateControls();
    }
    private string RequireConversation() => _conversationID ?? throw new InvalidOperationException("The saved conversation isn't loaded yet. Reopen Chat to try again.");
    private Task HistoryAsync(CancellationToken ct) => RequestAsync("history", null, ct);
    private async Task ClearAsync(CancellationToken ct)
    {
        if (_request.HasActiveSend || Engine.ChatSend.IsActive) throw new InvalidOperationException("Wait for the answer to finish before clearing.");
        if (!await ConfirmAsync("Start a new conversation?", "This conversation is deleted. Your files are not changed.")) return;
        ct.ThrowIfCancellationRequested();
        await RequestAsync("clear", null, ct);
    }
    private Task SendAsync(CancellationToken ct)
    {
        var text = _input.Text.Trim();
        if (string.IsNullOrWhiteSpace(text) || text.Length > 2000) throw new InvalidOperationException("Type a question of up to 2,000 characters.");
        return RequestAsync("send", text, ct);
    }
    private async Task RequestAsync(string action, string? text, CancellationToken ct)
    {
        var id = RequireConversation();
        await UI(SynchronizeSend, ct);
        await Engine.WaitForReadyAsync(TimeSpan.FromSeconds(30), ct);
        var generation = Engine.SpawnGeneration;
        var request = new ChatRequest(Guid.NewGuid().ToString(), id, action, text, UseModel: action == "send" && ChatFrontendPolicy.UseModelForSend(_useModel.IsChecked == true));
        if (action == "history" && _request.HasActiveSend)
        {
            ChatPresentationRevision.HistoryStamp? stamp = null;
            await UI(() => stamp = _presentation.CaptureHistory(request, generation), ct);
            ct.ThrowIfCancellationRequested();
            var history = await Engine.ChatAsync(request, generation, ct);
            await UI(() => _presentation.TryRenderHistory(stamp!.Value, history, _conversationID, Engine.SpawnGeneration, snapshot =>
            {
                Render(snapshot, "history");
                Status.Text = $"History snapshot · {snapshot.Status} · {snapshot.Message}" + (Engine.ChatSend.IsActive ? " · Send remains unconfirmed." : "");
            }), ct);
            return;
        }
        _request.Begin(request, generation);
        UpdateControls();
        using var observation = CancellationTokenSource.CreateLinkedTokenSource(ct);
        if (action == "send") _sendObservation = observation;
        try
        {
            var response = await Engine.ChatAsync(request, generation, observation.Token);
            await UI(() => Apply(response, generation), ct);
        }
        catch (OperationCanceledException) when (observation.IsCancellationRequested) { }
        finally
        {
            if (ReferenceEquals(_sendObservation, observation)) _sendObservation = null;
            await UI(SynchronizeSend, ct);
        }
    }
    private async Task StopAsync(CancellationToken ct)
    {
        if (!ChatFrontendPolicy.SupportsCancellation) throw new InvalidOperationException("Stopping an answer isn't available on Windows yet. Wait for it to finish.");
        SynchronizeSend();
        var generation = Engine.SpawnGeneration;
        var cancel = _request.Cancel(generation);
        Status.Text = "Stop requested; waiting for the engine to confirm…";
        try
        {
            var response = await Engine.ChatAsync(cancel, generation, ct);
            await UI(() => Apply(response, generation), ct);
        }
        catch
        {
            await UI(() =>
            {
                _presentation.Invalidate();
                _request.CancelFailed();
            }, ct);
            throw;
        }
    }
    private void Apply(ChatResponse response, int generation)
    {
        if (generation != Engine.SpawnGeneration || _conversationID != response.ConversationID) return;
        var action = _request.Action;
        var owner = _request.GetResponseOwner(response, generation);
        if (!_request.Apply(response, generation)) return;
        if (owner == ChatResponseOwner.CancellationAcknowledgement)
        {
            Status.Text = $"Cancellation acknowledgement · {response.Message} · Waiting for the send's own completion.";
            UpdateControls();
            return;
        }
        if (action != "history") _presentation.Invalidate();
        Status.Text = ChatFrontendPolicy.StatusLine(response);
        Render(response, action);
        if (action == "send" && !WorkbenchRequests.IsChatBusy(response.Status)) _sendObservation?.Cancel();
        if (action == "send" && response.Status == "completed") _input.Text = "";
        UpdateControls();
    }
    private void Render(ChatResponse response, string? action)
    {
        if (response.Status == "error") return;
        _messages.ItemsSource = response.Messages.Select(message => $"{(message.Role == "user" ? "You" : "FileID")}\n{message.Text}").ToArray();
        SyncClear(response.Messages.Count > 0 || response.Hits.Count > 0);
        if (ChatFrontendPolicy.ShouldReplaceCitations(action, response))
        {
            _citations.ItemsSource = response.Hits.Select((hit, index) => new CitationItem(index + 1, hit)).ToArray();
            _sourcesCount.Text = response.Hits.Count > 0 ? response.Hits.Count.ToString("N0") : string.Empty;
            _evidence.Select(null);
        }
    }
    private sealed record CitationItem(int Index, CatalogHit Hit)
    {
        public override string ToString() => $"[{Index}] {EvidencePanel.Label(Hit)}";
    }
}
