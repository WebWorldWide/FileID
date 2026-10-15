import Foundation

public enum FaceCacheProvenance {
    public static func currentSourceRevision(path: String) -> String? {
        guard let values = try? URL(fileURLWithPath: path).resourceValues(
            forKeys: [.isRegularFileKey, .fileSizeKey, .contentModificationDateKey]),
            values.isRegularFile == true, let size = values.fileSize,
            let modified = values.contentModificationDate?.timeIntervalSince1970,
            size >= 0, modified.isFinite else { return nil }
        return sourceRevision(size: Int64(size), modified: modified)
    }

    public static var processingVersion: String {
        ProcessInfo.processInfo.environment["FILEID_FACE_ALIGN"] != "0"
            ? "macos-landmark-overlap-v3-orientation"
            : "macos-bbox-crop-v3-orientation"
    }

    public static func sourceRevision(size: Int64, modified: Double?) -> String {
        "\(size):\(modified.map { String($0.bitPattern) } ?? "unknown")"
    }

    public static func normalizedEmbedding(_ data: Data) -> Bool {
        guard data.count == 128 * 4 else { return false }
        var sum = 0.0
        for offset in stride(from: 0, to: data.count, by: 4) {
            let bits = data.withUnsafeBytes {
                $0.loadUnaligned(fromByteOffset: offset, as: UInt32.self).littleEndian
            }
            let value = Float(bitPattern: bits)
            guard value.isFinite else { return false }
            sum += Double(value) * Double(value)
        }
        return (0.95...1.05).contains(sum)
    }

    public static func isCurrent(
        embedding: Data?, model: String?, processing: String?,
        sourceRevision: String?, catalogRevision: String?,
        size: Int64?, modified: Double?,
        selectedModel: String, selectedProcessing: String = processingVersion
    ) -> Bool {
        guard !selectedModel.isEmpty, selectedModel.count <= 150,
              !selectedProcessing.isEmpty, model == selectedModel,
              processing == selectedProcessing,
              let size, size >= 0, let modified, modified.isFinite,
              let sourceRevision, sourceRevision == catalogRevision,
              sourceRevision == Self.sourceRevision(size: size, modified: modified),
              let embedding, normalizedEmbedding(embedding)
        else { return false }
        return true
    }
}
