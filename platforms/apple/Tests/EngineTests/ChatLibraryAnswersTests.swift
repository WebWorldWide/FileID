import Foundation
import GRDB
import Testing
@testable import FileIDEngine

@Suite("Chat library answers")
struct ChatLibraryAnswersTests {
    private func catalog() async throws -> (FileIDEngine.Database, URL) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in
            let rows: [(Int, String, String, Int, Double)] = [
                (1, "/Volumes/Adlon/Camera/Game/a.jpg", "image", 3_000, 1_000_000),
                (2, "/Volumes/Adlon/Camera/Game/b.arw", "image", 9_000, 1_000_100),
                (3, "/Volumes/Adlon/Camera/Game/c.jpg", "image", 3_000, 1_000_200),
                (4, "/Volumes/Adlon/Cabinet/clip.mp4", "video", 50_000, 2_000_000),
                (5, "C:\\Users\\a\\Pictures\\d.png", "image", 1_000, 3_000_000),
            ]
            for (id, path, kind, size, created) in rows {
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,scanned_at,kind,extension) VALUES(?,?,?,?,?,0,?,'x')", arguments: [id, path, id, size, created, kind])
            }
        }
        return (database, directory)
    }

    private func answer(_ text: String, _ database: FileIDEngine.Database) async throws -> String {
        let question = ChatIntent.question(text)
        return try await database.pool.read { db in try ChatFacts.answer(db, question, ChatFacts.Filter()).0 }
    }

    @Test func overviewNamesKindsDatesAndBusiestFolders() async throws {
        let (database, directory) = try await catalog()
        defer { try? FileManager.default.removeItem(at: directory) }
        let message = try await answer("what is in my library", database)
        #expect(message.hasPrefix("Your library has 5 files using 66.0 KB: 4 photos, 1 video."))
        #expect(message.contains("Dates run from 1970-01-12 to 1970-02-"))
        #expect(message.contains("Busiest folders: Game (3 files, 15.0 KB), Cabinet (1 file, 50.0 KB), Pictures (1 file, 1.0 KB)."))
    }

    @Test func folderQuestionsRankByCountOrSizeAndFoldWindowsSeparators() async throws {
        let (database, directory) = try await catalog()
        defer { try? FileManager.default.removeItem(at: directory) }
        #expect(try await answer("which folder has the most files", database).hasPrefix("Folders with the most files: Game (3 files, 15.0 KB), Cabinet (1 file, 50.0 KB)"))
        #expect(try await answer("what are my biggest folders", database).hasPrefix("Biggest folders: Cabinet (1 file, 50.0 KB), Game (3 files, 15.0 KB)"))
        #expect(try await answer("which folder has the most files", database).contains("Pictures (1 file, 1.0 KB)"))
    }

    @Test func ordinaryQuestionsAreNotCapturedAsLibraryQuestions() {
        #expect(ChatIntent.question("how many files in my library").measure == "count")
        #expect(ChatIntent.question("photos of my library dog").measure == "list")
        #expect(ChatIntent.question("what is the biggest file").measure == "list")
        #expect(ChatIntent.question("show me the folder from Downloads").measure == "list")
    }
}
