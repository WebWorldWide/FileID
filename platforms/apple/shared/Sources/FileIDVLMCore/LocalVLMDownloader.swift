import Foundation
import MLXLMCommon

struct LocalVLMDownloader: Downloader {
    let repo: String
    let directory: URL

    func download(
        id: String,
        revision: String?,
        matching patterns: [String],
        useLatest: Bool,
        progressHandler: @Sendable @escaping (Progress) -> Void
    ) async throws -> URL {
        // VLMDownloader verified the pinned revision before this local-only factory call.
        guard id == repo else {
            throw NSError(domain: "FileID.LocalVLMDownloader", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "Model requested an unexpected repository."])
        }
        return directory
    }
}
