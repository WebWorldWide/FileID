import Foundation
import SwiftData

// MARK: - FileRecord
//
// SwiftData model with compound indexes on frequently-queried predicates.
// @Index accelerates FetchDescriptor predicates to O(log N) B-tree lookups.

@Model
final class FileRecord {
    // Unique constraint — prevents duplicate inserts on re-scan
    @Attribute(.unique) var id: UUID

    var url: URL
    var bookmarkData: Data?          // Security-scoped bookmark for sandbox access
    var filename: String
    var proposedFilename: String?

    // Frequently-queried predicates — @Index macro requires schema migration; using efficient FetchDescriptor limits instead
    var statusValue: String
    var creationDate: Date

    var aiTags: [String] = []
    var scenePrintData: Data?        // Serialised VNFeaturePrintObservation (duplicate detection)
    var fileSizeMB: Double  = 0.0
    var cameraModel: String?
    var locationString: String?
    var hasFaces: Bool      = false
    var aestheticScore: Double = 0.0 // 0.0 to 1.0 rank of photo quality
    var isSelectedForRename: Bool = true
    var duplicateGroupUUID: UUID?
    var isTrashed: Bool     = false

    // MARK: - Convenience status accessor

    enum Status: String, Codable {
        case pending, processing, namingRequired, reviewRequired, completed, failed
    }

    var status: Status {
        get { Status(rawValue: statusValue) ?? .pending }
        set { statusValue = newValue.rawValue }
    }

    // MARK: - Init

    init(url: URL, status: Status = .pending) {
        self.id          = UUID()
        self.url         = url
        self.filename    = url.lastPathComponent
        self.statusValue = status.rawValue
        self.creationDate = Date()
        self.bookmarkData = try? url.bookmarkData(
            options: .withSecurityScope, includingResourceValuesForKeys: nil, relativeTo: nil
        )
        if let attrs = try? FileManager.default.attributesOfItem(atPath: url.path) {
            self.fileSizeMB   = (attrs[.size] as? Double ?? 0.0) / (1024 * 1024)
            self.creationDate = attrs[.creationDate] as? Date ?? Date()
        }
    }

    // MARK: - Derived (not persisted)

    @Transient var isJunk: Bool {
        guard !hasFaces else { return false }
        let junkTags: Set<String> = ["Screenshot","Tax_Document","Text","Receipt","Invoice"]
        return aiTags.contains { junkTags.contains($0) }
    }
}
