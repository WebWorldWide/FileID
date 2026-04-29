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

/// Duplicate group — files sharing the same phash.
public struct DuplicateGroup: Sendable, Identifiable, Hashable {
    public let id: Int64           // phash
    public let files: [FileRow]    // sorted by keeperRank descending (best first)
    public init(id: Int64, files: [FileRow]) {
        self.id = id
        self.files = files
    }

    public var totalBytes: Int64 { files.reduce(0) { $0 + $1.sizeBytes } }
    public var reclaimableBytes: Int64 { totalBytes - (files.first?.sizeBytes ?? 0) }
    public var keeper: FileRow? { files.first }
    public var trashable: ArraySlice<FileRow> { files.dropFirst() }
}
