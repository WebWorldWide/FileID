using System.IO;
using FileID.IpcSchema;
using FileID.Services;
using Microsoft.UI.Xaml.Controls;

namespace FileID.Views.Workbenches;

internal sealed class ChatWorkbench : WorkbenchPanel
{
    private readonly TextBox _input = new() { PlaceholderText = "Ask about your local catalog (up to 2000 characters)", MaxLength = 2000, MinWidth = 400 };
    private readonly ListView _messages = new() { Height = 260, SelectionMode = ListViewSelectionMode.None };
    private readonly ListView _citations = new() { Height = 180 };
    private readonly EvidencePanel _evidence;
    private string? _conversationID;

    public ChatWorkbench() : base("FileID Chat")
    {
        _evidence = new EvidencePanel(message => Status.Text = message);
        Inputs.Children.Add(Text("Local keyword retrieval grounded in catalog evidence. Sparse analysis can miss events. Loaded-model summaries and Stop are not available in this Windows backend; responses are snapshots, not simulated token streaming."));
        Inputs.Children.Add(Row(ActionButton("Refresh conversation history", HistoryAsync), ActionButton("Delete conversation…", ClearAsync)));
        Inputs.Children.Add(_messages);
        Inputs.Children.Add(Row(_input, ActionButton("Send", SendAsync)));
        Inputs.Children.Add(Text("Citations · sampled frames are unverified, not identified events"));
        Inputs.Children.Add(_citations);
        Inputs.Children.Add(_evidence);
        _citations.SelectionChanged += (_, _) => _evidence.Select((_citations.SelectedItem as CitationItem)?.Hit);
    }
    protected override void OnOpened() => Run(async ct =>
    {
        var id = await ChatConversationStore.LoadOrCreateAsync(Path.Combine(AppPaths.Root, "chat-conversation-id.txt"), ct);
        await UI(() => _conversationID = id, ct);
        await HistoryAsync(ct);
    });
    private string RequireConversation() => _conversationID ?? throw new InvalidOperationException("The saved conversation is not ready. Reopen Chat to retry loading it.");
    private Task HistoryAsync(CancellationToken ct) => RequestAsync("history", null, ct);
    private async Task ClearAsync(CancellationToken ct)
    {
        if (!await ConfirmAsync("Delete this conversation?", "Permanently clear the saved local messages? Catalog files and evidence are unchanged.")) return;
        ct.ThrowIfCancellationRequested();
        await RequestAsync("clear", null, ct);
    }
    private Task SendAsync(CancellationToken ct)
    {
        var text = _input.Text.Trim();
        if (string.IsNullOrWhiteSpace(text) || text.Length > 2000) throw new InvalidOperationException("Enter a query of 1–2000 characters.");
        return RequestAsync("send", text, ct);
    }
    private async Task RequestAsync(string action, string? text, CancellationToken ct)
    {
        var id = RequireConversation();
        var response = await Engine.ChatAsync(new ChatRequest(Guid.NewGuid().ToString(), id, action, text, UseModel: false), ct);
        await UI(() =>
        {
            if (_conversationID != response.ConversationID) return;
            Status.Text = $"{response.Status} · {response.Message}";
            if (response.Status == "error") return;
            _messages.ItemsSource = response.Messages.Select(message => $"{(message.Role == "user" ? "You" : "FileID")}\n{message.Text}").ToArray();
            if (action != "history" || response.Hits.Count > 0)
            {
                _citations.ItemsSource = response.Hits.Select((hit, index) => new CitationItem(index + 1, hit)).ToArray();
                _evidence.Select(null);
            }
            if (action == "send" && response.Status == "completed") _input.Text = "";
        }, ct);
    }
    private sealed record CitationItem(int Index, CatalogHit Hit)
    {
        public override string ToString() => $"[{Index}] {EvidencePanel.Label(Hit)}";
    }
}
