import Foundation
import GRDB
import FileIDShared

/// Reads finished image rows so a later scan can give their analysis to a
/// byte-identical file instead of running the models again.
enum TwinCatalogStore {
    /// The shared engine's `COMPLETE_IMAGE_SQL` (pipeline/twin_reuse.rs), plus:
    /// nothing still owed to the row by the after-scan catch-up.
    static let completeImageSQL = """
        files.kind = 'image' AND files.failed = 0 AND files.content_hash IS NOT NULL
        AND length(files.content_hash) = 32
        AND (EXISTS (SELECT 1 FROM tags t WHERE t.file_id = files.id AND t.source = 'auto')
             OR EXISTS (SELECT 1 FROM clip_embeddings c WHERE c.file_id = files.id))
        AND NOT EXISTS (SELECT 1 FROM scan_catchup s WHERE s.file_id = files.id)
        """

    /// Half the hash and the size: enough to say "maybe in the catalog" at 24
    /// bytes a row; the row read then compares the whole hash.
    struct Fingerprint: Hashable, Sendable {
        let head: UInt64
        let tail: UInt64
        let size: Int64

        init?(hash: Data, size: Int64) {
            guard hash.count == 32 else { return nil }
            head = hash.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 0, as: UInt64.self) }
            tail = hash.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: 8, as: UInt64.self) }
            self.size = size
        }
    }

    /// The models a row must have come from to stand in for a fresh analysis.
    struct Provenance: Sendable {
        var clipModel: String
        var clipBytes: Int
        var clipLoaded: Bool
        var taggerLoaded: Bool
        var faceProcessing: String
        var faceModel: String?

        static var current: Provenance {
            Provenance(
                clipModel: CLIPEmbeddingSpace.modelID, clipBytes: CLIPEmbeddingSpace.dimension * 4,
                clipLoaded: MobileCLIPService.shared.canEmbed, taggerLoaded: RamPlusService.shared.isReady,
                faceProcessing: FaceAnalysisCache.processingVersion, faceModel: ArcFaceService.shared.modelVersion)
        }
    }

    struct Face: Sendable, Equatable {
        var print: Data
        var bbox: String
        var quality: Double?
        var excluded: Bool
    }

    struct Row: Sendable {
        var path: String
        var phash: UInt64?
        var aesthetic: Double?
        var hasFaces: Bool
        var cameraModel: String?
        var locationLat: Double?
        var locationLon: Double?
        /// Auto tags in the order the scan stored them.
        var tags: [(tag: String, score: Double?)]
        var ocrText: String?
        var ocrHandwritten: Bool
        var clip: Data?
        var faces: [Face]
    }

    static func knownFingerprints(_ db: GRDB.Database) throws -> Set<Fingerprint> {
        var known = Set<Fingerprint>()
        let rows = try GRDB.Row.fetchCursor(db, sql: """
            SELECT content_hash, size_bytes FROM files
            WHERE \(completeImageSQL) AND size_bytes >= ? AND size_bytes <= ?
            """, arguments: [TwinReuse.minBytes, Int64(ContentHash.fullHashMaxBytes)])
        while let row = try rows.next() {
            if let print = Fingerprint(hash: row[0], size: row[1]) { known.insert(print) }
        }
        return known
    }

    /// A finished row with exactly these bytes whose analysis came from the
    /// models in `provenance`; the row at `preferPath` first. nil when there is none.
    static func row(
        _ db: GRDB.Database, hash: Data, size: Int64, preferPath: String, provenance: Provenance
    ) throws -> Row? {
        let candidates = try GRDB.Row.fetchAll(db, sql: """
            SELECT id, path_text, phash, aesthetic, has_faces, camera_model, location_lat, location_lon
            FROM files
            WHERE content_hash = ? AND size_bytes = ? AND \(completeImageSQL)
            ORDER BY (path_text = ?) DESC, id LIMIT 4
            """, arguments: [hash, size, preferPath])
        for file in candidates {
            if let row = try analysis(db, file: file, provenance: provenance) { return row }
        }
        return nil
    }

    private static func analysis(_ db: GRDB.Database, file: GRDB.Row, provenance: Provenance) throws -> Row? {
        let id: Int64 = file["id"]

        var clip: Data?
        if let stored = try GRDB.Row.fetchOne(
            db, sql: "SELECT embedding, model FROM clip_embeddings WHERE file_id = ?", arguments: [id]) {
            let blob: Data = stored["embedding"]
            let model: String = stored["model"]
            guard model == provenance.clipModel, blob.count == provenance.clipBytes else { return nil }
            clip = blob
        } else if provenance.clipLoaded {
            return nil
        }

        let tags = try GRDB.Row.fetchAll(
            db, sql: "SELECT tag, score FROM tags WHERE file_id = ? AND source = 'auto' ORDER BY rowid", arguments: [id]
        ).map { (tag: $0["tag"] as String, score: $0["score"] as Double?) }
        if provenance.taggerLoaded, !tags.contains(where: { $0.score != nil }) { return nil }

        var faces: [Face] = []
        for face in try GRDB.Row.fetchAll(db, sql: """
            SELECT print_data, bbox, face_quality, excluded, embedding_model, processing_version
            FROM face_prints WHERE file_id = ? ORDER BY id
            """, arguments: [id]) {
            if let processing = face["processing_version"] as String?, processing != provenance.faceProcessing { return nil }
            if let model = face["embedding_model"] as String?, let current = provenance.faceModel, model != current {
                return nil
            }
            faces.append(Face(
                print: face["print_data"] ?? Data(), bbox: face["bbox"], quality: face["face_quality"],
                excluded: (face["excluded"] as Int64? ?? 0) != 0))
        }

        let text = try GRDB.Row.fetchOne(
            db, sql: "SELECT text, handwritten FROM ocr_text WHERE file_id = ? AND source = 'ocr'", arguments: [id])
        let stored: Int64? = file["phash"]
        return Row(
            path: file["path_text"],
            phash: stored.map { UInt64(bitPattern: $0) },
            aesthetic: file["aesthetic"],
            hasFaces: (file["has_faces"] as Int64? ?? 0) != 0,
            cameraModel: file["camera_model"],
            locationLat: file["location_lat"],
            locationLon: file["location_lon"],
            tags: tags,
            ocrText: text?["text"],
            ocrHandwritten: (text?["handwritten"] as Int64? ?? 0) != 0,
            clip: clip,
            faces: faces)
    }
}
