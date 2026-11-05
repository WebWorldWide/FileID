using System.ComponentModel;
using System.IO;
using FileID.IpcSchema;
using FileID.Services;
using FileID.Theme.Controls;
using FileID.Theme.Motion;
using FileID.ViewModels;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Windows.UI;

namespace FileID.Views.Workbenches;

internal sealed class ChatWorkbench : WorkbenchPanel
{
    private readonly TextBox _input = new() { PlaceholderText = "Ask about your files (up to 2,000 characters)", MaxLength = 2000, MinWidth = 400 };
    private readonly CheckBox _useModel = new() { Content = "Written answers from local AI (not available on Windows yet)", IsChecked = false, IsEnabled = ChatFrontendPolicy.SupportsModelGeneration };
    private readonly StackPanel _messageList = new() { Spacing = 20, Padding = new Thickness(0, 4, 12, 4) };
    private readonly ScrollViewer _messageScroll;
    private readonly List<MessageView> _messageViews = new();
    private readonly StackPanel _busy = new() { Orientation = Orientation.Horizontal, Spacing = 10, Visibility = Visibility.Collapsed };
    private readonly ListView _citations = new() { Height = 180, IsItemClickEnabled = true };
    private readonly TextBlock _sourcesEmpty = Meta("Files an answer draws on show here. Click one to open it.");
    private IReadOnlyList<CatalogHit> _sourceHits = Array.Empty<CatalogHit>();
    private int? _openedSource;
    private string? _lastScrolledID;
    private readonly Brush _gold = ThemeHelper.GetBrushSafe("GoldBrush");
    private readonly Brush _ai = ThemeHelper.GetBrushSafe("AiBrush");
    private readonly Brush _text1 = ThemeHelper.GetBrushSafe("Text1Brush");
    private readonly Brush _text2 = ThemeHelper.GetBrushSafe("Text2Brush");
    private readonly Brush _text3 = ThemeHelper.GetBrushSafe("Text3Brush");
    private readonly Brush _hairlineStrong = ThemeHelper.GetBrushSafe("HairlineStrongBrush");
    private readonly Brush _goldRail = new SolidColorBrush(Color.FromArgb(0x80, 0xFF, 0xCC, 0x00));
    private readonly Brush _aiRail = new SolidColorBrush(Color.FromArgb(0x80, 0xB1, 0x9B, 0xCE));
    private readonly Brush _chipFill = new SolidColorBrush(Color.FromArgb(0x0A, 0xFF, 0xFF, 0xFF));
    private readonly Brush _chipActiveFill = new SolidColorBrush(Color.FromArgb(0x1F, 0xFF, 0xCC, 0x00));
    private readonly Brush _chipActiveStroke = new SolidColorBrush(Color.FromArgb(0x80, 0xFF, 0xCC, 0x00));
    private readonly EvidencePanel _evidence;
    private readonly ChatRequestState _request = new();
    private readonly ChatPresentationRevision _presentation = new();
    private readonly Button _stop;
    private readonly Button _send;
    private readonly Button _clear;
    private CancellationTokenSource? _sendObservation;
    private string? _conversationID;
    private readonly TextBlock _sourcesCount = Meta("");
    private readonly StackPanel _empty = new() { Spacing = 12, Padding = new Thickness(0, 8, 0, 8) };
    private static readonly string[] Suggestions = { "Find birthday gift opening", "How many videos are from last month?", "How much space do duplicates use?" };

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
        _messageScroll = new ScrollViewer
        {
            Content = _messageList,
            Height = 420,
            MaxWidth = 760,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            Visibility = Visibility.Collapsed,
        };
        AutomationProperties.SetName(_messageScroll, "Conversation");
        BuildEmptyState();
        Inputs.Children.Add(_empty);
        Inputs.Children.Add(_messageScroll);
        var busyText = Meta("Searching your library…");
        busyText.Foreground = _ai;
        busyText.VerticalAlignment = VerticalAlignment.Center;
        _busy.Children.Add(new ProgressBar { IsIndeterminate = true, Width = 60, VerticalAlignment = VerticalAlignment.Center });
        _busy.Children.Add(busyText);
        Inputs.Children.Add(_busy);
        _input.AcceptsReturn = false;
        _input.MinHeight = 30;
        _send = Primary(ActionButton("Send", SendAsync));
        var send = _send;
        _input.KeyDown += (_, e) => DebugLog.SafeRun("ChatWorkbench.ReturnSends", () =>
        {
            if (e.Key != Windows.System.VirtualKey.Enter || !send.IsEnabled) return;
            e.Handled = true;
            Run(SendAsync);
        });
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
        _sourcesCount.Text = ChatPresentation.SourcesCount(0);
        _sourcesCount.FontSize = 12;
        Grid.SetColumn(_sourcesCount, 1);
        sourcesHeader.Children.Add(_sourcesCount);
        _citations.Height = double.NaN;
        _citations.Padding = new Thickness(6, 8, 6, 8);
        Grid.SetRow(_citations, 1);
        _sourcesEmpty.TextAlignment = TextAlignment.Center;
        _sourcesEmpty.Margin = new Thickness(24);
        _sourcesEmpty.VerticalAlignment = VerticalAlignment.Center;
        _sourcesEmpty.IsHitTestVisible = false;
        Grid.SetRow(_sourcesEmpty, 1);
        var evidenceHost = new StackPanel { Spacing = 6, Padding = new Thickness(14, 10, 14, 14) };
        evidenceHost.Children.Add(Meta("Video frames are not checked, so they may not show the exact moment."));
        evidenceHost.Children.Add(_evidence);
        Grid.SetRow(evidenceHost, 2);
        sources.Children.Add(sourcesHeader);
        sources.Children.Add(_citations);
        sources.Children.Add(_sourcesEmpty);
        sources.Children.Add(evidenceHost);
        UseSidePane(sources, 300, paneOnLeft: false);
        _stop = ControlButton("Stop response", StopAsync);
        _citations.SelectionChanged += (_, _) => DebugLog.SafeRun("ChatWorkbench.SourceSelected", () =>
        {
            var index = _citations.SelectedIndex;
            _openedSource = index >= 0 && index < _sourceHits.Count ? index : null;
            _evidence.Select(_openedSource is { } opened ? _sourceHits[opened] : null);
            SyncChipStates();
        });
        _citations.ItemClick += (_, e) => DebugLog.SafeRun("ChatWorkbench.SourceClicked", () =>
        {
            if (e.ClickedItem is FrameworkElement { Tag: int position }) OpenSource(position);
        });
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
    private void BuildEmptyState()
    {
        var title = new TextBlock { Text = "Find the moment.", FontSize = 34, Foreground = ThemeHelper.GetBrushSafe("Text1Brush") };
        var body = new TextBlock
        {
            Text = "Ask about files, people and moments in this library. Answers list the files they come from.",
            FontSize = 13.5,
            TextWrapping = TextWrapping.Wrap,
            MaxWidth = 560,
            HorizontalAlignment = HorizontalAlignment.Left,
            Foreground = ThemeHelper.GetBrushSafe("Text2Brush"),
        };
        try
        {
            if (Application.Current.Resources["DisplayFontFamily"] is Microsoft.UI.Xaml.Media.FontFamily display) title.FontFamily = display;
            if (Application.Current.Resources["BodyFontFamily"] is Microsoft.UI.Xaml.Media.FontFamily font) body.FontFamily = font;
        }
        catch (Exception ex)
        {
            DebugLog.Warn("ChatWorkbench: fonts unavailable: " + ex.Message);
        }
        _empty.Children.Add(title);
        _empty.Children.Add(body);
        var list = new StackPanel { Spacing = 6, MaxWidth = 460, HorizontalAlignment = HorizontalAlignment.Left };
        foreach (var suggestion in Suggestions)
        {
            var row = new Grid();
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            var arrow = new TextBlock { Text = "↖", FontSize = 11, Foreground = ThemeHelper.GetBrushSafe("Text3Brush"), VerticalAlignment = VerticalAlignment.Center };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetAccessibilityView(arrow, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
            Grid.SetColumn(arrow, 1);
            row.Children.Add(new TextBlock { Text = suggestion, FontSize = 13, VerticalAlignment = VerticalAlignment.Center });
            row.Children.Add(arrow);
            var button = Secondary(new Button
            {
                Content = row,
                Height = 34,
                HorizontalAlignment = HorizontalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Stretch,
            });
            button.Background = ThemeHelper.GetBrushSafe("PaneFillBrush");
            button.BorderBrush = ThemeHelper.GetBrushSafe("HairlineBrush");
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(button, suggestion);
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(button, "Puts this question in the message field");
            button.Click += (_, _) => DebugLog.SafeRun("ChatWorkbench.Suggestion", () =>
            {
                _input.Text = suggestion;
                _input.SelectionStart = suggestion.Length;
                _input.Focus(FocusState.Programmatic);
            });
            list.Children.Add(button);
        }
        _empty.Children.Add(list);
    }

    private void SyncClear(bool hasContent)
    {
        _clear.IsEnabled = hasContent;
        ToolTipService.SetToolTip(_clear, hasContent ? "Start a new conversation. This one is deleted." : "Nothing to clear yet");
    }
    protected override void UpdateControls()
    {
        _stop.IsEnabled = ChatFrontendPolicy.SupportsCancellation && _request.CanStop && _request.Generation == Engine.SpawnGeneration;
        _busy.Visibility = _request.HasActiveSend ? Visibility.Visible : Visibility.Collapsed;
    }
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
        RenderMessages(response.Messages);
        _messageScroll.Visibility = response.Messages.Count > 0 ? Visibility.Visible : Visibility.Collapsed;
        _empty.Visibility = response.Messages.Count > 0 ? Visibility.Collapsed : Visibility.Visible;
        SyncClear(response.Messages.Count > 0 || response.Hits.Count > 0);
        if (ChatFrontendPolicy.ShouldReplaceCitations(action, response))
        {
            RenderSources(response.Hits);
            _sourcesCount.Text = ChatPresentation.SourcesCount(response.Hits.Count);
            _evidence.Select(null);
        }
        RenderChips(response.Messages);
    }

    /// <summary>Updates the shown messages in place while a reply only adds to
    /// them; rebuilds the list when the conversation changed underneath.</summary>
    private void RenderMessages(IReadOnlyList<ChatMessage> messages)
    {
        if (!ChatPresentation.ExtendsShown(_messageViews.Select(view => view.ID).ToArray(), messages))
        {
            _messageList.Children.Clear();
            _messageViews.Clear();
        }
        for (var i = 0; i < messages.Count; i++)
        {
            var message = messages[i];
            if (i < _messageViews.Count)
            {
                if (_messageViews[i].Body.Text != message.Text) _messageViews[i].Body.Text = message.Text;
                continue;
            }
            var view = BuildMessage(message);
            _messageViews.Add(view);
            _messageList.Children.Add(view.Root);
        }
        var lastID = messages.Count > 0 ? messages[^1].Id : null;
        if (lastID is null || lastID == _lastScrolledID) return;
        _lastScrolledID = lastID;
        DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () => DebugLog.SafeRun("ChatWorkbench.ScrollToLatest", () =>
            _messageScroll.ChangeView(null, _messageScroll.ScrollableHeight, null, ReducedMotion.Instance.IsReduced)));
    }

    /// <summary>One message (DESIGN_V3_PORT "Chat", Messages): 12 px inset beside a 2 px rail in the
    /// role colour, the role label, the selectable body and room for citation chips.</summary>
    private MessageView BuildMessage(ChatMessage message)
    {
        var user = ChatPresentation.IsUser(message);
        var role = SectionLabel(ChatPresentation.RoleLabel(message));
        role.Foreground = user ? _gold : _ai;
        var body = new TextBlock
        {
            Text = message.Text,
            FontSize = 13.5,
            LineHeight = 20,
            Foreground = _text1,
            TextWrapping = TextWrapping.Wrap,
            IsTextSelectionEnabled = true,
        };
        UseFont(body, "BodyFontFamily");
        var chips = new FlowPanel { Spacing = 6, Margin = new Thickness(0, 2, 0, 0), Visibility = Visibility.Collapsed };
        var stack = new StackPanel { Spacing = 6, Margin = new Thickness(12, 0, 0, 0) };
        stack.Children.Add(role);
        stack.Children.Add(body);
        stack.Children.Add(chips);
        var rail = new Border { Width = 2, HorizontalAlignment = HorizontalAlignment.Left, Background = user ? _goldRail : _aiRail };
        var root = new Grid();
        root.Children.Add(rail);
        root.Children.Add(stack);
        return new MessageView(message.Id, root, body, chips);
    }

    /// <summary>Citation chips under the latest answer only, at most eight.</summary>
    private void RenderChips(IReadOnlyList<ChatMessage> messages)
    {
        var lastAnswer = ChatPresentation.LastAnswerID(messages);
        var hits = ChatPresentation.ChipHits(_sourceHits);
        foreach (var view in _messageViews)
        {
            if (view.ID != lastAnswer || hits.Count == 0)
            {
                if (view.Chips.Children.Count > 0) view.Chips.Children.Clear();
                view.ChipSource = null;
                view.Chips.Visibility = Visibility.Collapsed;
                continue;
            }
            if (!ReferenceEquals(view.ChipSource, _sourceHits))
            {
                view.Chips.Children.Clear();
                for (var i = 0; i < hits.Count; i++) view.Chips.Children.Add(CitationChip(i, hits[i]));
                view.ChipSource = _sourceHits;
            }
            view.Chips.Visibility = Visibility.Visible;
        }
        SyncChipStates();
    }

    private Button CitationChip(int position, CatalogHit hit)
    {
        var index = position + 1;
        var number = TextLine($"[{index}]", 11, _gold, "MonoFontFamily");
        number.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        var content = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 5 };
        content.Children.Add(number);
        content.Children.Add(TextLine(HitPresentation.FileName(hit.Path), 11, _text2, "MonoFontFamily"));
        var meta = ChatPresentation.ChipMeta(hit);
        if (meta.Length > 0) content.Children.Add(TextLine(meta, 11, _text3, "MonoFontFamily"));
        var chip = Ghost(new Button
        {
            Content = content,
            MinHeight = 24,
            MaxWidth = 280,
            Padding = new Thickness(8, 2, 8, 2),
            CornerRadius = new CornerRadius(6),
            BorderThickness = new Thickness(1),
            Tag = position,
        });
        chip.Background = _chipFill;
        chip.BorderBrush = _hairlineStrong;
        ToolTipService.SetToolTip(chip, hit.Path);
        AutomationProperties.SetName(chip, HitPresentation.SourceSpoken(index, hit));
        AutomationProperties.SetHelpText(chip, "Opens the file");
        chip.ContextFlyout = RevealMenu(hit.Path);
        chip.Click += (_, _) => DebugLog.SafeRun("ChatWorkbench.Chip", () => OpenSource(position));
        return chip;
    }

    private void SyncChipStates()
    {
        foreach (var view in _messageViews)
        {
            foreach (var child in view.Chips.Children)
            {
                if (child is not Button { Tag: int position } chip) continue;
                var active = _openedSource == position;
                chip.Background = active ? _chipActiveFill : _chipFill;
                chip.BorderBrush = active ? _chipActiveStroke : _hairlineStrong;
            }
        }
    }

    /// <summary>Source rail rows (DESIGN_V3_PORT "Chat", Sources pane): "[n]" in gold, file name,
    /// time or page (lavender for unchecked video frames) and up to three lines of matched text.</summary>
    private void RenderSources(IReadOnlyList<CatalogHit> hits)
    {
        _sourceHits = hits;
        _openedSource = null;
        _citations.Items.Clear();
        for (var i = 0; i < hits.Count; i++) _citations.Items.Add(SourceRow(i, hits[i]));
        _sourcesEmpty.Visibility = hits.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
    }

    private Grid SourceRow(int position, CatalogHit hit)
    {
        var index = position + 1;
        var number = TextLine($"[{index}]", 11.5, _gold, "MonoFontFamily");
        number.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        var name = TextLine(HitPresentation.FileName(hit.Path), 12, _text1, "MonoFontFamily");
        name.FontWeight = Microsoft.UI.Text.FontWeights.Medium;
        var text = new StackPanel { Spacing = 3 };
        text.Children.Add(name);
        var meta = HitPresentation.SourceMeta(hit);
        if (meta.Length > 0) text.Children.Add(TextLine(meta, 11.5, HitPresentation.IsSampled(hit) ? _ai : _text3));
        if (!string.IsNullOrWhiteSpace(hit.Text))
        {
            var snippet = TextLine(hit.Text, 12, _text2);
            snippet.TextWrapping = TextWrapping.Wrap;
            snippet.MaxLines = 3;
            text.Children.Add(snippet);
        }
        var row = new Grid { ColumnSpacing = 8, Padding = new Thickness(2, 7, 2, 7), Tag = position };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(30) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        Grid.SetColumn(text, 1);
        row.Children.Add(number);
        row.Children.Add(text);
        AutomationProperties.SetName(row, HitPresentation.SourceSpoken(index, hit));
        AutomationProperties.SetHelpText(row, ("Opens the file. " + hit.Text).Trim());
        ToolTipService.SetToolTip(row, hit.Path);
        row.ContextFlyout = RevealMenu(hit.Path);
        return row;
    }

    private static MenuFlyout RevealMenu(string path)
    {
        var reveal = new MenuFlyoutItem { Text = "Show in Explorer" };
        reveal.Click += (_, _) => DebugLog.SafeRun("ChatWorkbench.Reveal", () => SafeOpen.Reveal(path));
        var menu = new MenuFlyout();
        menu.Items.Add(reveal);
        return menu;
    }

    /// <summary>Selects the source and opens it: media plays in the panel below, other files open in their app.</summary>
    private void OpenSource(int position)
    {
        if (position < 0 || position >= _sourceHits.Count) return;
        if (_citations.SelectedIndex != position) _citations.SelectedIndex = position;
        var start = _sourceHits[position].StartSeconds ?? 0;
        _ = DebugLog.SafeRunAsync("ChatWorkbench.OpenSource", () => _evidence.SeekAsync(start));
    }

    private sealed class MessageView(string id, Grid root, TextBlock body, FlowPanel chips)
    {
        public string ID { get; } = id;
        public Grid Root { get; } = root;
        public TextBlock Body { get; } = body;
        public FlowPanel Chips { get; } = chips;
        public IReadOnlyList<CatalogHit>? ChipSource { get; set; }
    }
}
