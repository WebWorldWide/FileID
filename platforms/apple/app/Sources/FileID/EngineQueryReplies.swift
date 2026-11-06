import Foundation
import FileIDShared

/// Waiters for engine replies that answer one request: merge suggestions,
/// CLIP query vectors and video thumbnails. Every wait ends with the reply,
/// nil on timeout, cancellation or engine exit, or nil when the command
/// could not be sent.
@MainActor
final class EngineQueryReplies {
    static let mergeSuggestionsTimeout: Duration = .seconds(60)
    static let textEmbeddingTimeout: Duration = .seconds(30)
    static let videoThumbnailTimeout: Duration = .seconds(15)

    private let merges: ReplyWaiters<Int, [MergeSuggestionPair]>
    private let embeddings: ReplyWaiters<String, [Float]>
    private let thumbnails: ReplyWaiters<String, ThumbnailGenerated>

    init(mergeTimeout: Duration = mergeSuggestionsTimeout,
         embeddingTimeout: Duration = textEmbeddingTimeout,
         thumbnailTimeout: Duration = videoThumbnailTimeout) {
        merges = ReplyWaiters(timeout: mergeTimeout)
        embeddings = ReplyWaiters(timeout: embeddingTimeout)
        thumbnails = ReplyWaiters(timeout: thumbnailTimeout)
    }

    var waiting: Int { merges.count + embeddings.count + thumbnails.count }

    /// `findMergeSuggestions` carries no request id, so one reply answers
    /// every caller waiting at that moment.
    func mergeSuggestions(send: () -> Bool) async -> [MergeSuggestionPair]? {
        await merges.wait(for: 0, send: send)
    }

    /// Nil when the engine has no CLIP text encoder; it answers with an empty
    /// vector then, and search stays on keywords.
    func textEmbedding(send: (String) -> Bool) async -> [Float]? {
        let queryID = UUID().uuidString
        let vector = await embeddings.wait(for: queryID) { send(queryID) }
        guard let vector, vector.count == CLIPEmbeddingSpace.dimension else { return nil }
        return vector
    }

    /// JPEG bytes for `path`, or nil when the engine stays silent (it sends
    /// nothing for a file it can't decode) or answers for another revision.
    func videoThumbnail(path: String, modifiedAt: Double?, send: () -> Bool) async -> Data? {
        guard let reply = await thumbnails.wait(for: path, send: send),
              reply.modifiedAt == modifiedAt,
              let jpeg = Data(base64Encoded: reply.bytes), !jpeg.isEmpty
        else { return nil }
        return jpeg
    }

    func deliver(_ reply: MergeSuggestions) { merges.resolve(0, with: reply.pairs) }
    func deliver(_ reply: ClipTextEmbedding) { embeddings.resolve(reply.queryID, with: reply.embedding) }
    func deliver(_ reply: ThumbnailGenerated) { thumbnails.resolve(reply.path, with: reply) }

    func deliver(_ error: EngineError) {
        if error.kind == "find_merge_suggestions_failed" { merges.resolveAll(with: nil) }
    }

    /// A dead or restarted engine will never answer what it was asked.
    func abandonAll() {
        merges.resolveAll(with: nil)
        embeddings.resolveAll(with: nil)
        thumbnails.resolveAll(with: nil)
    }
}

@MainActor
final class ReplyWaiters<Key: Hashable & Sendable, Value: Sendable> {
    private let timeout: Duration
    private var waiting: [Key: [UUID: CheckedContinuation<Value?, Never>]] = [:]

    init(timeout: Duration) { self.timeout = timeout }

    var count: Int { waiting.values.reduce(0) { $0 + $1.count } }

    func wait(for key: Key, send: () -> Bool) async -> Value? {
        let token = UUID()
        let timer = Task { [weak self, timeout] in
            try? await Task.sleep(for: timeout)
            self?.resume(key, token: token, with: nil)
        }
        defer { timer.cancel() }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                guard !Task.isCancelled else { continuation.resume(returning: nil); return }
                waiting[key, default: [:]][token] = continuation
                if !send() { resume(key, token: token, with: nil) }
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.resume(key, token: token, with: nil) }
        }
    }

    func resolve(_ key: Key, with value: Value?) {
        guard let continuations = waiting.removeValue(forKey: key) else { return }
        for continuation in continuations.values { continuation.resume(returning: value) }
    }

    func resolveAll(with value: Value?) {
        let all = waiting
        waiting.removeAll()
        for continuations in all.values {
            for continuation in continuations.values { continuation.resume(returning: value) }
        }
    }

    private func resume(_ key: Key, token: UUID, with value: Value?) {
        guard let continuation = waiting[key]?.removeValue(forKey: token) else { return }
        if waiting[key]?.isEmpty == true { waiting.removeValue(forKey: key) }
        continuation.resume(returning: value)
    }
}
