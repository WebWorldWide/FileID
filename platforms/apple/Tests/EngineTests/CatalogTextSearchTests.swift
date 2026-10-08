import Foundation
import GRDB
import Testing
import FileIDShared
@testable import FileIDEngine

@Suite("Catalog text retrieval")
struct CatalogTextSearchTests {
    @Test func chatAndCatalogFindExtractedDocumentAndOCRText() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(1,'/internal/notes.txt',1,100,0,'doc','txt')")
            try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(2,'/internal/photo.jpg',2,100,0,'image','jpg')")
            try db.execute(sql: "INSERT INTO doc_text(file_id,text) VALUES(1,'Birthday gift opening with Alex')")
            try db.execute(sql: "INSERT INTO ocr_text(file_id,text) VALUES(2,'Invoice total 42 dollars')")
        }

        let documentHits = try await database.pool.read {
            try CatalogStore.search($0, query: "birthday gift")
        }
        #expect(documentHits.contains { $0.fileID == 1 && $0.kind == "documentText" && $0.text.contains("Birthday gift") })
        #expect(try await database.pool.read {
            try CatalogStore.search($0, query: "birthday gift", kinds: ["video"])
        }.isEmpty)

        let imageHits = try await database.pool.read {
            try CatalogStore.search($0, query: "invoice total")
        }
        #expect(imageHits.contains { $0.fileID == 2 && $0.kind == "ocrText" && $0.text.contains("Invoice total") })
    }
}
