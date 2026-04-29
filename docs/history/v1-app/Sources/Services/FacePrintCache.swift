import Foundation

// MARK: - FacePrintCache

// On-disk face-print storage during scan, keyed by FileRecord.id. Lives in
// ~/Library/Caches/FileID/faceprints so SwiftData doesn't pay to load blobs
// on every FileRecord fetch. Wiped after the clustering pass.

enum FacePrintCache {
    private static let folderName = "FileID/faceprints"

    private static let baseURL: URL = {
        let base = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)
            .first
            ?? FileManager.default.temporaryDirectory
        let dir = base.appendingPathComponent(folderName, isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }()

    // Dedicated serial queue so scan workers don't stall on the disk write.
    // Archival + fsync happens here; scan thread just hands off the bytes.
    private static let writeQueue = DispatchQueue(
        label: "FileID.FacePrintCache.write", qos: .utility
    )

    static func store(_ id: UUID, prints: [Data]) {
        guard !prints.isEmpty else { return }
        let url = baseURL.appendingPathComponent("\(id.uuidString).bin")
        writeQueue.async {
            if let archived = try? NSKeyedArchiver.archivedData(
                withRootObject: prints as NSArray, requiringSecureCoding: true
            ) {
                try? archived.write(to: url, options: .atomic)
            }
        }
    }

    static func load(_ id: UUID) -> [Data] {
        let url = baseURL.appendingPathComponent("\(id.uuidString).bin")
        guard let data = try? Data(contentsOf: url) else { return [] }
        let classes: [AnyClass] = [NSArray.self, NSData.self]
        guard let arr = try? NSKeyedUnarchiver.unarchivedObject(
            ofClasses: classes, from: data
        ) as? [Data] else { return [] }
        return arr
    }

    static func allCachedIDs() -> [UUID] {
        guard let entries = try? FileManager.default.contentsOfDirectory(
            at: baseURL, includingPropertiesForKeys: nil, options: [.skipsHiddenFiles]
        ) else { return [] }
        return entries.compactMap { UUID(uuidString: $0.deletingPathExtension().lastPathComponent) }
    }

    static func remove(_ id: UUID) {
        let url = baseURL.appendingPathComponent("\(id.uuidString).bin")
        try? FileManager.default.removeItem(at: url)
    }

    static func removeAll() {
        guard let entries = try? FileManager.default.contentsOfDirectory(
            at: baseURL, includingPropertiesForKeys: nil, options: [.skipsHiddenFiles]
        ) else { return }
        for url in entries { try? FileManager.default.removeItem(at: url) }
    }

    // Fire-and-forget variant for wipe-for-new-scan: a 17 K-file serial delete
    // on the main actor was stalling the next scan's Discovery by tens of
    // seconds. Uses the same writeQueue as store() so enqueue ordering holds.
    static func removeAllAsync() {
        let dir = baseURL
        writeQueue.async {
            guard let entries = try? FileManager.default.contentsOfDirectory(
                at: dir, includingPropertiesForKeys: nil, options: [.skipsHiddenFiles]
            ) else { return }
            for url in entries { try? FileManager.default.removeItem(at: url) }
        }
    }
}
