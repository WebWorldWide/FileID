import FileIDVLMCore
import Foundation
import GRDB
import ImageIO
import UniformTypeIdentifiers
import FileIDShared
import FileIDImaging

/// Read-only query commands whose replies mirror the shared Rust engine:
/// merge suggestions, CLIP query vectors and on-demand video thumbnails.
enum QueryCommands {
    static let clipText = CLIPTextEncoder(log: { message in
        JSONLog.shared.warn(ev: "clip_text", error: message)
    })

    /// Loads the search text model soon after launch, so the first search or
    /// Chat question does not wait for it. Skipped when memory is tight; the
    /// first query then loads it as before. Runs at query priority: a query
    /// that arrives mid-load waits on this load, and at utility priority a
    /// busy Mac stretched it from 0.3 s to over 10 s.
    static func prewarmTextEncoder(availableMB: @escaping @Sendable () -> Int = Hardware.availableMemoryMB) {
        guard clipText.isInstalled else { return }
        Task.detached(priority: .userInitiated) {
            guard availableMB() >= textEncoderPrewarmMB else { return }
            _ = clipText.load()
        }
    }

    static let textEncoderPrewarmMB = 2048

    static let thumbnailLongSide = 192
    static let thumbnailLongSideRange = 64...2048
    private static let thumbnailGate = AsyncSemaphore(value: 2)

    static func mergeSuggestions(database: Database) async -> IPCEvent.Payload {
        let model = ArcFaceService.shared.modelVersion ?? PersonMergeSuggestions.manifestFaceModel
        do {
            let pairs = try await database.pool.read { db in
                try PersonMergeSuggestions.find(in: db, selectedModel: model)
            }
            return .mergeSuggestions(MergeSuggestions(pairs: pairs))
        } catch {
            JSONLog.shared.warn(ev: "find_merge_suggestions_failed", error: "\(error)")
            return .error(EngineError(kind: "find_merge_suggestions_failed",
                                      message: "Find merge suggestions failed: \(error)"))
        }
    }

    /// An unavailable encoder answers with an empty vector so search falls
    /// back to keywords at once instead of waiting for a reply that never comes.
    static func textEmbedding(query: String, queryID: String) -> IPCEvent.Payload {
        .clipTextEmbedding(ClipTextEmbedding(queryID: queryID, query: query, embedding: clipText.embedText(query) ?? []))
    }

    static func imageEmbedding(database: Database, fileID: Int64, queryID: String) async -> IPCEvent.Payload {
        let blob = try? await database.pool.read { db in
            try Data.fetchOne(db, sql: "SELECT embedding FROM clip_embeddings WHERE file_id = ?", arguments: [fileID])
        }
        let embedding = blob.flatMap(CLIPEmbeddingSpace.vector(from:)) ?? []
        return .clipTextEmbedding(ClipTextEmbedding(queryID: queryID, query: "file:\(fileID)", embedding: embedding))
    }

    /// Nil when the video can't be decoded; the caller then emits nothing and
    /// the app keeps its placeholder, as the Rust engine does.
    static func videoThumbnail(path: String, modifiedAt: Double?, maxPixelSize: Int? = nil) async -> IPCEvent.Payload? {
        await thumbnailGate.wait()
        let jpeg = await videoThumbnailJPEG(url: URL(fileURLWithPath: path), longSide: thumbnailLongSide(for: maxPixelSize))
        await thumbnailGate.signal()
        guard let jpeg else {
            JSONLog.shared.warn(ev: "video_thumbnail_failed", path: redactPathForLog(path))
            return nil
        }
        return .thumbnailGenerated(ThumbnailGenerated(path: path, modifiedAt: modifiedAt, bytes: jpeg.base64EncodedString()))
    }

    static func thumbnailLongSide(for requested: Int?) -> Int {
        guard let requested else { return thumbnailLongSide }
        return min(max(requested, thumbnailLongSideRange.lowerBound), thumbnailLongSideRange.upperBound)
    }

    static func videoThumbnailJPEG(url: URL, longSide: Int = thumbnailLongSide) async -> Data? {
        guard FileManager.default.isReadableFile(atPath: url.path),
              let frame = await DeepAnalyze.extractVideoKeyframe(url: url, maxPixelSize: longSide)
        else { return nil }
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, UTType.jpeg.identifier as CFString, 1, nil) else { return nil }
        CGImageDestinationAddImage(destination, frame, [kCGImageDestinationLossyCompressionQuality: 0.8] as CFDictionary)
        return CGImageDestinationFinalize(destination) ? data as Data : nil
    }
}
