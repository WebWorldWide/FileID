import Foundation
import GRDB
import FileIDShared

public enum CatalogStore {
    public static func handle(_ request: CatalogRequest, database: Database) async -> CatalogResponse {
        do {
            guard !request.requestID.isEmpty, request.requestID.count <= 200 else { throw InvalidRequest() }
            switch request.action {
            case "search":
                return try await CatalogSearch.handle(request, database: database)
            case "detail":
                guard let fileID = request.fileID else { throw InvalidRequest() }
                let chapters = try await database.pool.read { db in try Self.chapters(db, fileID: fileID) }
                return CatalogResponse(requestID: request.requestID, status: "ok", chapters: chapters)
            case "saveChapter":
                guard var chapter = request.chapter, !chapter.id.isEmpty, chapter.id.count <= 200,
                    chapter.startSeconds.isFinite, chapter.endSeconds.isFinite,
                    chapter.startSeconds >= 0, chapter.endSeconds >= chapter.startSeconds,
                    !chapter.title.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty,
                    chapter.title.count <= 200, chapter.summary.count <= 4000 else { throw InvalidRequest() }
                chapter.userEdited = true
                chapter.stale = false
                chapter.modelVersion = "user"
                chapter.confidence = 1
                let edit = chapter
                try await database.pool.write { db in
                    var saved = edit
                    saved.sourceRevision = try revision(db, fileID: saved.fileID)
                    if let owner = try Int64.fetchOne(db, sql: "SELECT file_id FROM catalog_chapters WHERE id=?", arguments: [saved.id]), owner != saved.fileID { throw InvalidRequest() }
                    let before = try Self.chapters(db, fileID: saved.fileID).first { $0.id == saved.id }
                    try upsertChapter(db, chapter: saved)
                    try journalChapter(db, fileID: saved.fileID, chapterID: saved.id, before: before, after: saved)
                }
                let saved = try await database.pool.read { db in try Self.chapters(db, fileID: edit.fileID) }
                return CatalogResponse(requestID: request.requestID, status: "ok", chapters: saved)
            case "deleteChapter":
                guard let chapterID = request.chapterID, let fileID = request.fileID else { throw InvalidRequest() }
                try await database.pool.write { db in
                    guard let before = try Self.chapters(db, fileID: fileID).first(where: { $0.id == chapterID }) else { throw InvalidRequest() }
                    try db.execute(sql: "DELETE FROM catalog_chapters WHERE id=? AND file_id=?", arguments: [chapterID, fileID])
                    try journalChapter(db, fileID: fileID, chapterID: chapterID, before: before, after: nil)
                }
                return CatalogResponse(requestID: request.requestID, status: "ok")
            case "undoChapterEdit":
                guard let fileID = request.fileID else { throw InvalidRequest() }
                try await database.pool.write { db in
                    guard let row = try Row.fetchOne(db, sql: "SELECT o.id,o.inverse_json,c.after_json FROM catalog_operations o JOIN catalog_corrections c ON c.id=o.id WHERE c.file_id=? AND c.kind='chapter' AND o.state='completed' ORDER BY o.rowid DESC LIMIT 1", arguments: [fileID]) else { throw InvalidRequest() }
                    let inverse: String = row["inverse_json"]
                    let afterJSON: String = row["after_json"]
                    let after = try JSONDecoder().decode(CatalogChapter?.self, from: Data(afterJSON.utf8))
                    if var before = try JSONDecoder().decode(CatalogChapter?.self, from: Data(inverse.utf8)) {
                        let currentRevision = try revision(db, fileID: fileID)
                        before.stale = before.stale || before.sourceRevision != currentRevision
                        try upsertChapter(db, chapter: before)
                    } else if let after {
                        try db.execute(sql: "DELETE FROM catalog_chapters WHERE id=? AND file_id=?", arguments: [after.id, fileID])
                    }
                    let id: String = row["id"]
                    try db.execute(sql: "UPDATE catalog_operations SET state='undone' WHERE id=?", arguments: [id])
                }
                return CatalogResponse(requestID: request.requestID, status: "ok", chapters: try await database.pool.read { db in try Self.chapters(db, fileID: fileID) })
            case "jobs":
                return CatalogResponse(requestID: request.requestID, status: "ok", jobs: try await jobs(database))
            case "pauseJob", "resumeJob", "cancelJob":
                guard let jobID = request.jobID else { throw InvalidRequest() }
                let target = request.action == "pauseJob" ? "paused" : request.action == "resumeJob" ? "queued" : "cancelled"
                try await database.pool.write { db in
                    let current = try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [jobID])
                    guard let current, ["queued", "running", "paused"].contains(current) else { throw InvalidRequest() }
                    if target == "queued", current != "paused" { throw InvalidRequest() }
                    try db.execute(sql: "UPDATE catalog_jobs SET state=?,updated_at=? WHERE id=?", arguments: [target, Date().timeIntervalSince1970, jobID])
                }
                return CatalogResponse(requestID: request.requestID, status: "ok", jobs: try await jobs(database))
            default:
                throw InvalidRequest()
            }
        } catch {
            return CatalogResponse(requestID: request.requestID, status: "error", message: error.localizedDescription)
        }
    }

    static func search(_ db: GRDB.Database, query: String, kinds: [String] = []) throws -> [CatalogHit] {
        guard kinds.allSatisfy({ ["image", "video", "pdf", "doc", "audio", "other", "model"].contains($0) }) else { throw InvalidRequest() }
        let filter = kinds.isEmpty ? nil : String(decoding: try JSONEncoder().encode(kinds), as: UTF8.self)
        let meaningful = query.split(whereSeparator: \.isWhitespace).filter { $0.contains(where: { $0.isLetter || $0.isNumber }) }.joined(separator: " ")
        guard !meaningful.isEmpty else {
            guard let filter else { return [] }
            return try Row.fetchAll(db, sql: "SELECT id,path_text,kind,COALESCE(vlm_description,'') AS description FROM files WHERE kind IN (SELECT value FROM json_each(?)) ORDER BY id DESC LIMIT 100", arguments: [filter]).map { row in
                CatalogHit(fileID: row["id"], path: row["path_text"], kind: row["kind"], text: row["description"])
            }
        }
        let match = FTSQuery.quoted(meaningful)
        var results: [CatalogHit] = []
        let files = try Row.fetchAll(db, sql: """
            SELECT f.id,f.path_text,f.kind,COALESCE(f.vlm_description,'') AS description
            FROM catalog_file_fts JOIN files f ON f.id=catalog_file_fts.rowid
            WHERE f.failed=0 AND catalog_file_fts MATCH ? AND (? IS NULL OR f.kind IN (SELECT value FROM json_each(?))) ORDER BY bm25(catalog_file_fts) LIMIT 100
            """, arguments: [match, filter, filter])
        for row in files {
            results.append(CatalogHit(fileID: row["id"], path: row["path_text"], kind: row["kind"], text: row["description"]))
        }
        let evidence = try Row.fetchAll(db, sql: """
            SELECT e.evidence_id,CASE WHEN p.start_seconds IS NOT NULL AND p.confidence=0 THEN 'sampledFrame' ELSE e.kind END AS kind,e.text,f.id,f.path_text,c.start_seconds AS chapter_time,p.start_seconds AS passage_time,p.page
            FROM catalog_evidence_fts e JOIN files f ON f.id=CAST(e.file_id AS INTEGER)
            LEFT JOIN catalog_chapters c ON c.id=e.evidence_id AND e.kind='chapter'
            LEFT JOIN catalog_passages p ON p.id=e.evidence_id AND e.kind='passage'
            WHERE f.failed=0 AND catalog_evidence_fts MATCH ? AND (? IS NULL OR f.kind IN (SELECT value FROM json_each(?))) AND (c.stale=0 OR p.stale=0)
            ORDER BY bm25(catalog_evidence_fts) LIMIT 100
            """, arguments: [match, filter, filter])
        for row in evidence {
            let chapterTime: Double? = row["chapter_time"]
            results.append(CatalogHit(fileID: row["id"], path: row["path_text"], kind: row["kind"], text: row["text"], evidenceID: row["evidence_id"], startSeconds: chapterTime ?? row["passage_time"], page: row["page"]))
        }
        return results
    }

    static func chapters(_ db: GRDB.Database, fileID: Int64) throws -> [CatalogChapter] {
        try Row.fetchAll(db, sql: "SELECT * FROM catalog_chapters WHERE file_id=? ORDER BY start_seconds,id", arguments: [fileID]).map { row in
            CatalogChapter(id: row["id"], fileID: row["file_id"], startSeconds: row["start_seconds"], endSeconds: row["end_seconds"], title: row["title"], summary: row["summary"], sourceRevision: row["source_revision"], modelVersion: row["model_version"], confidence: row["confidence"], userEdited: row["user_edited"], stale: row["stale"])
        }
    }

    static func revision(_ db: GRDB.Database, fileID: Int64) throws -> String {
        guard let row = try Row.fetchOne(db, sql: "SELECT size_bytes,modified_at FROM files WHERE id=?", arguments: [fileID]) else { throw InvalidRequest() }
        let size: Int64 = row["size_bytes"]
        let modified: Double? = row["modified_at"]
        return "\(size):\(modified.map { String($0.bitPattern) } ?? "unknown")"
    }

    static func upsertChapter(_ db: GRDB.Database, chapter c: CatalogChapter) throws {
        try db.execute(sql: """
            INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited,stale)
            VALUES(?,?,?,?,?,?,?,?,?,?,?) ON CONFLICT(id) DO UPDATE SET start_seconds=excluded.start_seconds,end_seconds=excluded.end_seconds,title=excluded.title,summary=excluded.summary,source_revision=excluded.source_revision,model_version=excluded.model_version,confidence=excluded.confidence,user_edited=excluded.user_edited,stale=excluded.stale
            """, arguments: [c.id,c.fileID,c.startSeconds,c.endSeconds,c.title,c.summary,c.sourceRevision,c.modelVersion,c.confidence,c.userEdited,c.stale])
    }

    static func journalChapter(_ db: GRDB.Database, fileID: Int64, chapterID: String, before: CatalogChapter?, after: CatalogChapter?) throws {
        let encoder = JSONEncoder()
        let beforeJSON = String(decoding: try encoder.encode(before), as: UTF8.self)
        let afterJSON = String(decoding: try encoder.encode(after), as: UTF8.self)
        let plan = String(decoding: try encoder.encode(["kind": "chapter", "chapterID": chapterID]), as: UTF8.self)
        let id = UUID().uuidString
        let now = Date().timeIntervalSince1970
        try db.execute(sql: "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?,?,'chapter',?,?,?)", arguments: [id,fileID,beforeJSON,afterJSON,now])
        try db.execute(sql: "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?,?,?,'completed',?)", arguments: [id,plan,beforeJSON,now])
    }

    public static func jobs(_ database: Database) async throws -> [CatalogJob] {
        try await database.pool.read { db in
            try Row.fetchAll(db, sql: "SELECT * FROM catalog_jobs ORDER BY created_at DESC LIMIT 200").map { row in
                let json: String = row["file_ids_json"]
                let ids = try JSONDecoder().decode([Int64].self, from: Data(json.utf8))
                return CatalogJob(id: row["id"], kind: row["kind"], fileIDs: ids, state: row["state"], progress: row["progress"], error: row["error"], createdAt: row["created_at"], updatedAt: row["updated_at"])
            }
        }
    }

    struct InvalidRequest: LocalizedError {
        var errorDescription: String? { "The catalog request is invalid or the selected item is no longer available." }
    }
}
