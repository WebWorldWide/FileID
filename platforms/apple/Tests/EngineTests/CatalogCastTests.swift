import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Personal media cast", .serialized)
struct CatalogCastTests {
    private func fixture() async throws -> (FileIDEngine.Database, URL, URL) {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-cast-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        let source = root.appendingPathComponent("clip.mov")
        try Data([1]).write(to: source)
        try FileManager.default.setAttributes([.modificationDate: Date(timeIntervalSince1970:1)], ofItemAtPath: source.path)
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("test.sqlite"))
        let blob = FloatBlob.encode([Float(1)] + [Float](repeating:0,count:127))
        try await db.pool.write { d in
            try d.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,modified_at,scanned_at,kind,extension) VALUES(1,?,1,1,1,1,'video','mov')", arguments:[source.path])
            try d.execute(sql: "INSERT INTO catalog_revisions VALUES(1,'1:4607182418800017408','file-stat-v1',1)")
            try d.execute(sql: "INSERT INTO persons(id,first_name,created_at) VALUES(1,'Maya',1)")
            try d.execute(sql: "INSERT INTO face_prints(id,file_id,person_id,print_data,bbox,arcface_embedding,embedding_model,processing_version,source_revision) VALUES(1,1,1,X'00','[0.1,0.1,0.3,0.3]',?,'fixture',?,'1:4607182418800017408')",arguments:[blob,FaceAnalysisCache.processingVersion])
        }
        return (db,root,source)
    }
    @Test func detectedAndReviewedAppearancesKeepTheirEvidence() async throws {
        let (db,root,_) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        try await db.pool.write { d in
            try d.execute(sql: "INSERT INTO catalog_observations(id,file_id,person_id,start_seconds,end_seconds,source_revision,model_version,confidence,user_edited) VALUES('reviewed',1,1,4,5,'1:4607182418800017408','user',1,1)")
        }
        let hits = try await db.pool.read { try CatalogCast.hits($0, fileID:1,faceModel:"fixture") }
        #expect(hits.count == 2)
        #expect(hits.contains { $0.text == "Maya · Detected" && $0.startSeconds == nil })
        #expect(hits.contains { $0.text == "Maya · Reviewed" && $0.startSeconds == 4 })
        #expect(hits.allSatisfy { $0.kind == "person" && $0.evidenceID?.hasPrefix("person:") == true })
    }
    @Test(arguments: ["excluded","unknown","revision","model","source","generic"])
    func staleIgnoredOrAmbiguousIdentitiesAreAbsent(fault:String) async throws {
        let (db,root,source) = try await fixture(); defer { try? FileManager.default.removeItem(at: root) }
        if fault == "source" { try Data([1,2]).write(to:source) }
        else { try await db.pool.write { d in
            switch fault {
            case "excluded": try d.execute(sql:"UPDATE face_prints SET excluded=1")
            case "unknown": try d.execute(sql:"UPDATE persons SET is_unknown=1")
            case "revision": try d.execute(sql:"UPDATE face_prints SET source_revision='stale'")
            case "model": try d.execute(sql:"UPDATE face_prints SET embedding_model='other'")
            default: try d.execute(sql:"UPDATE persons SET first_name='Person 12'")
            }
        } }
        #expect(try await db.pool.read { try CatalogCast.hits($0,fileID:1,faceModel:"fixture") }.isEmpty)
    }
}
