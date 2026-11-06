import Foundation
import GRDB
import Testing
@testable import FileIDEngine

@Suite("BGE document matrix", .serialized)
struct BGEDocumentMatrixTests {
    private func catalog(rows: Int, dimension: Int = 384) async throws -> (FileIDEngine.Database, URL, [Float]) {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDBGE-\(UUID().uuidString)")
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        var state: UInt64 = 0x9E37_79B9_7F4A_7C15
        func next() -> Float { state = state &* 6_364_136_223_846_793_005 &+ 1_442_695_040_888_963_407; return Float(Int64(bitPattern: state >> 11) % 2_000_001 - 1_000_000) / 1_000_000 }
        func unit() -> [Float] { let v = (0..<dimension).map { _ in next() }; let n = sqrt(v.reduce(0) { $0 + $1 * $1 }); return v.map { $0 / n } }
        let centre = unit()
        let vectors = (1...rows).map { _ in zip(centre, unit()).map { $0 * 0.75 + $1 * 0.25 } }
        try await db.pool.write { sql in
            for id in 1...rows {
                let vector = vectors[id - 1]
                let blob = vector.withUnsafeBufferPointer { Data(buffer: $0) }
                try sql.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,failed) VALUES(?,?,?,1,0,'doc','txt',?)", arguments: [id, "/d/\(id).txt", id, id % 97 == 0 ? 1 : 0])
                try sql.execute(sql: "INSERT INTO text_embeddings(file_id,embedding,model) VALUES(?,?,'bge_small_en_v1_5')", arguments: [id, blob])
            }
        }
        return (db, root, centre)
    }

    private func scan(_ db: GRDB.Database, _ wanted: [Float]) throws -> [Int64] {
        var scored: [(Float, Int64)] = []
        let rows = try Row.fetchCursor(db, sql: "SELECT e.file_id,e.embedding FROM text_embeddings e JOIN files f ON f.id=e.file_id WHERE f.failed=0")
        while let row = try rows.next() {
            let score = BGEDocumentMatrix.dot(row["embedding"], wanted)
            if score.isFinite, score >= ChatService.bgeFloor { scored.append((score, row["file_id"])) }
        }
        return scored.sorted { $0.0 != $1.0 ? $0.0 > $1.0 : $0.1 < $1.1 }.prefix(20).map(\.1)
    }

    @Test("cached scoring returns exactly what the row scan returns, and follows new rows")
    func matchesTheScan() async throws {
        BGEDocumentMatrix.shared.reset()
        defer { BGEDocumentMatrix.shared.reset() }
        let (db, root, centre) = try await catalog(rows: 20_000)
        defer { try? FileManager.default.removeItem(at: root) }
        let query = centre.enumerated().map { $0.offset % 3 == 0 ? $0.element * 1.1 : $0.element }
        let norm = sqrt(query.reduce(0) { $0 + $1 * $1 })
        let wanted = query.map { $0 / norm }
        try await db.pool.read { sql in
            let expected = try scan(sql, wanted)
            #expect(expected.count == 20)
            let clock = ContinuousClock()
            var cold: [Int64] = []
            let coldTime = try clock.measure { cold = try ChatService.bgeHits(sql, wanted).map(\.fileID) }
            var warm: [Int64] = []
            let warmTime = try clock.measure { warm = try ChatService.bgeHits(sql, wanted).map(\.fileID) }
            let scanTime = try clock.measure { _ = try scan(sql, wanted) }
            #expect(cold == expected)
            #expect(warm == expected)
            print("bge 20k: scan \(scanTime), first \(coldTime), cached \(warmTime)")
        }
        #expect(BGEDocumentMatrix.shared.isLoaded)
        try await db.pool.write { sql in
            let blob = wanted.withUnsafeBufferPointer { Data(buffer: $0) }
            try sql.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(99999,'/d/new.txt',99999,1,0,'doc','txt')")
            try sql.execute(sql: "INSERT INTO text_embeddings(file_id,embedding,model) VALUES(99999,?,'bge_small_en_v1_5')", arguments: [blob])
        }
        let first = try await db.pool.read { sql in try ChatService.bgeHits(sql, wanted).first?.fileID }
        #expect(first == 99999, "a new document is seen without a restart")
        #expect(!BGEDocumentMatrix.shared.releaseIfIdle(now: Date()))
        #expect(BGEDocumentMatrix.shared.releaseIfIdle(now: Date().addingTimeInterval(301)))
        #expect(!BGEDocumentMatrix.shared.isLoaded)
    }
}
