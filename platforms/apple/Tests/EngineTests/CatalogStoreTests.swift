import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Evidence catalog")
struct CatalogStoreTests {
    @Test func chapterUndoSurvivesReopenAndKeepsChangedSourcesStale() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let databaseURL = directory.appendingPathComponent("catalog.sqlite")
        let database = try FileIDEngine.Database(at: databaseURL)
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,'/internal/source.mov',1,100,0,'video','mov')")
        }
        let chapter = CatalogChapter(id: "gift", fileID: 1, startSeconds: 1, endSeconds: 2, title: "Gift", summary: "A present", sourceRevision: "", modelVersion: "user", confidence: 1, userEdited: true, stale: false)
        let saved = await CatalogStore.handle(CatalogRequest(requestID: "save", action: "saveChapter", chapter: chapter), database: database)
        #expect(saved.status == "ok")
        let deleted = await CatalogStore.handle(CatalogRequest(requestID: "delete", action: "deleteChapter", fileID: 1, chapterID: "gift"), database: database)
        #expect(deleted.status == "ok")
        try await database.pool.write { db in
            try db.execute(sql: "UPDATE catalog_operations SET created_at=999999999999 WHERE rowid=(SELECT MIN(rowid) FROM catalog_operations)")
            try db.execute(sql: "UPDATE files SET size_bytes=200 WHERE id=1")
        }
        try database.pool.close()
        let reopened = try FileIDEngine.Database(at: databaseURL)
        let restored = await CatalogStore.handle(CatalogRequest(requestID: "undo", action: "undoChapterEdit", fileID: 1), database: reopened)
        #expect(restored.status == "ok")
        #expect(restored.chapters.first?.summary == "A present")
        #expect(restored.chapters.first?.stale == true)
        let undoneCreation = await CatalogStore.handle(CatalogRequest(requestID: "undo-again", action: "undoChapterEdit", fileID: 1), database: reopened)
        #expect(undoneCreation.status == "ok")
        #expect(undoneCreation.chapters.isEmpty)
        let noHistory = await CatalogStore.handle(CatalogRequest(requestID: "empty", action: "undoChapterEdit", fileID: 1), database: reopened)
        #expect(noHistory.status == "error")
    }

    @Test func storesManualChaptersAndFindsTimestampEvidence() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension,vlm_proposed_name) VALUES(1,'/Volumes/Adlon/Family Birthday.mov',1,100,10,0,'video','mov','Old Model Proposal')")
        }
        let chapter = CatalogChapter(id: "gift", fileID: 1, startSeconds: 12.5, endSeconds: 20, title: "Gift Opening", summary: "Grandma opens a present", sourceRevision: "untrusted", modelVersion: "untrusted", confidence: 0, userEdited: false, stale: true)
        let saved = await CatalogStore.handle(CatalogRequest(requestID: "save", action: "saveChapter", chapter: chapter), database: database)
        #expect(saved.status == "ok")
        #expect(saved.chapters.first?.modelVersion == "user")
        #expect(saved.chapters.first?.userEdited == true)
        #expect(saved.chapters.first?.sourceRevision != "untrusted")
        let hits = await CatalogStore.handle(CatalogRequest(requestID: "search", action: "search", query: "Grandma present"), database: database)
        #expect(hits.hits.count == 1)
        #expect(hits.hits.first?.startSeconds == 12.5)
        let injection = await CatalogStore.handle(CatalogRequest(requestID: "literal", action: "search", query: "\" OR 1=1"), database: database)
        #expect(injection.status == "ok")
        #expect(injection.hits.isEmpty)
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO catalog_events(id,title) VALUES('birthday','Birthday')")
            try db.execute(sql: "INSERT INTO catalog_take_scores(event_id,file_id,outcome_score,quality_score,confidence,explanation,source_revision,model_version,preferred) VALUES('birthday',1,0.9,0.5,0.8,'Fixture','old','fixture',1)")
            try db.execute(sql: "INSERT INTO catalog_passages(id,file_id,text,source_revision,model_version,confidence) VALUES('auto',1,'baseball hit','old','model',0.5)")
            try db.execute(sql: "UPDATE files SET size_bytes=200,modified_at=11 WHERE id=1")
        }
        let stale = await CatalogStore.handle(CatalogRequest(requestID: "stale", action: "search", query: "baseball hit"), database: database)
        #expect(stale.hits.isEmpty)
        let manual = await CatalogStore.handle(CatalogRequest(requestID: "manual", action: "search", query: "Gift Opening"), database: database)
        #expect(manual.hits.isEmpty)
        let stored = try await database.pool.read { db in try CatalogStore.chapters(db, fileID: 1) }
        #expect(stored.first?.title == "Gift Opening")
        #expect(stored.first?.userEdited == true)
        #expect(stored.first?.stale == true)
        let take = try await database.pool.read { db in
            let row = try Row.fetchOne(db, sql: "SELECT preferred,stale FROM catalog_take_scores WHERE file_id=1")!
            return (row["preferred"] as Bool, row["stale"] as Bool)
        }
        #expect(take.0 && take.1)
        let restored = await CatalogStore.handle(CatalogRequest(requestID: "reconfirm", action: "saveChapter", chapter: chapter), database: database)
        #expect(restored.status == "ok")
        let accepted = try await database.pool.read { db in try String.fetchOne(db, sql: "SELECT path_text FROM files WHERE id=1") }
        #expect(accepted == "/Volumes/Adlon/Family Birthday.mov")
    }

    @Test func rejectsCrossFileChapterReplacementAndInvalidTiming() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            for id in 1...2 { try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?,?,?,10,0,'video','mov')", arguments: [id,"/internal/\(id).mov",id]) }
        }
        var chapter = CatalogChapter(id: "one", fileID: 1, startSeconds: 2, endSeconds: 3, title: "One", summary: "", sourceRevision: "", modelVersion: "user", confidence: 1, userEdited: true, stale: false)
        let first = await CatalogStore.handle(CatalogRequest(requestID: "first", action: "saveChapter", chapter: chapter), database: database)
        #expect(first.status == "ok")
        chapter.fileID = 2
        let cross = await CatalogStore.handle(CatalogRequest(requestID: "cross", action: "saveChapter", chapter: chapter), database: database)
        #expect(cross.status == "error")
        chapter.id = "two"; chapter.startSeconds = -1
        let invalid = await CatalogStore.handle(CatalogRequest(requestID: "invalid", action: "saveChapter", chapter: chapter), database: database)
        #expect(invalid.status == "error")
    }
}
