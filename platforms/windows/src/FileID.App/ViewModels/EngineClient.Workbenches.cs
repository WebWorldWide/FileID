using FileID.IpcSchema;
using FileID.Services;

namespace FileID.ViewModels;

internal sealed partial class EngineClient
{
    private readonly WorkbenchRequests _workbenchRequests = new();
    private CatalogResponse? _catalogResponse;
    private ToolResponse? _toolResponse;
    private ChatResponse? _chatResponse;
    public CatalogResponse? CatalogResponse { get => _catalogResponse; private set => Set(ref _catalogResponse, value); }
    public ToolResponse? ToolResponse { get => _toolResponse; private set => Set(ref _toolResponse, value); }
    public ChatResponse? ChatResponse { get => _chatResponse; private set => Set(ref _chatResponse, value); }

    public Task<CatalogResponse> CatalogAsync(CatalogRequest request, CancellationToken ct = default) =>
        SendWorkbenchAsync<CatalogResponse>(request.RequestID, new CatalogRequestCommand(request), TimeSpan.FromSeconds(30), ct);
    public Task<ToolResponse> ToolAsync(ToolRequest request, CancellationToken ct = default) =>
        SendWorkbenchAsync<ToolResponse>(request.RequestID, new ToolRequestCommand(request), TimeSpan.FromMinutes(5), ct);
    public Task<ChatResponse> ChatAsync(ChatRequest request, CancellationToken ct = default) =>
        SendWorkbenchAsync<ChatResponse>(request.RequestID, new ChatRequestCommand(request), TimeSpan.FromSeconds(60), ct, request.ConversationID);

    public async Task<CatalogResponse> CatalogSearchAsync(string query, string mode, CancellationToken ct = default)
    {
        var generation = SpawnGeneration;
        IReadOnlyList<float>? vector = null;
        if (mode is "hybrid" or "semantic")
        {
            var queryID = Guid.NewGuid().ToString();
            var embedding = await SendWorkbenchAsync<ClipTextEmbedding>(queryID, new EmbedTextQueryCommand(query, queryID),
                TimeSpan.FromSeconds(30), ct, query).ConfigureAwait(false);
            if (generation != SpawnGeneration || State != LifecycleState.Ready)
                throw new InvalidOperationException("The engine changed while embedding this query. Retry, or use keyword search.");
            vector = embedding.Embedding;
        }
        var request = CatalogEmbeddingSpace.SearchRequest(query, mode, vector);
        return await CatalogAsync(request, ct).ConfigureAwait(false);
    }

    private async Task<T> SendWorkbenchAsync<T>(string id, CommandPayload command, TimeSpan timeout,
        CancellationToken ct, string? conversationID = null)
    {
        var generation = SpawnGeneration;
        var response = _workbenchRequests.Register<T>(id, generation, conversationID);
        using var deadline = CancellationTokenSource.CreateLinkedTokenSource(ct);
        deadline.CancelAfter(timeout);
        try
        {
            await SendCommandAsync(command, deadline.Token).WaitAsync(deadline.Token).ConfigureAwait(false);
            return await response.WaitAsync(deadline.Token).ConfigureAwait(false);
        }
        catch (OperationCanceledException) when (!ct.IsCancellationRequested)
        {
            throw new TimeoutException("The engine has not confirmed this request. It may still be running; refresh history before retrying changes.");
        }
        finally
        {
            _workbenchRequests.Remove<T>(id, new OperationCanceledException("The request is no longer being observed."));
            _ = response.Exception;
            _ = response.ContinueWith(task => _ = task.Exception, CancellationToken.None,
                TaskContinuationOptions.OnlyOnFaulted | TaskContinuationOptions.ExecuteSynchronously, TaskScheduler.Default);
        }
    }

    private void ApplyWorkbenchResponse(EventPayload? payload, int generation)
    {
        switch (payload)
        {
            case ClipTextEmbeddingEvent embedding:
                _workbenchRequests.Apply(embedding.Embedding.QueryId, generation, embedding.Embedding, embedding.Embedding.Query);
                break;
            case CatalogResponseEvent catalog when _workbenchRequests.Apply(catalog.Response.RequestID, generation, catalog.Response):
                CatalogResponse = catalog.Response;
                break;
            case ToolResponseEvent tool when _workbenchRequests.Apply(tool.Response.RequestID, generation, tool.Response):
                ToolResponse = tool.Response;
                break;
            case ChatResponseEvent chat when _workbenchRequests.Apply(chat.Response.RequestID, generation, chat.Response,
                chat.Response.ConversationID, !WorkbenchRequests.IsChatBusy(chat.Response.Status)):
                ChatResponse = chat.Response;
                break;
        }
    }
}
