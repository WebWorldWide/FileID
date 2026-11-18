import Foundation
import GRDB
import FileIDShared
import Testing
@testable import FileIDEngine

@Suite("Chat people and peak questions")
struct ChatPeopleAndPeakTests {
    private func catalog(_ setup: @escaping @Sendable (GRDB.Database) throws -> Void) async throws -> (FileIDEngine.Database, URL) {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        let database = try FileIDEngine.Database(at: directory.appendingPathComponent("catalog.sqlite"))
        try await database.pool.write { db in try setup(db) }
        return (database, directory)
    }

    private func ask(_ text: String, _ database: FileIDEngine.Database) async throws -> ChatService.Prepared {
        try await ChatService.prepare(ChatRequest(requestID: "r", conversationID: text, action: "send", text: text, useModel: false), text: text, database: database)
    }

    @Test func thisLibraryAndOurCollectionAreNotSubjects() async throws {
        let (database, directory) = try await catalog { db in
            for (id, kind, ext) in [(1, "video", "mp4"), (2, "video", "mov"), (3, "image", "jpg"), (4, "image", "jpg"), (5, "image", "jpg")] {
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES(?,?,?,10,0,?,?)", arguments: [id, "/p/\(id).\(ext)", id, kind, ext])
            }
        }
        defer { try? FileManager.default.removeItem(at: directory) }
        #expect(try await ask("How many videos are in this library?", database).message == "Found 2 videos, using 20 bytes.")
        #expect(try await ask("count the videos in our library", database).message == "Found 2 videos, using 20 bytes.")
        #expect(try await ask("how many photos are in our collection", database).message == "Found 3 photos, using 30 bytes.")
        #expect(try await ask("how many photos of this dog", database).message == "No photos matching “this dog”.")
    }

    @Test func peopleQuestionsAnswerFromFaceGroupsNotThePeopleTag() async throws {
        let (database, directory) = try await catalog { db in
            try db.execute(sql: """
                INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES (1,'/p/1.jpg',1,1,0,'image','jpg'),(2,'/p/2.jpg',2,1,0,'image','jpg'),(3,'/p/3.jpg',3,1,0,'image','jpg'),(4,'/p/4.jpg',4,1,0,'image','jpg');
                INSERT INTO tags(file_id,tag,source,score) VALUES (3,'people','auto',0.9),(4,'people','auto',0.9);
                INSERT INTO persons(id,name,created_at,is_unknown) VALUES (1,NULL,0,0),(2,NULL,0,0),(3,'Hidden',0,1),(4,NULL,0,0);
                INSERT INTO face_prints(file_id,person_id,print_data,bbox,excluded) VALUES (1,1,x'00','',0),(2,1,x'00','',0),(2,2,x'00','',0),(4,3,x'00','',0),(3,4,x'00','',1);
                """)
        }
        defer { try? FileManager.default.removeItem(at: directory) }
        let noneNamed = "Your library has 2 groups of people in 2 files, but none are named yet. Open People to name them."
        for text in ["how many people are in my library?", "who is in my photos?", "show me the people in my photos"] {
            let reply = try await ask(text, database)
            #expect(reply.message == noneNamed, Comment(rawValue: text))
            #expect(Set(reply.hits.map(\.fileID)) == [1, 2], Comment(rawValue: text))
        }
        try await database.pool.write { db in try db.execute(sql: "UPDATE persons SET name='Kyle' WHERE id=1") }
        #expect(try await ask("How many people do I have?", database).message == "Your library has 2 groups of people in 2 files: 1 named (Kyle), 1 not named yet. Open People to name the rest.")
        #expect(try await ask("who's in my library?", database).message == "People in your library: Kyle (2 files). 1 more group is not named yet; open People to name them.")
        let named = try await ask("show me photos of Kyle", database)
        #expect(named.message.hasPrefix("Found 2 matches"), Comment(rawValue: named.message))
        #expect(Set(named.hits.map(\.fileID)) == [1, 2])
        try await database.pool.write { db in try db.execute(sql: "DELETE FROM face_prints") }
        #expect(try await ask("how many people are in my library?", database).message == "No people are grouped yet. Faces are grouped after a scan finds them; open People to see the groups.")
    }

    @Test func aNameThatMatchesNoPersonSaysPeopleAreNotNamedYet() async throws {
        let (database, directory) = try await catalog { db in
            try db.execute(sql: """
                INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension) VALUES (1,'/p/Kyle at the beach.jpg',1,1,0,'image','jpg'),(2,'/p/2.jpg',2,1,0,'image','jpg'),(3,'/p/Kyle Senior Pics/3.jpg',3,1,0,'image','jpg');
                INSERT INTO persons(id,name,created_at,is_unknown) VALUES (1,NULL,0,0);
                INSERT INTO face_prints(file_id,person_id,print_data,bbox,excluded) VALUES (2,1,x'00','',0);
                """)
        }
        defer { try? FileManager.default.removeItem(at: directory) }
        let note = "People aren't named yet — name them in People to search by person."
        #expect(try await ask("show me photos of Kyle", database).message == "Found 2 matches for “Kyle” (photos). \(note)")
        #expect(try await ask("show me photos of Zorro", database).message == "No matches for “Zorro” (photos). Try names or a few descriptive words; files that haven't been analyzed may still contain it. \(note)")
        #expect(try await ask("how many photos of Kyle?", database).message.hasSuffix(note))
        for text in ["show me photos of kyle", "photos from Kyle Senior Pics", "show me photos of Kyle Senior Pics"] {
            #expect(!(try await ask(text, database).message.contains(note)), Comment(rawValue: text))
        }
        try await database.pool.write { db in try db.execute(sql: "UPDATE persons SET name='Kyle' WHERE id=1") }
        #expect(!(try await ask("show me photos of Kyle", database).message.contains(note)))
        try await database.pool.write { db in try db.execute(sql: "UPDATE persons SET name='Maya' WHERE id=1") }
        #expect(!(try await ask("show me photos of Zorro", database).message.contains(note)))
        try await database.pool.write { db in try db.execute(sql: "DELETE FROM face_prints") }
        #expect(!(try await ask("show me photos of Kyle", database).message.contains(note)))
    }

    @Test func peakQuestionsAnswerByYearOrMonthOrSayTheyCannot() async throws {
        let (database, directory) = try await catalog { db in
            for (id, taken) in [(1, 1_718_452_800), (2, 1_749_988_800), (3, 1_752_580_800), (4, 1_781_524_800), (5, 1_781_524_900), (6, 1_781_525_000)] {
                try db.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,scanned_at,kind,extension,created_at) VALUES(?,?,?,10,0,'image','jpg',?)", arguments: [id, "/p/\(id).jpg", id, taken])
            }
        }
        defer { try? FileManager.default.removeItem(at: directory) }
        #expect(try await ask("Which year has the most photos?", database).message == "2026 has the most photos (3). Photos by year: 2026: 3, 2025: 2, 2024: 1.")
        #expect(try await ask("in which year did I take the fewest photos?", database).message == "2024 has the fewest photos (1). Photos by year: 2026: 3, 2025: 2, 2024: 1.")
        #expect(try await ask("which month has the most photos?", database).message.hasPrefix("June 2026 has the most photos (3). Photos by month: 2026-06: 3,"))
        #expect(try await ask("what was my busiest month?", database).message.hasPrefix("June 2026 has the most files (3)."))
        #expect(try await ask("which month has the fewest photos?", database).message.hasPrefix("July 2025, June 2025 and June 2024 are tied for the fewest photos (1 each)."))
        #expect(try await ask("which day has the most photos?", database).message == "I can't rank photos by day yet. I can count them by year or month, for example “which month has the most photos?”")
        #expect(try await ask("which year uses the most space?", database).message.hasPrefix("2026 uses the most space (30 bytes)."))
    }
}
