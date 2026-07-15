// Shared DB-row types — used by the read side (FileID app) and exposed
// over IPC where useful. The engine owns the schema and writes; the app
// reads via GRDB.
import Foundation

public struct FileRow: Sendable, Hashable, Identifiable, Codable {
    public let id: Int64
    public let pathText: String
    public let sizeBytes: Int64
    public let createdAt: Date?
    public let modifiedAt: Date?
    public let scannedAt: Date
    public let kind: String
    public let `extension`: String
    public let phash: Int64?
    public let aesthetic: Double?
    public let hasFaces: Bool
    public let hasText: Bool
    public let cameraModel: String?
    public let locationLat: Double?
    public let locationLon: Double?
    public let failed: Bool
    public let errorMessage: String?
    // Deep Analyze — populated only after the VLM has run on this file.
    public let vlmDescription: String?
    public let vlmProposedName: String?
    public let vlmModel: String?
    public let vlmAnalyzedAt: Date?

    public init(
        id: Int64, pathText: String, sizeBytes: Int64,
        createdAt: Date?, modifiedAt: Date?, scannedAt: Date,
        kind: String, extension ext: String, phash: Int64?,
        aesthetic: Double?, hasFaces: Bool, hasText: Bool,
        cameraModel: String?, locationLat: Double?, locationLon: Double?,
        failed: Bool, errorMessage: String?,
        vlmDescription: String? = nil, vlmProposedName: String? = nil,
        vlmModel: String? = nil, vlmAnalyzedAt: Date? = nil
    ) {
        self.id = id
        self.pathText = pathText
        self.sizeBytes = sizeBytes
        self.createdAt = createdAt
        self.modifiedAt = modifiedAt
        self.scannedAt = scannedAt
        self.kind = kind
        self.extension = ext
        self.phash = phash
        self.aesthetic = aesthetic
        self.hasFaces = hasFaces
        self.hasText = hasText
        self.cameraModel = cameraModel
        self.locationLat = locationLat
        self.locationLon = locationLon
        self.failed = failed
        self.errorMessage = errorMessage
        self.vlmDescription = vlmDescription
        self.vlmProposedName = vlmProposedName
        self.vlmModel = vlmModel
        self.vlmAnalyzedAt = vlmAnalyzedAt
    }

    public var url: URL { URL(fileURLWithPath: pathText) }

    public var sizeMB: Double { Double(sizeBytes) / 1_048_576 }

    public var displayDate: Date? { createdAt ?? modifiedAt }

    public var isImage: Bool { kind == "image" }
    public var isVideo: Bool { kind == "video" }
}

/// Duplicate group — files verified by a live full-file digest, or a
/// perceptual near-duplicate cluster when `isSimilar` is true (Cleanup's
/// "Similar" mode: dHash Hamming grouping — NOT byte-identical).
public struct DuplicateGroup: Sendable, Identifiable, Hashable {
    public let id: Int64           // exact: first 8 bytes of full digest; similar: min member file id
    public let files: [FileRow]    // sorted by keeperRank descending (best first)
    /// Exact cardinality/bytes for the whole group. `files` is a bounded
    /// interactive preview when a pathological group contains thousands of
    /// copies, so the preview and total counts may differ.
    public let totalFileCount: Int
    private let storedTotalBytes: Int64?
    /// True for perceptual near-duplicate groups. The Cleanup "Similar" view
    /// surfaces these with a "review before deleting — not identical" disclaimer
    /// and never pre-selects copies for deletion.
    public let isSimilar: Bool
    public init(
        id: Int64, files: [FileRow], isSimilar: Bool = false,
        totalFileCount: Int? = nil, totalBytes: Int64? = nil
    ) {
        self.id = id
        self.files = files
        self.isSimilar = isSimilar
        self.totalFileCount = totalFileCount ?? files.count
        self.storedTotalBytes = totalBytes
    }

    public var isTruncated: Bool { totalFileCount > files.count }
    public var totalBytes: Int64 {
        storedTotalBytes ?? files.reduce(0) { $0 + $1.sizeBytes }
    }
    public var reclaimableBytes: Int64 { totalBytes - (files.first?.sizeBytes ?? 0) }
    public var keeper: FileRow? { files.first }
    public var trashable: ArraySlice<FileRow> { files.dropFirst() }
}
