import Foundation
import Testing
import GRDB
import FileIDShared
@testable import FileIDEngine

/// Questions that combine a subject with people, places, dates, formats, order
/// and counts, against a catalog large enough that a capped search would miss.
@Suite("Chat complex questions", .serialized)
struct ChatComplexQueryTests {
    static let day: Double = 86_400
    /// 2023-07-04 12:00 UTC and 2019-03-10 12:00 UTC.
    static let july2023: Double = 1_688_472_000
    static let march2019: Double = 1_552_219_200

    private func library() async throws -> (FileIDEngine.Database, URL) {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("FileIDChatBench-\(UUID().uuidString)")
        let db = try FileIDEngine.Database(at: root.appendingPathComponent("catalog.sqlite"))
        try await db.pool.write { sql in
            try sql.execute(sql: "INSERT INTO persons(id,name,file_count,created_at) VALUES (1,'Kyle',0,0),(2,'Maya',0,0)")
            var id: Int64 = 0
            func add(_ path: String, _ kind: String, _ ext: String, size: Int64, at time: Double, description: String? = nil,
                     tags: [String] = [], people: [Int64] = [], hash: UInt8? = nil) throws {
                id += 1
                try sql.execute(sql: "INSERT INTO files(id,path_text,path_hash,size_bytes,created_at,scanned_at,kind,extension,content_hash,vlm_description) VALUES (?,?,?,?,?,0,?,?,?,?)",
                                arguments: [id, path, id, size, time, kind, ext, hash.map { Data([$0, 0xAB]) } ?? Data([UInt8(id % 251), UInt8(id / 251 % 251), 0x01]), description])
                for tag in tags { try sql.execute(sql: "INSERT INTO tags(file_id,tag,source,score) VALUES (?,?,'auto',0.9)", arguments: [id, tag]) }
                for person in people {
                    try sql.execute(sql: "INSERT INTO face_prints(file_id,person_id,print_data,bbox) VALUES (?,?,?,'0,0,1,1')", arguments: [id, person, Data(count: 8)])
                }
            }
            // 1,500 dog photos spread over 2018–2022: a capped search would only see some.
            for n in 0..<1_500 {
                try add("/lib/Pets/dog_\(n).jpg", "image", "jpg", size: 3_000_000, at: Self.march2019 - Double(n % 400) * Self.day * 3,
                        description: "A dog playing in the yard", tags: ["dog"])
            }
            // Beach trip, July 2023: Kyle in 12 JPGs and 6 RAWs, Maya in 4 of the JPGs.
            for n in 0..<12 {
                try add("/lib/Trips/Beach 2023/IMG_\(n).jpg", "image", "jpg", size: 4_000_000 + Int64(n), at: Self.july2023 + Double(n) * 60,
                        description: "Kyle on the beach near the water", tags: ["beach", "sea"], people: n < 4 ? [1, 2] : [1])
            }
            for n in 0..<6 {
                try add("/lib/Trips/Beach 2023/DSC_\(n).ARW", "image", "arw", size: 25_000_000, at: Self.july2023 + Double(n) * 60 + 30,
                        description: "Kyle standing on the beach at sunset", tags: ["beach", "sunset"], people: [1])
            }
            // Beach videos: one in July 2023 (2.5 GB), one in 2019 (900 MB).
            try add("/lib/Trips/Beach 2023/surf.mov", "video", "mov", size: 2_500_000_000, at: Self.july2023 + 3_600,
                    description: "Kyle surfing a wave at the beach", tags: ["beach", "surfing"], people: [1])
            try add("/lib/Videos/beach_2019.mp4", "video", "mp4", size: 900_000_000, at: Self.march2019, description: "Waves on a beach", tags: ["beach"])
            // Downloads: a tax PDF from 2023, invoices, an installer, and a duplicate pair.
            try add("/Users/me/Downloads/2023 tax return.pdf", "pdf", "pdf", size: 800_000, at: Self.july2023 - 90 * Self.day, description: "IRS Form 1040 tax return")
            try add("/Users/me/Downloads/invoice-march.pdf", "pdf", "pdf", size: 120_000, at: Self.march2019, description: "Invoice for plumbing repair")
            try add("/Users/me/Downloads/Setup.dmg", "other", "dmg", size: 300_000_000, at: Self.july2023)
            try add("/Users/me/Downloads/beach copy.jpg", "image", "jpg", size: 4_000_000, at: Self.july2023, description: "Kyle on the beach near the water", tags: ["beach"], people: [1], hash: 7)
            try add("/lib/Trips/beach original.jpg", "image", "jpg", size: 4_000_000, at: Self.july2023, description: "Kyle on the beach near the water", tags: ["beach"], people: [1], hash: 7)
            // Undescribed beach photos (tags only).
            for n in 0..<3 {
                try add("/lib/Trips/Beach 2023/extra_\(n).jpg", "image", "jpg", size: 3_500_000, at: Self.july2023 + 7_200, tags: ["beach"], people: [2])
            }
        }
        return (db, root)
    }

    private struct Case { let text: String; let check: (ChatService.Prepared) -> String? }

    private func ids(_ prepared: ChatService.Prepared) -> Set<Int64> { Set(prepared.hits.map(\.fileID)) }

    @Test("complex questions get the right files and numbers")
    func bench() async throws {
        let (db, root) = try await library()
        defer { try? FileManager.default.removeItem(at: root) }
        let paths = try await db.pool.read { sql in
            Dictionary(uniqueKeysWithValues: try Row.fetchAll(sql, sql: "SELECT id,path_text FROM files").map { ($0["id"] as Int64, $0["path_text"] as String) })
        }
        func every(_ prepared: ChatService.Prepared, _ test: (String) -> Bool, _ label: String) -> String? {
            let bad = prepared.hits.map(\.path).filter { !test($0) }
            return prepared.hits.isEmpty ? "no hits (\(prepared.message))" : bad.isEmpty ? nil : "\(label): \(bad.prefix(3)) — \(prepared.message)"
        }
        func says(_ prepared: ChatService.Prepared, _ needle: String) -> String? {
            prepared.message.contains(needle) ? nil : "message lacks “\(needle)”: \(prepared.message)"
        }
        let cases: [Case] = [
            Case(text: "how many dog photos do I have?") { says($0, "1,500") },
            Case(text: "how many photos of dogs from 2018?") { p in
                says(p, "dog") ?? (p.message.contains("100") ? "capped at 100: \(p.message)" : nil)
            },
            Case(text: "show me RAW photos of Kyle at the beach") { every($0, { $0.hasSuffix(".ARW") }, "non-RAW") },
            Case(text: "photos of Kyle and Maya at the beach") { p in
                every(p, { $0.contains("IMG_0") || $0.contains("IMG_1.") || $0.contains("IMG_2.") || $0.contains("IMG_3.") }, "not both people")
            },
            Case(text: "beach photos from July 2023") { every($0, { !$0.contains("2019") && !$0.hasSuffix(".mov") && !$0.hasSuffix(".mp4") }, "wrong date/kind") },
            Case(text: "largest beach videos") { p in p.hits.first.map { $0.path.hasSuffix("surf.mov") ? nil : "first is \($0.path)" } ?? "no hits (\(p.message))" },
            Case(text: "beach videos from 2019") { every($0, { $0.hasSuffix("beach_2019.mp4") }, "wrong") },
            Case(text: "how much space do beach videos take?") { says($0, "3.4 GB") },
            Case(text: "pdfs in downloads about taxes") { every($0, { $0.hasSuffix("tax return.pdf") }, "wrong") },
            Case(text: "invoices from 2019") { every($0, { $0.contains("invoice") }, "wrong") },
            Case(text: "beach photos not described yet") { every($0, { $0.contains("extra_") }, "described") },
            Case(text: "duplicate beach photos") { p in ids(p).isEmpty ? "no hits (\(p.message))" : ids(p).allSatisfy { paths[$0]?.contains("beach") == true } ? nil : "\(p.message)" },
            Case(text: "oldest dog photos") { p in
                guard let first = p.hits.first else { return "no hits (\(p.message))" }
                return first.path.hasSuffix("dog_399.jpg") || first.path.hasSuffix("dog_799.jpg") || first.path.hasSuffix("dog_1199.jpg") ? nil : "first is \(first.path)"
            },
            Case(text: "videos of Kyle surfing") { every($0, { $0.hasSuffix("surf.mov") }, "wrong") },
            Case(text: "Kyle at sunset") { every($0, { $0.hasSuffix(".ARW") }, "wrong") },
            Case(text: "what's the biggest file in Downloads?") { p in p.hits.first.map { $0.path.hasSuffix("Setup.dmg") ? nil : "first is \($0.path)" } ?? "no hits (\(p.message))" },
            Case(text: "how many photos of Maya?") { says($0, "7") },
            Case(text: "how many JPGs of Kyle from 2023?") { says($0, "14") },
            Case(text: "beach or sunset photos of Kyle") { p in
                every(p, { $0.contains("Beach 2023") || $0.contains("beach") }, "wrong")
            },
            Case(text: "beach photos without Maya") { p in
                p.hits.isEmpty ? "no hits (\(p.message))" : p.hits.contains { $0.path.contains("extra_") || $0.path.hasSuffix("IMG_0.jpg") } ? "has Maya: \(p.message)" : nil
            },
            Case(text: "show me the 3 largest files") { p in p.hits.count == 3 && p.hits.first?.path.hasSuffix("surf.mov") == true ? nil : "\(p.hits.count) hits, first \(p.hits.first?.path ?? "-") — \(p.message)" },
            Case(text: "videos bigger than 1 GB") { every($0, { $0.hasSuffix("surf.mov") }, "too small") },
            Case(text: "pdfs smaller than 500 KB") { every($0, { $0.contains("invoice") }, "too big") },
            Case(text: "which folder has the most beach photos?") { says($0, "Beach 2023") },
            Case(text: "how many photos did I take in July 2023?") { says($0, "23 photos") },
            Case(text: "how many beach photos are there?") { says($0, "23 photos") },
            Case(text: "count the photos with Kyle") { says($0, "20 photos") },
            Case(text: "what's the total size of Kyle's videos?") { says($0, "2.5 GB") },
            Case(text: "documents in Downloads mentioning invoice") { every($0, { $0.contains("invoice") }, "wrong") },
            Case(text: "photos of Kyle sorted by size") { p in p.hits.first.map { $0.path.hasSuffix(".ARW") ? nil : "first is \($0.path)" } ?? "no hits (\(p.message))" },
            Case(text: "find the biggest video of Kyle") { p in p.hits.first.map { $0.path.hasSuffix("surf.mov") ? nil : "first is \($0.path)" } ?? "no hits (\(p.message))" },
            Case(text: "do I have any duplicates in Downloads?") { every($0, { $0.contains("beach copy") }, "wrong") },
            Case(text: "what's taking up the most space?") { says($0, "surf.mov") },
            Case(text: "how many photos per year?") { p in says(p, "2019") ?? says(p, "2023") },
            Case(text: "Kyle's photos from summer 2023") { every($0, { $0.contains("2023") || $0.contains("beach") }, "wrong") },
            Case(text: "show me sunset photos taken with Kyle in them") { p in
                every(p, { $0.hasSuffix(".ARW") }, "wrong") ?? (p.message.contains("them") ? "kept a pronoun: \(p.message)" : nil)
            },
            Case(text: "pictures of the sea") { every($0, { $0.contains("IMG_") }, "wrong") },
            Case(text: "how many summer photos of Kyle?") { says($0, "20 photos") },
            Case(text: "dog photos from the winter") { p in
                every(p, { $0.contains("dog_") }, "not a dog") ?? (p.message.contains("winter (any year)") ? nil : "no season filter: \(p.message)")
            },
        ]
        var failures: [String] = []
        let followUps: [(String, String, (ChatService.Prepared) -> String?)] = [
            ("beach photos of Kyle", "only the RAW ones", { every($0, { $0.hasSuffix(".ARW") }, "non-RAW") }),
            ("dog photos", "how many are from 2018?", { p in p.message.contains("dog") && !p.message.contains("1,500") ? nil : "\(p.message)" }),
            ("beach videos", "how much space do duplicates use?", { p in p.message.hasPrefix("Extra copies of identical files use 4.0 MB") ? nil : "\(p.message)" }),
            ("beach photos", "any duplicates among those?", { p in p.hits.isEmpty || p.message.contains("matching") ? nil : "\(p.message)" }),
        ]
        for (first, second, check) in followUps {
            let conversation = UUID().uuidString
            try await db.pool.write { sql in
                try sql.execute(sql: "INSERT INTO catalog_chat(id,conversation_id,role,text,created_at) VALUES(?,?,'user',?,0),(?,?,'user',?,1)",
                                arguments: [UUID().uuidString, conversation, first, UUID().uuidString, conversation, second])
            }
            let request = ChatRequest(requestID: UUID().uuidString, conversationID: conversation, action: "send", text: second)
            let prepared = try await ChatService.prepare(request, text: second, database: db)
            if let problem = check(prepared) { failures.append("“\(first)” then “\(second)” → \(problem) [\(prepared.hits.count) hits]") }
        }
        for item in cases {
            let request = ChatRequest(requestID: UUID().uuidString, conversationID: UUID().uuidString, action: "send", text: item.text)
            let prepared = try await ChatService.prepare(request, text: item.text, database: db)
            if let problem = item.check(prepared) { failures.append("“\(item.text)” → \(problem) [\(prepared.hits.count) hits]") }
        }
        if let out = ProcessInfo.processInfo.environment["FILEID_CHAT_BENCH_OUT"] {
            try? failures.joined(separator: "\n").write(toFile: out, atomically: true, encoding: .utf8)
        }
        #expect(failures.isEmpty, "\(failures.count) of \(cases.count + followUps.count) failed:\n\(failures.joined(separator: "\n"))")
    }
}
