import Foundation
import Testing
import GRDB
import FileIDShared
@testable import FileIDEngine

@Suite("Reviewed naming evidence", .serialized)
struct CatalogNamingContextTests {
    private func fixture() async throws -> (FileIDEngine.Database, URL, URL) {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-name-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let source = root.appendingPathComponent("IMG_001.jpg")
        try Data([1]).write(to: source)
        try FileManager.default.setAttributes([.modificationDate: Date(timeIntervalSince1970: 1)], ofItemAtPath: source.path)
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("test.sqlite"))
        try await db.pool.write { d in
            try d.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,kind,extension,scanned_at) VALUES(1,?,1,1,1,'image','jpg',1)", arguments: [source.path])
            try d.execute(sql: "INSERT INTO catalog_revisions VALUES(1,'1:4607182418800017408','file-stat-v1',1)")
        }
        return (db, root, source)
    }
    private func chapter(_ d: GRDB.Database, title: String, id: String = "chapter", edited: Bool = true, stale: Bool = false) throws {
        try d.execute(sql: "INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,source_revision,model_version,confidence,user_edited,stale) VALUES(?,1,0,1,?,'1:4607182418800017408','user',1,?,?)", arguments: [id,title,edited,stale])
    }
    @Test func reviewedTitleProducesShortSpecificName() async throws {
        let (db,root,_) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        try await db.pool.write { try chapter($0, title: "Maya hits a home run") }
        let context = try await db.pool.read { try CatalogNamingContext.load($0, fileID: 1, faceModel: nil) }
        let name = try #require(context.proposedName)
        #expect(name.lowercased().contains("maya"))
        #expect(name.lowercased().contains("home"))
        #expect(name.count <= 80)
        #expect(!name.contains(".jpg"))
        try await DeepAnalyzeRunner.persist(database: db, fileID: 1, description: nil, proposedName: name, modelKey: "reviewed", updatesDescription: false, completesFullPass: false, namingContext: context)
        #expect(try await db.pool.read { try String.fetchOne($0, sql: "SELECT vlm_proposed_name FROM files WHERE id=1") } == name)
    }
    @Test(arguments: ["draft", "stale", "generic", "conflict", "goal"])
    func ambiguousOrUnreviewedEvidenceAbstains(fault: String) async throws {
        let (db,root,_) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        try await db.pool.write { d in
            switch fault {
            case "draft": try chapter(d, title: "Maya hits a home run", edited: false)
            case "stale": try chapter(d, title: "Maya hits a home run", stale: true)
            case "generic": try chapter(d, title: "Video")
            case "conflict": try chapter(d, title: "Home run"); try chapter(d, title: "Birthday gift", id: "other")
            default:
                try d.execute(sql: "INSERT INTO catalog_events VALUES('event','Baseball','gets a hit',1)")
                try d.execute(sql: "INSERT INTO catalog_event_files VALUES('event',1)")
            }
        }
        #expect(try await db.pool.read { try CatalogNamingContext.load($0, fileID: 1, faceModel: nil).proposedName } == nil)
    }
    @Test(arguments: ["source", "label", "person"])
    func lateEvidenceChangesRejectPersistence(fault: String) async throws {
        let (db,root,source) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        let vector = [Float(1)] + [Float](repeating: 0,count:127)
        let blob = ArcFaceService.embeddingToBlob(vector)
        try await db.pool.write { d in
            try chapter(d, title: "Home run")
            try d.execute(sql: "INSERT INTO persons(id,first_name,created_at) VALUES(1,'Maya',1)")
            try d.execute(sql: "INSERT INTO face_prints(file_id,person_id,print_data,bbox,arcface_embedding,embedding_model,processing_version,source_revision) VALUES(1,1,X'00','[0.1,0.1,0.3,0.3]',?,'fixture',?,'1:4607182418800017408')", arguments: [blob,FaceAnalysisCache.processingVersion])
        }
        let context = try await db.pool.read { try CatalogNamingContext.load($0, fileID: 1, faceModel: "fixture") }
        #expect(context.subjects == ["Maya"])
        if fault == "source" { try Data([1,2]).write(to: source) }
        else { try await db.pool.write { d in
            if fault == "label" { try d.execute(sql: "UPDATE catalog_chapters SET title='Birthday gift'") }
            else { try d.execute(sql: "UPDATE persons SET first_name='Alex'") }
        } }
        await #expect(throws: (any Error).self) {
            try await DeepAnalyzeRunner.persist(database: db, fileID: 1, description: "stale result", proposedName: context.proposedName, modelKey: "fixture", namingContext: context)
        }
        #expect(try await db.pool.read { try String.fetchOne($0, sql: "SELECT vlm_proposed_name FROM files WHERE id=1") } == nil)
    }
    @Test(arguments: ["excluded", "unknown", "model", "revision", "failed", "generic"])
    func invalidIdentityHintsAreOmitted(fault: String) async throws {
        let (db,root,_) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        let blob = ArcFaceService.embeddingToBlob([Float(1)] + [Float](repeating:0,count:127))
        try await db.pool.write { d in
            try d.execute(sql: "INSERT INTO persons(id,first_name,created_at) VALUES(1,'Maya',1)")
            try d.execute(sql: "INSERT INTO face_prints(file_id,person_id,print_data,bbox,arcface_embedding,embedding_model,processing_version,source_revision) VALUES(1,1,X'00','[0.1,0.1,0.3,0.3]',?,'fixture',?,'1:4607182418800017408')",arguments:[blob,FaceAnalysisCache.processingVersion])
            switch fault {
            case "excluded": try d.execute(sql: "UPDATE face_prints SET excluded=1")
            case "unknown": try d.execute(sql: "UPDATE persons SET is_unknown=1")
            case "model": try d.execute(sql: "UPDATE face_prints SET embedding_model='other'")
            case "revision": try d.execute(sql: "UPDATE face_prints SET source_revision='stale'")
            case "failed": try d.execute(sql: "UPDATE files SET failed=1")
            default: try d.execute(sql: "UPDATE persons SET first_name='Person 12'")
            }
        }
        #expect(try await db.pool.read { try CatalogNamingContext.load($0, fileID: 1, faceModel: "fixture").subjects }.isEmpty)
    }
}
