import Testing
import Foundation
import GRDB
import AsyncAlgorithms
@testable import FileIDEngine
private typealias Database = FileIDEngine.Database

@Suite("DBWriter keeps a stored modification time that names the same instant")
struct DBWriterModifiedTimeTests {
    private func file(_ url: URL, modified: Double) -> TaggedFile {
        TaggedFile(url: url, kind: "doc", extension: "txt", sizeBytes: 4, createdAt: nil,
                   modifiedAt: Date(timeIntervalSince1970: modified), modifiedUnix: modified, visionTags: [], ocrText: nil)
    }

    private func drain(_ db: Database, _ file: TaggedFile) async {
        let writer = DBWriter(db: db, sink: IPCSink(), coordinator: ScanCoordinator(), sessionID: UUID().uuidString)
        let channel = AsyncChannel<TaggedFile>()
        let producer = Task { await channel.send(file); channel.finish() }
        await writer.drain(channel)
        await producer.value
    }

    private func read(_ db: Database, _ fileID: Int64) async throws -> (modified: Double, stale: Int) {
        try await db.pool.read { db in
            let row = try #require(try Row.fetchOne(db, sql: "SELECT f.modified_at, c.stale FROM files f JOIN catalog_chapters c ON c.file_id=f.id WHERE f.id=?", arguments: [fileID]))
            return (row["modified_at"], row["stale"])
        }
    }

    @Test("a time a bit apart keeps the stored number and leaves the reviewed chapter current; a real change updates and stales it")
    func keepsStoredNumber() async throws {
        let folder = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDModifiedTime-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: folder) }
        let db = try Database(at: folder.appendingPathComponent("test.sqlite"))
        let url = folder.appendingPathComponent("a.txt")
        let stored = Double(bitPattern: 0x41d9_b450_2f75_52cf)
        await drain(db, file(url, modified: stored))
        let id = try await db.pool.read { try Int64.fetchOne($0, sql: "SELECT id FROM files WHERE path_text=?", arguments: [url.path]) }
        let fileID = try #require(id)
        try await db.pool.write {
            try $0.execute(sql: "INSERT INTO catalog_chapters(id,file_id,start_seconds,end_seconds,title,summary,source_revision,model_version,confidence,user_edited,stale) VALUES('c',?,0,1,'t','','r','user',1,1,0)", arguments: [fileID])
        }

        // The same instant rounded one bit differently by the other engine.
        await drain(db, file(url, modified: stored.nextUp))
        var row = try await read(db, fileID)
        #expect(row.modified.bitPattern == stored.bitPattern)
        #expect(row.stale == 0)

        // A real change of a minute is stored and stales the chapter.
        await drain(db, file(url, modified: stored + 60))
        row = try await read(db, fileID)
        #expect(row.modified == stored + 60)
        #expect(row.stale == 1)
    }

    @Test("discovery reads the time from one stat as seconds plus nanoseconds")
    func discoveryTime() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("fileid-discovery-time-\(UUID().uuidString).txt")
        try Data([1]).write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }
        var times = [timespec(tv_sec: 1_724_989_629, tv_nsec: 833_179_165), timespec(tv_sec: 1_724_989_629, tv_nsec: 833_179_165)]
        #expect(utimensat(AT_FDCWD, url.path, &times, 0) == 0)
        let identity = Discovery.identity(of: url)
        #expect(identity.modified?.bitPattern == 0x41d9_b450_2f75_52cf)
        // Foundation's own reading of the same file differs in the last bit.
        let foundation = try url.resourceValues(forKeys: [.contentModificationDateKey]).contentModificationDate?.timeIntervalSince1970
        #expect(foundation != identity.modified)
        #expect(identity.size == 1)
        #expect(Discovery.identity(of: url.appendingPathExtension("missing")).modified == nil)
        #expect(Discovery.identity(of: url.appendingPathExtension("missing")).size == nil)
    }
}
