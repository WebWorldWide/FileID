import Foundation
import GRDB
import FileIDShared

enum FaceAnalysisCache {
    private static let vectorValid = DatabaseFunction("fileid_face_vector_valid", argumentCount: 1, pure: true) { arguments in
        guard case .blob(let data) = arguments[0].storage else { return false }
        return normalized(data)
    }

    static var processingVersion: String { FaceAlign.enabled ? "macos-landmark-overlap-v3-orientation" : "macos-bbox-crop-v3-orientation" }

    struct Input: Sendable {
        let id: Int64
        let fileID: Int64
        let bbox: String
        let path: String
        let size: Int64
        let modified: Double?
        let previousProcessingVersion: String?
        var revision: String { "\(size):\(modified.map { String($0.bitPattern) } ?? "unknown")" }

        func currentIdentity() -> ExactFileDigest.CacheKey? {
            let url = URL(fileURLWithPath: path)
            guard let modified, modified.isFinite,
                  let values = try? url.resourceValues(forKeys: [.fileSizeKey, .contentModificationDateKey]),
                  let currentSize = values.fileSize, Int64(currentSize) == size, values.contentModificationDate?.timeIntervalSince1970 == modified else { return nil }
            return ExactFileDigest.cacheKey(url: url, expectedSize: size)
        }
    }

    static func pending(_ db: GRDB.Database, modelVersion: String, limit: Int, afterFaceID: Int64 = 0, now: Double = Date().timeIntervalSince1970) throws -> [Input] {
        db.add(function: vectorValid)
        return try Row.fetchAll(db, sql: """
            SELECT fp.id,fp.file_id,fp.bbox,f.path_text,f.size_bytes,f.modified_at,fp.processing_version
            FROM face_prints fp JOIN files f ON f.id=fp.file_id
            LEFT JOIN catalog_revisions r ON r.file_id=f.id
            WHERE f.failed=0 AND fp.excluded=0 AND fp.id>? AND (LENGTH(COALESCE(fp.arcface_embedding,X''))!=512 OR fp.embedding_model IS NULL OR fp.embedding_model!=?
               OR fp.processing_version IS NULL OR fp.processing_version!=?
               OR fp.source_revision IS NULL OR r.revision IS NULL OR fp.source_revision!=r.revision OR fileid_face_vector_valid(fp.arcface_embedding)=0)
            AND NOT EXISTS (
                SELECT 1 FROM face_refresh_failures q WHERE q.face_id=fp.id
                AND q.embedding_model=? AND q.processing_version=? AND q.source_path=f.path_text
                AND q.source_size=f.size_bytes AND q.source_modified IS f.modified_at
                AND q.bbox=fp.bbox AND q.retry_after>?
            )
            ORDER BY fp.id LIMIT ?
            """, arguments: [afterFaceID,modelVersion,processingVersion,modelVersion,processingVersion,now,limit]).map { row in
                Input(id: row["id"], fileID: row["file_id"], bbox: row["bbox"], path: row["path_text"], size: row["size_bytes"], modified: row["modified_at"], previousProcessingVersion: row["processing_version"])
            }
    }

    static func persist(_ db: GRDB.Database, input: Input, embedding: Data, modelVersion: String) throws -> Bool {
        guard !modelVersion.isEmpty, modelVersion.count <= 150, normalized(embedding),
              let file = try Row.fetchOne(db, sql: "SELECT path_text,size_bytes,modified_at FROM files WHERE id=?", arguments: [input.fileID]),
              file["path_text"] as String == input.path,
              file["size_bytes"] as Int64 == input.size,
              (file["modified_at"] as Double?) == input.modified,
              try Int.fetchOne(db, sql: "SELECT COUNT(*) FROM face_prints WHERE id=? AND file_id=? AND bbox=? AND excluded=0", arguments: [input.id,input.fileID,input.bbox]) == 1 else { return false }
        try db.execute(sql: "INSERT INTO catalog_revisions(file_id,revision,processing_version,updated_at) VALUES(?,?,'file-stat-v1',?) ON CONFLICT(file_id) DO UPDATE SET revision=excluded.revision,processing_version=excluded.processing_version,updated_at=excluded.updated_at WHERE catalog_revisions.revision!=excluded.revision", arguments: [input.fileID,input.revision,Date().timeIntervalSince1970])
        try db.execute(sql: """
            UPDATE face_prints SET arcface_embedding=?,embedding_model=?,processing_version=?,source_revision=?
            WHERE id=? AND file_id=? AND bbox=? AND excluded=0
            """, arguments: [embedding,modelVersion,processingVersion,input.revision,input.id,input.fileID,input.bbox])
        let saved = db.changesCount == 1
        if saved {
            try db.execute(sql: "DELETE FROM face_refresh_failures WHERE face_id=?", arguments: [input.id])
        }
        return saved
    }

    static func recordFailure(_ db: GRDB.Database, input: Input, modelVersion: String,
                              reason: String, now: Double = Date().timeIntervalSince1970) throws {
        guard now.isFinite, !modelVersion.isEmpty, modelVersion.count <= 150,
              ["source_unavailable_or_changed", "no_embedding"].contains(reason),
              try Int.fetchOne(db, sql: """
                SELECT COUNT(*) FROM face_prints fp JOIN files f ON f.id=fp.file_id
                WHERE fp.id=? AND fp.file_id=? AND fp.bbox=? AND fp.excluded=0
                AND f.path_text=? AND f.size_bytes=? AND f.modified_at IS ?
                """, arguments: [input.id,input.fileID,input.bbox,input.path,input.size,input.modified]) == 1
        else { return }
        let previous = try Int.fetchOne(db, sql: """
            SELECT attempts FROM face_refresh_failures WHERE face_id=? AND embedding_model=?
            AND processing_version=? AND source_path=? AND source_size=? AND source_modified IS ? AND bbox=?
            """, arguments: [input.id,modelVersion,processingVersion,input.path,input.size,input.modified,input.bbox]) ?? 0
        let attempts = min(previous + 1, 16)
        let retryAfter = now + min(3600, 60 * pow(2, Double(attempts - 1)))
        try db.execute(sql: """
            INSERT INTO face_refresh_failures(face_id,embedding_model,processing_version,source_revision,
                source_path,source_size,source_modified,bbox,attempts,retry_after,reason,updated_at)
            VALUES(?,?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(face_id) DO UPDATE SET
                embedding_model=excluded.embedding_model,processing_version=excluded.processing_version,
                source_revision=excluded.source_revision,source_path=excluded.source_path,
                source_size=excluded.source_size,source_modified=excluded.source_modified,bbox=excluded.bbox,
                attempts=excluded.attempts,retry_after=excluded.retry_after,reason=excluded.reason,updated_at=excluded.updated_at
            """, arguments: [input.id,modelVersion,processingVersion,input.revision,input.path,input.size,
                              input.modified,input.bbox,attempts,retryAfter,reason,now])
    }

    static func normalized(_ data: Data) -> Bool {
        guard data.count == 128 * 4 else { return false }
        var sum = 0.0
        for offset in stride(from: 0, to: data.count, by: 4) {
            let bits = data.withUnsafeBytes { $0.loadUnaligned(fromByteOffset: offset, as: UInt32.self).littleEndian }
            let value = Float(bitPattern: bits)
            guard value.isFinite else { return false }
            sum += Double(value) * Double(value)
        }
        return (0.95...1.05).contains(sum)
    }
}
