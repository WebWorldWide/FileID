import Foundation
import GRDB
import FileIDShared

struct CatalogNamingContext: Sendable, Equatable {
    struct Label: Sendable, Equatable {
        let text: String
        let source: String
    }

    let fileID: Int64
    let path: String
    let revision: String
    let faceModel: String?
    let subjects: [String]
    let label: Label?

    static func load(_ db: GRDB.Database, fileID: Int64, faceModel: String?) throws -> CatalogNamingContext {
        guard let file = try Row.fetchOne(db, sql: "SELECT path_text,size_bytes,modified_at FROM files WHERE id=?", arguments: [fileID]) else {
            throw CatalogStore.InvalidRequest()
        }
        let path: String = file["path_text"]
        let size: Int64 = file["size_bytes"]
        let modified: Double? = file["modified_at"]
        let revision = try CatalogStore.revision(db, fileID: fileID)
        let validStat = size >= 0 && modified?.isFinite == true
            && FaceCacheProvenance.currentSourceRevision(path: path) == revision
        guard validStat else { throw CatalogStore.InvalidRequest() }
        let chapterLabels = try String.fetchAll(db, sql: "SELECT title FROM catalog_chapters WHERE file_id=? AND user_edited=1 AND stale=0 AND source_revision=? ORDER BY id", arguments: [fileID,revision])
        let eventLabels = try String.fetchAll(db, sql: """
            SELECT e.title FROM catalog_events e
            JOIN catalog_event_files ef ON ef.event_id=e.id
            JOIN catalog_take_scores s ON s.event_id=e.id AND s.file_id=ef.file_id
            WHERE ef.file_id=? AND e.user_edited=1 AND s.model_version='user'
              AND s.stale=0 AND s.source_revision=? AND s.confidence=1
            ORDER BY e.id
            """, arguments: [fileID,revision])
        let chapters = usableLabels(chapterLabels)
        let events = usableLabels(eventLabels)
        let label: Label? = chapters.count == 1 ? Label(text: chapters[0], source: "chapter")
            : chapters.isEmpty && events.count == 1 ? Label(text: events[0], source: "event") : nil
        var subjects: [String] = []
        if let faceModel, !faceModel.isEmpty {
            let faces = try Row.fetchAll(db, sql: """
                SELECT p.title,p.first_name,p.name,fp.arcface_embedding,fp.embedding_model,fp.processing_version,fp.source_revision,r.revision,f.size_bytes,f.modified_at
                FROM face_prints fp JOIN persons p ON p.id=fp.person_id
                JOIN files f ON f.id=fp.file_id JOIN catalog_revisions r ON r.file_id=f.id
                WHERE fp.file_id=? AND fp.excluded=0 AND f.failed=0 AND IFNULL(p.is_unknown,0)=0
                  AND fp.embedding_model=? AND fp.processing_version=? AND fp.source_revision=? AND r.revision=?
                ORDER BY fp.id
                """, arguments: [fileID,faceModel,FaceAnalysisCache.processingVersion,revision,revision])
            for row in faces {
                guard let embedding: Data = row["arcface_embedding"],
                      FaceCacheProvenance.isCurrent(embedding: embedding, model: row["embedding_model"], processing: row["processing_version"],
                                                    sourceRevision: row["source_revision"], catalogRevision: row["revision"],
                                                    size: row["size_bytes"], modified: row["modified_at"], selectedModel: faceModel,
                                                    selectedProcessing: FaceAnalysisCache.processingVersion) else { continue }
                let title = (row["title"] as String?)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                let first = (row["first_name"] as String?)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                let legacy = (row["name"] as String?)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
                let name = !title.isEmpty && !first.isEmpty ? title + " " + first : !first.isEmpty ? first : !title.isEmpty ? title : legacy
                if !name.isEmpty, name.range(of: "^(person|unknown)([ _-]*[0-9]+)?$", options: [.regularExpression, .caseInsensitive]) == nil { subjects.append(name) }
            }
        }
        return CatalogNamingContext(fileID: fileID, path: path, revision: revision, faceModel: faceModel,
                                    subjects: Array(Set(subjects)).sorted(), label: label)
    }

    var proposedName: String? {
        guard let label else { return nil }
        return SmartFileName.stem(label.text, confirmedSubjects: subjects,
                                  originalStem: URL(fileURLWithPath: path).deletingPathExtension().lastPathComponent)
    }

    private static func usableLabels(_ values: [String]) -> [String] {
        let generic: Set<String> = ["untitled", "chapter", "moment", "marker", "event", "file", "photo", "image", "video", "document"]
        var unique: [String: String] = [:]
        for value in values {
            let text = value.split(whereSeparator: \.isWhitespace).joined(separator: " ")
            let words = text.split { !$0.isLetter && !$0.isNumber }
            guard !text.isEmpty, text.count <= 60, text.utf8.count <= 200, words.count <= 8,
                  words.contains(where: { $0.contains(where: \.isLetter) && !generic.contains($0.lowercased()) }) else { continue }
            if unique[text.lowercased()] == nil { unique[text.lowercased()] = text }
        }
        return unique.values.sorted()
    }
}
