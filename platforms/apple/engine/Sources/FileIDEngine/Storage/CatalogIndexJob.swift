import Foundation
import GRDB
import FileIDShared

enum CatalogIndexJob {
    static let kind = "catalogIndex"
    static var id: String { "catalog-index-" + CatalogVectorIndex.digest(Data(CLIPEmbeddingSpace.modelID.utf8)) }

    struct Stopped: LocalizedError {
        var errorDescription: String? { "Search index preparation is stopped. Resume its job in Tools to continue; keyword search remains available." }
    }

    struct AdmissionDeferred: LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    private struct Checkpoint: Codable {
        let model: String
        let revision: CatalogVectorIndex.Revision
        let processed: Int
        let total: Int
    }

    static func begin(_ pool: DatabasePool) throws {
        try Task.checkCancellation()
        try pool.write { db in
            let now = Date().timeIntervalSince1970
            let recipe = String(decoding: try JSONEncoder().encode(["model": CLIPEmbeddingSpace.modelID]), as: UTF8.self)
            try db.execute(sql: "INSERT OR IGNORE INTO catalog_jobs(id,kind,file_ids_json,recipe_json,state,priority,created_at,updated_at) VALUES(?,?,'[]',?,'queued',1,?,?)", arguments: [id, kind, recipe, now, now])
            let state = try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=? AND kind=?", arguments: [id,kind])
            guard state == "queued" || state == "completed" else { throw Stopped() }
            try db.execute(sql: "UPDATE catalog_jobs SET state='running',progress=0,error=NULL,updated_at=? WHERE id=?", arguments: [now,id])
        }
    }

    static func progress(_ pool: DatabasePool, revision: CatalogVectorIndex.Revision, processed: Int, total: Int) throws {
        try Task.checkCancellation()
        let checkpoint = String(decoding: try JSONEncoder().encode(Checkpoint(model: CLIPEmbeddingSpace.modelID, revision: revision, processed: processed, total: total)), as: UTF8.self)
        try pool.write { db in
            guard try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [id]) == "running" else { throw Stopped() }
            try db.execute(sql: "UPDATE catalog_jobs SET checkpoint_json=?,progress=?,updated_at=? WHERE id=? AND state='running'", arguments: [checkpoint, min(0.99, Double(processed)/Double(max(1,total))), Date().timeIntervalSince1970,id])
        }
    }

    static func finish(_ pool: DatabasePool, error: Error? = nil) throws {
        try pool.write { db in
            let state = error is CancellationError || error is AdmissionDeferred ? "paused" : error == nil ? "completed" : "failed"
            try db.execute(sql: "UPDATE catalog_jobs SET state=?,progress=CASE WHEN ?='completed' THEN 1 ELSE progress END,error=?,updated_at=? WHERE id=? AND state='running'", arguments: [state,state,error?.localizedDescription,Date().timeIntervalSince1970,id])
        }
    }

    static func finishCached(_ pool: DatabasePool) throws {
        guard try pool.read({ db in try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [id]) }) == "queued" else { return }
        try pool.write { db in
            try db.execute(sql: "UPDATE catalog_jobs SET state='completed',progress=1,error=NULL,updated_at=? WHERE id=? AND state='queued'", arguments: [Date().timeIntervalSince1970,id])
        }
    }

    static func recover(_ pool: DatabasePool) async throws {
        try await pool.write { db in
            try db.execute(sql: "UPDATE catalog_jobs SET state='paused',error='Interrupted. Resume search index preparation to rebuild from the last verified snapshot.',updated_at=? WHERE kind=? AND state IN ('running','queued')", arguments: [Date().timeIntervalSince1970,kind])
        }
    }
}
