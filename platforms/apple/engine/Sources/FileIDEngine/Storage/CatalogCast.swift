import Foundation
import GRDB
import FileIDShared

enum CatalogCast {
    static func hits(_ db: GRDB.Database, fileID: Int64, faceModel: String) throws -> [CatalogHit] {
        guard let file = try Row.fetchOne(db, sql: "SELECT path_text,kind,size_bytes,modified_at FROM files WHERE id=? AND failed=0", arguments: [fileID]) else { return [] }
        let path: String = file["path_text"]
        let revision = try CatalogStore.revision(db, fileID: fileID)
        guard FaceCacheProvenance.currentSourceRevision(path: path) == revision else { return [] }
        let rows = try Row.fetchAll(db, sql: """
            SELECT o.id,o.person_id,o.start_seconds,o.user_edited,o.model_version,
                   p.name,p.title,p.first_name,p.middle_name,p.last_name,p.suffix,
                   fp.arcface_embedding,fp.embedding_model,fp.processing_version,fp.source_revision,
                   r.revision AS catalog_revision,f.size_bytes,f.modified_at
            FROM catalog_observations o JOIN persons p ON p.id=o.person_id
            JOIN files f ON f.id=o.file_id JOIN catalog_revisions r ON r.file_id=f.id
            LEFT JOIN face_prints fp ON o.id='faceprint:' || fp.id
            WHERE o.file_id=? AND o.stale=0 AND o.source_revision=? AND IFNULL(p.is_unknown,0)=0
              AND (fp.id IS NULL OR fp.excluded=0) AND (o.user_edited=1 OR fp.id IS NOT NULL)
            ORDER BY p.id,o.start_seconds,o.id LIMIT 500
            """, arguments: [fileID,revision])
        var seen = Set<String>()
        return rows.compactMap { row in
            let reviewed: Bool = row["user_edited"]
            if !reviewed {
                guard let embedding: Data = row["arcface_embedding"], FaceCacheProvenance.isCurrent(
                    embedding: embedding, model: row["embedding_model"], processing: row["processing_version"],
                    sourceRevision: row["source_revision"], catalogRevision: row["catalog_revision"],
                    size: row["size_bytes"], modified: row["modified_at"], selectedModel: faceModel,
                    selectedProcessing: FaceAnalysisCache.processingVersion) else { return nil }
            }
            let names = ["title","first_name","middle_name","last_name","suffix"]
                .compactMap { (row[$0] as String?)?.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
            let name = names.isEmpty ? (row["name"] as String? ?? "") : names.joined(separator: " ")
            guard !name.isEmpty, name.range(of: "^(person|unknown)([ _-]*[0-9]+)?$", options: [.regularExpression,.caseInsensitive]) == nil else { return nil }
            let time: Double? = row["start_seconds"]
            guard time == nil || (time!.isFinite && time! >= 0) else { return nil }
            let personID: Int64 = row["person_id"]
            guard seen.insert("\(personID):\(time.map(String.init(describing:)) ?? "sample")").inserted else { return nil }
            return CatalogHit(fileID: fileID,path: path,kind: "person",text: name + (reviewed ? " · Reviewed" : " · Detected"),
                              evidenceID: "person:" + (row["id"] as String),startSeconds: time)
        }
    }
}
