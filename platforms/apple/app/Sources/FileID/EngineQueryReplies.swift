import Foundation
import ImageIO
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
    static let faceReviewTimeout: Duration = .seconds(60)

    private let merges: ReplyWaiters<Int, [MergeSuggestionPair]>
    private let embeddings: ReplyWaiters<String, [Float]>
    private let thumbnails: ReplyWaiters<String, VideoFrameReply>
    private let faceReviews: ReplyWaiters<String, FaceReviewPage>

    init(mergeTimeout: Duration = mergeSuggestionsTimeout,
         embeddingTimeout: Duration = textEmbeddingTimeout,
         thumbnailTimeout: Duration = videoThumbnailTimeout,
         faceReviewTimeout: Duration = faceReviewTimeout) {
        merges = ReplyWaiters(timeout: mergeTimeout)
        embeddings = ReplyWaiters(timeout: embeddingTimeout)
        thumbnails = ReplyWaiters(timeout: thumbnailTimeout)
        faceReviews = ReplyWaiters(timeout: faceReviewTimeout)
    }

    var waiting: Int { merges.count + embeddings.count + thumbnails.count + faceReviews.count }

    /// Each face review command carries its own request id, which the reply echoes.
    func faceReview(send: (String) -> Bool) async -> FaceReviewPage? {
        let requestID = "review-" + UUID().uuidString.replacingOccurrences(of: "-", with: "")
        return await faceReviews.wait(for: requestID) { send(requestID) }
    }

    func deliver(_ reply: FaceReviewPage) { faceReviews.resolve(reply.requestID, with: reply) }

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
    /// `maxPixelSize` must already be clamped the way the engine clamps it.
    func videoThumbnail(path: String, modifiedAt: Double?, maxPixelSize: Int, send: () -> Bool) async -> Data? {
        guard let reply = await thumbnails.wait(for: path, tag: maxPixelSize, send: send),
              reply.modifiedAt == modifiedAt
        else { return nil }
        return reply.jpeg
    }

    func deliver(_ reply: MergeSuggestions) { merges.resolve(0, with: reply.pairs) }
    func deliver(_ reply: ClipTextEmbedding) { embeddings.resolve(reply.queryID, with: reply.embedding) }
    /// The reply doesn't echo the requested size, so it is read from the JPEG.
    /// Engines never upscale: a frame smaller than every waiting request means
    /// the source itself is that small, which answers all larger requests too.
    func deliver(_ reply: ThumbnailGenerated) {
        guard let jpeg = Data(base64Encoded: reply.bytes), let longSide = Self.longSide(of: jpeg) else {
            thumbnails.resolve(reply.path, with: nil)
            return
        }
        let frame = VideoFrameReply(modifiedAt: reply.modifiedAt, jpeg: jpeg)
        if thumbnails.hasWaiter(for: reply.path, where: { $0 == longSide }) {
            thumbnails.resolve(reply.path, with: frame) { $0 == longSide }
        } else {
            thumbnails.resolve(reply.path, with: frame) { $0 > longSide }
        }
    }

    nonisolated static func longSide(of jpeg: Data) -> Int? {
        guard let source = CGImageSourceCreateWithData(jpeg as CFData, nil),
              let properties = CGImageSourceCopyPropertiesAtIndex(source, 0, nil) as? [CFString: Any],
              let width = (properties[kCGImagePropertyPixelWidth] as? NSNumber)?.intValue,
              let height = (properties[kCGImagePropertyPixelHeight] as? NSNumber)?.intValue,
              width > 0, height > 0
        else { return nil }
        return max(width, height)
    }

    func deliver(_ error: EngineError) {
        if error.kind == "find_merge_suggestions_failed" { merges.resolveAll(with: nil) }
    }

    /// A dead or restarted engine will never answer what it was asked.
    func abandonAll() {
        merges.resolveAll(with: nil)
        embeddings.resolveAll(with: nil)
        thumbnails.resolveAll(with: nil)
        faceReviews.resolveAll(with: nil)
    }
}

struct VideoFrameReply: Sendable {
    let modifiedAt: Double?
    let jpeg: Data
}

/// Pending calls by key. `tag` tells apart waiters on one key that asked for
/// different things (a thumbnail size).
@MainActor
final class ReplyWaiters<Key: Hashable & Sendable, Value: Sendable> {
    private struct Waiter {
        let tag: Int
        let continuation: CheckedContinuation<Value?, Never>
    }

    private let timeout: Duration
    private var waiting: [Key: [UUID: Waiter]] = [:]

    init(timeout: Duration) { self.timeout = timeout }

    var count: Int { waiting.values.reduce(0) { $0 + $1.count } }

    func hasWaiter(for key: Key, where matches: (Int) -> Bool) -> Bool {
        waiting[key]?.values.contains { matches($0.tag) } ?? false
    }

    func wait(for key: Key, tag: Int = 0, send: () -> Bool) async -> Value? {
        let token = UUID()
        let timer = Task { [weak self, timeout] in
            try? await Task.sleep(for: timeout)
            self?.resume(key, token: token, with: nil)
        }
        defer { timer.cancel() }
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                guard !Task.isCancelled else { continuation.resume(returning: nil); return }
                waiting[key, default: [:]][token] = Waiter(tag: tag, continuation: continuation)
                if !send() { resume(key, token: token, with: nil) }
            }
        } onCancel: {
            Task { @MainActor [weak self] in self?.resume(key, token: token, with: nil) }
        }
    }

    func resolve(_ key: Key, with value: Value?, where matches: (Int) -> Bool = { _ in true }) {
        guard let waiters = waiting[key] else { return }
        let chosen = waiters.filter { matches($0.value.tag) }
        let rest = waiters.filter { !matches($0.value.tag) }
        if rest.isEmpty { waiting.removeValue(forKey: key) } else { waiting[key] = rest }
        for waiter in chosen.values { waiter.continuation.resume(returning: value) }
    }

    func resolveAll(with value: Value?) {
        let all = waiting
        waiting.removeAll()
        for waiters in all.values {
            for waiter in waiters.values { waiter.continuation.resume(returning: value) }
        }
    }

    private func resume(_ key: Key, token: UUID, with value: Value?) {
        guard let waiter = waiting[key]?.removeValue(forKey: token) else { return }
        if waiting[key]?.isEmpty == true { waiting.removeValue(forKey: key) }
        waiter.continuation.resume(returning: value)
    }
}
