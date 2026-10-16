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

        try await database.pool.write { db in
            try db.execute(sql: "UPDATE files SET failed=1 WHERE id=1")
        }
        #expect(try await database.pool.read {
            try CatalogStore.search($0, query: "birthday gift")
        }.isEmpty)
    }

    @Test func chatFindsScannedTagsWithoutDescriptions() async throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: directory) }
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            for id in 1...3 {
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?,?,?,100,0,'image','jpg')",
                               arguments: [id, "/internal/IMG_\(id).jpg", id])
            }
            try db.execute(sql: """
                INSERT INTO tags(file_id,tag,source,score) VALUES
                  (1,'grassland','auto',0.9),(1,'dog','auto',0.8),
                  (2,'blue_sky','auto',0.9),(2,'grass','auto',0.7),
                  (3,'grassy_knoll','rejected',0.9)
                """)
        }

        let grass = try await database.pool.read { try CatalogStore.search($0, query: "photos with grass") }
        #expect(Set(grass.map(\.fileID)) == [1, 2])
        #expect(grass.first { $0.fileID == 1 }?.text.contains("grassland") == true)

        let both = try await database.pool.read { try CatalogStore.search($0, query: "dog on grass") }
        #expect(both.map(\.fileID) == [1], "files matching every word outrank partial matches")

        #expect(try await database.pool.read { try CatalogStore.search($0, query: "sky") }.map(\.fileID) == [2])
    }

    @Test func evidencePromptIsNumberedAndBounded() {
        let hits = (1...10).map { CatalogHit(fileID: Int64($0), path: "/a/IMG_\($0).jpg", kind: "image", text: "Tags: grass\nsky") }
        let prompt = ChatService.evidencePrompt(request: "grass", hits: hits, limit: 3)
        #expect(prompt.contains("Matches found: 10 (showing 3)"))
        #expect(prompt.contains("[1] IMG_1.jpg (image): Tags: grass sky"))
        #expect(prompt.contains("[3] IMG_3.jpg"))
        #expect(!prompt.contains("[4]"))
        #expect(prompt.hasSuffix("</evidence>"))
    }
}
