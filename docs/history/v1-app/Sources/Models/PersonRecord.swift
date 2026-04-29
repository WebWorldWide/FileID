import Foundation
import SwiftData
import Vision

@Model
final class PersonRecord {
    @Attribute(.unique) var id: UUID
    var name: String?
    // .externalStorage — a single face JPEG is ~5-15 KB; at 2 K identities
    // that's 10-30 MB of JPEG bytes that would otherwise inflate every
    // SwiftData save. Sidecar storage keeps WAL tiny.
    @Attribute(.externalStorage) var representativeFaceCropData: Data?

    // .externalStorage — each feature print is ~2 KB Data; 50 prints/identity
    // × 2 K identities = ~200 MB. Inline storage would dominate WAL fsync
    // time on every batch save. Sidecar files: SwiftData reads them lazily.
    @Attribute(.externalStorage) var featurePrintsData: [Data] = []

    var faceCount: Int = 1

    // ≤8 URLs for card thumbnails (subset of fileIDs).
    var sampleFileURLs: [URL] = []

    // Authoritative set of FileRecord IDs in this cluster. Populated by
    // FaceClusteringService.clusterSync.
    var fileIDs: [UUID] = []

    init(id: UUID = UUID(), name: String? = nil, representativeFaceCropData: Data? = nil) {
        self.id = id
        self.name = name
        self.representativeFaceCropData = representativeFaceCropData
    }
}

// @Model generates @available(*, unavailable) Sendable conformance. We opt in with @unchecked
// so PersonRecord can be used across PeopleView's Task closures on @MainActor.
// The "redundant conformance" warning from @Model is expected and harmless.
extension PersonRecord: @unchecked Sendable {}

