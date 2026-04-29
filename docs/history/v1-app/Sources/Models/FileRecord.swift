import Foundation
import SwiftData

@Model
final class FileRecord {
    #Index<FileRecord>(
        [\.statusValue],
        [\.creationDate],
        [\.isTrashed],
        [\.junkScore],
        [\.duplicateGroupUUID],
        [\.fileSizeMB],
        [\.pHashValue]
    )

    @Attribute(.unique) var id: UUID

    var url: URL
    // .externalStorage = SwiftData stores the blob in a sidecar file under
    // the store directory, not inline in the SQLite row. Keeps the WAL
    // small on libraries with many files (each bookmark is ~100-500 B but
    // adds up across 100K rows; same rationale as clipEmbedding below).
    @Attribute(.externalStorage) var bookmarkData: Data?
    var filename: String
    var proposedFilename: String?

    var statusValue: String
    var creationDate: Date

    var aiTags: [String] = []
    // Note: `scenePrintData` was removed in Batch 6.5 (Vision scenePrint was
    // never enabled past testing). `facePrintsRawData` was removed in Batch 14
    // — face prints live in FacePrintCache (disk) and PersonRecord.featurePrintsData.
    var pHashValue: UInt64   = 0
    var fileSizeMB: Double  = 0.0
    var cameraModel: String?
    var locationString: String?
    var hasFaces: Bool      = false
    var aestheticScore: Double = 0.0
    var isSelectedForRename: Bool = true
    var duplicateGroupUUID: UUID?
    var isTrashed: Bool      = false
    var junkScore: Double    = 0.0
    var junkReasons: [String] = []

    // .externalStorage critical here — clipEmbedding is ~1 KB Data per record;
    // inline storage of 100 K rows = 100 MB inside the SQLite file. With
    // .externalStorage SwiftData writes one tiny sidecar file per record and
    // the SQLite row only carries a reference. WAL stays small; long-scan
    // saves stay fast.
    @Attribute(.externalStorage) var clipEmbedding: Data?
    @Attribute(.externalStorage) var deepAnalysis: String?

    // Populated by "shortcuts only" Folder Restructure — each entry is an
    // absolute path to a symlink pointing at `url`. Empty for files that were
    // physically moved or untouched.
    var shortcutPaths: [String] = []

    enum Status: String, Codable {
        case pending, processing, namingRequired, reviewRequired, completed, failed
    }

    var status: Status {
        get { Status(rawValue: statusValue) ?? .pending }
        set { statusValue = newValue.rawValue }
    }

    init(
        url: URL,
        status: Status = .pending,
        creationDate: Date? = nil,
        fileSizeBytes: Int? = nil
    ) {
        self.id           = UUID()
        self.url          = url
        self.filename     = url.lastPathComponent
        self.statusValue  = status.rawValue
        self.bookmarkData = nil

        if let creationDate, let fileSizeBytes {
            self.creationDate = creationDate
            self.fileSizeMB   = Double(fileSizeBytes) / 1_048_576
        } else {
            let rv = try? url.resourceValues(forKeys: [.creationDateKey, .fileSizeKey])
            self.creationDate = rv?.creationDate ?? Date()
            self.fileSizeMB   = Double(rv?.fileSize ?? 0) / 1_048_576
        }
    }

    @Transient var isJunk: Bool { junkScore >= 0.6 }
}

// @unchecked so FileRecord can cross the MediaProcessor TaskGroup boundary.
// All writes go through FileIDDataStore's single-writer @ModelActor.
extension FileRecord: @unchecked Sendable {}

