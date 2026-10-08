import Foundation
import GRDB
import FileIDShared

enum CatalogTakes {
    private struct Score: Codable, Equatable {
        var eventID: String
        var fileID: Int64
        var outcomeScore: Double?
        var qualityScore: Double?
        var confidence: Double
        var explanation: String
        var sourceRevision: String
        var modelVersion: String
        var preferred: Bool
        var stale: Bool
    }

    private struct Snapshot: Codable, Equatable {
        var event: CatalogEvent
        var scores: [Score]
    }

    static func handle(_ request: CatalogRequest, database: Database) async throws -> CatalogResponse {
        switch request.action {
        case "listEvents":
            let events = try await database.pool.read { db in try list(db, query: request.query) }
            return CatalogResponse(requestID: request.requestID, status: "ok", events: events)
        case "takeGroup":
            guard let eventID = request.eventID else { throw CatalogStore.InvalidRequest() }
            return try await database.pool.read { db in try group(db, eventID: eventID, requestID: request.requestID) }
        case "saveEvent":
            guard var event = request.event else { throw CatalogStore.InvalidRequest() }
            event.title = event.title.trimmingCharacters(in: .whitespacesAndNewlines)
            event.goal = event.goal.trimmingCharacters(in: .whitespacesAndNewlines)
            event.fileIDs = Array(Set(event.fileIDs)).sorted()
            guard valid(event) else { throw CatalogStore.InvalidRequest() }
            event.userEdited = true
            let savedEvent = event
            try await database.pool.write { db in
                let before = try snapshot(db, eventID: savedEvent.id)
                for fileID in savedEvent.fileIDs {
                    guard try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM files WHERE id=?)", arguments: [fileID]) == true else {
                        throw CatalogStore.InvalidRequest()
                    }
                }
                try db.execute(sql: "INSERT INTO catalog_events(id,title,goal,user_edited) VALUES(?,?,?,1) ON CONFLICT(id) DO UPDATE SET title=excluded.title,goal=excluded.goal,user_edited=1",
                               arguments: [savedEvent.id, savedEvent.title, savedEvent.goal])
                try db.execute(sql: "DELETE FROM catalog_event_files WHERE event_id=?", arguments: [savedEvent.id])
                for fileID in savedEvent.fileIDs {
                    try db.execute(sql: "INSERT INTO catalog_event_files(event_id,file_id) VALUES(?,?)", arguments: [savedEvent.id, fileID])
                }
                try journal(db, kind: "event", eventID: savedEvent.id, fileID: nil, before: before, after: snapshot(db, eventID: savedEvent.id))
            }
            return try await database.pool.read { db in try group(db, eventID: savedEvent.id, requestID: request.requestID) }
        case "deleteEvent":
            guard let eventID = request.eventID, !eventID.isEmpty else { throw CatalogStore.InvalidRequest() }
            try await database.pool.write { db in
                guard let before = try snapshot(db, eventID: eventID) else { throw CatalogStore.InvalidRequest() }
                try db.execute(sql: "DELETE FROM catalog_events WHERE id=?", arguments: [eventID])
                try journal(db, kind: "event", eventID: eventID, fileID: nil, before: before, after: Optional<Snapshot>.none)
            }
            return CatalogResponse(requestID: request.requestID, status: "ok", events: try await database.pool.read { db in try list(db, query: nil) })
        case "undoEventEdit":
            guard let eventID = request.eventID else { throw CatalogStore.InvalidRequest() }
            try await database.pool.write { db in
                let row = try correction(db, kind: "event", eventID: eventID, fileID: nil)
                let before = try decode(Snapshot?.self, row.before)
                let after = try decode(Snapshot?.self, row.after)
                guard try snapshot(db, eventID: eventID) == after else { throw CatalogStore.InvalidRequest() }
                try db.execute(sql: "DELETE FROM catalog_events WHERE id=?", arguments: [eventID])
                if let before { try restore(db, snapshot: before) }
                try db.execute(sql: "UPDATE catalog_operations SET state='undone' WHERE id=?", arguments: [row.id])
            }
            return CatalogResponse(requestID: request.requestID, status: "ok", events: try await database.pool.read { db in try list(db, query: nil) })
        case "setTakeFeedback":
            guard let feedback = request.takeFeedback,
                  feedback.fileID > 0,
                  feedback.outcomeScore.map({ $0.isFinite && (0...1).contains($0) }) ?? true else { throw CatalogStore.InvalidRequest() }
            try await database.pool.write { db in
                guard try member(db, eventID: feedback.eventID, fileID: feedback.fileID) else { throw CatalogStore.InvalidRequest() }
                let before = try score(db, eventID: feedback.eventID, fileID: feedback.fileID)
                let revision = try CatalogStore.revision(db, fileID: feedback.fileID)
                let quality = before.flatMap { !$0.stale && $0.sourceRevision == revision ? $0.qualityScore : nil }
                let current = Score(eventID: feedback.eventID, fileID: feedback.fileID,
                                    outcomeScore: feedback.outcomeScore, qualityScore: quality,
                                    confidence: 1, explanation: "Reviewed by you", sourceRevision: revision,
                                    modelVersion: "user", preferred: feedback.preferred, stale: false)
                try upsert(db, score: current)
                try journal(db, kind: "take", eventID: feedback.eventID, fileID: feedback.fileID, before: before, after: current)
            }
            return try await database.pool.read { db in try group(db, eventID: feedback.eventID, requestID: request.requestID) }
        case "undoTakeFeedback":
            guard let eventID = request.eventID, let fileID = request.fileID,
                  try await database.pool.read({ db in try member(db, eventID: eventID, fileID: fileID) }) else { throw CatalogStore.InvalidRequest() }
            try await database.pool.write { db in
                let row = try correction(db, kind: "take", eventID: eventID, fileID: fileID)
                let before = try decode(Score?.self, row.before)
                let after = try decode(Score?.self, row.after)
                guard try score(db, eventID: eventID, fileID: fileID) == after else { throw CatalogStore.InvalidRequest() }
                try db.execute(sql: "DELETE FROM catalog_take_scores WHERE event_id=? AND file_id=?", arguments: [eventID, fileID])
                if let before { try upsert(db, score: before) }
                try db.execute(sql: "UPDATE catalog_operations SET state='undone' WHERE id=?", arguments: [row.id])
            }
            return try await database.pool.read { db in try group(db, eventID: eventID, requestID: request.requestID) }
        default:
            throw CatalogStore.InvalidRequest()
        }
    }

    static func recommend(event: CatalogEvent, takes: [CatalogTake]) -> CatalogTakeRecommendation {
        func result(_ status: String, _ fileIDs: [Int64], _ reason: String) -> CatalogTakeRecommendation {
            CatalogTakeRecommendation(eventID: event.id, status: status, fileIDs: fileIDs, reason: reason)
        }
        guard !event.goal.isEmpty, takes.count >= 2 else {
            return result("insufficient", [], "Describe the desired outcome and include at least two takes.")
        }
        let preferred = takes.filter { $0.preferred && !$0.stale }
        if preferred.count == 1 { return result("preferred", [preferred[0].fileID], "Marked as preferred by you.") }
        if preferred.count > 1 { return result("tie", preferred.map(\.fileID), "More than one take is marked preferred.") }
        guard takes.allSatisfy({ !$0.stale && $0.outcomeScore != nil && ($0.confidence ?? 0) >= 0.65 }) else {
            return result("insufficient", [], "Some takes lack current outcome evidence; review them before choosing a winner.")
        }
        let bestOutcome = takes.compactMap(\.outcomeScore).max() ?? 0
        guard bestOutcome >= 0.5 else { return result("insufficient", [], "No take has evidence of the desired outcome.") }
        let outcomeLeaders = takes.filter { ($0.outcomeScore ?? 0) >= bestOutcome - 0.05 }
        if outcomeLeaders.count == 1 {
            return result("ranked", [outcomeLeaders[0].fileID], "Best supported desired outcome; technical quality was considered separately.")
        }
        if outcomeLeaders.allSatisfy({ $0.qualityScore != nil }) {
            let sorted = outcomeLeaders.sorted { ($0.qualityScore ?? 0) > ($1.qualityScore ?? 0) }
            if let first = sorted.first, sorted.count > 1,
               (first.qualityScore ?? 0) - (sorted[1].qualityScore ?? 0) >= 0.1 {
                return result("ranked", [first.fileID], "Desired outcomes tie; this take has better technical quality.")
            }
        }
        return result("tie", outcomeLeaders.map(\.fileID), "The supported outcomes are too close to choose one take.")
    }

    private static func valid(_ event: CatalogEvent) -> Bool {
        !event.id.isEmpty && event.id.count <= 200 && !event.title.isEmpty && event.title.count <= 200
            && !event.goal.isEmpty && event.goal.count <= 500 && (2...100).contains(event.fileIDs.count)
            && event.fileIDs.allSatisfy { $0 > 0 }
    }

    private static func list(_ db: GRDB.Database, query: String?) throws -> [CatalogEvent] {
        let search = query?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard search.count <= 200 else { throw CatalogStore.InvalidRequest() }
        let rows = try Row.fetchAll(db, sql: "SELECT id,title,goal,user_edited FROM catalog_events WHERE ?='' OR instr(lower(title),lower(?))>0 OR instr(lower(goal),lower(?))>0 ORDER BY title,id LIMIT 100",
                                    arguments: [search, search, search])
        return try rows.map { row in
            let id: String = row["id"]
            let ids = try Int64.fetchAll(db, sql: "SELECT file_id FROM catalog_event_files WHERE event_id=? ORDER BY file_id", arguments: [id])
            return CatalogEvent(id: id, title: row["title"], goal: row["goal"], fileIDs: ids, userEdited: row["user_edited"])
        }
    }

    private static func event(_ db: GRDB.Database, id: String) throws -> CatalogEvent? {
        guard let row = try Row.fetchOne(db, sql: "SELECT id,title,goal,user_edited FROM catalog_events WHERE id=?", arguments: [id]) else { return nil }
        let ids = try Int64.fetchAll(db, sql: "SELECT file_id FROM catalog_event_files WHERE event_id=? ORDER BY file_id", arguments: [id])
        return CatalogEvent(id: id, title: row["title"], goal: row["goal"], fileIDs: ids, userEdited: row["user_edited"])
    }

    private static func group(_ db: GRDB.Database, eventID: String, requestID: String) throws -> CatalogResponse {
        guard let event = try event(db, id: eventID) else { throw CatalogStore.InvalidRequest() }
        let rows = try Row.fetchAll(db, sql: """
            SELECT f.id AS file_id,f.path_text,s.outcome_score,s.quality_score,s.confidence,s.explanation,
                   s.source_revision,s.model_version,s.preferred,s.stale
            FROM catalog_event_files ef JOIN files f ON f.id=ef.file_id
            LEFT JOIN catalog_take_scores s ON s.event_id=ef.event_id AND s.file_id=ef.file_id
            WHERE ef.event_id=? ORDER BY f.id
            """, arguments: [eventID])
        let takes: [CatalogTake] = rows.map { row in
            let id: Int64 = row["file_id"]
            let sourceRevision: String? = row["source_revision"]
            let storedStale: Bool? = row["stale"]
            let currentRevision = try? CatalogStore.revision(db, fileID: id)
            return CatalogTake(eventID: eventID, fileID: id, path: row["path_text"],
                               outcomeScore: row["outcome_score"], qualityScore: row["quality_score"],
                               confidence: row["confidence"], explanation: row["explanation"],
                               sourceRevision: sourceRevision, modelVersion: row["model_version"],
                               preferred: (row["preferred"] as Bool?) ?? false,
                               stale: (storedStale ?? false) || (sourceRevision != nil && sourceRevision != currentRevision))
        }
        return CatalogResponse(requestID: requestID, status: "ok", events: [event], takes: takes,
                               recommendation: recommend(event: event, takes: takes))
    }

    private static func member(_ db: GRDB.Database, eventID: String, fileID: Int64) throws -> Bool {
        try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM catalog_event_files WHERE event_id=? AND file_id=?)", arguments: [eventID, fileID]) ?? false
    }

    private static func score(_ db: GRDB.Database, eventID: String, fileID: Int64) throws -> Score? {
        guard let row = try Row.fetchOne(db, sql: "SELECT * FROM catalog_take_scores WHERE event_id=? AND file_id=?", arguments: [eventID, fileID]) else { return nil }
        return Score(eventID: eventID, fileID: fileID, outcomeScore: row["outcome_score"], qualityScore: row["quality_score"],
                     confidence: row["confidence"], explanation: row["explanation"], sourceRevision: row["source_revision"],
                     modelVersion: row["model_version"], preferred: row["preferred"], stale: row["stale"])
    }

    private static func snapshot(_ db: GRDB.Database, eventID: String) throws -> Snapshot? {
        guard let event = try event(db, id: eventID) else { return nil }
        let ids = try Int64.fetchAll(db, sql: "SELECT file_id FROM catalog_take_scores WHERE event_id=? ORDER BY file_id", arguments: [eventID])
        return Snapshot(event: event, scores: try ids.compactMap { try score(db, eventID: eventID, fileID: $0) })
    }

    private static func restore(_ db: GRDB.Database, snapshot: Snapshot) throws {
        let event = snapshot.event
        try db.execute(sql: "INSERT INTO catalog_events(id,title,goal,user_edited) VALUES(?,?,?,?)",
                       arguments: [event.id, event.title, event.goal, event.userEdited])
        for fileID in event.fileIDs {
            try db.execute(sql: "INSERT INTO catalog_event_files(event_id,file_id) VALUES(?,?)", arguments: [event.id, fileID])
        }
        for score in snapshot.scores { try upsert(db, score: score) }
    }

    private static func upsert(_ db: GRDB.Database, score: Score) throws {
        try db.execute(sql: """
            INSERT INTO catalog_take_scores(event_id,file_id,outcome_score,quality_score,confidence,explanation,source_revision,model_version,preferred,stale)
            VALUES(?,?,?,?,?,?,?,?,?,?) ON CONFLICT(event_id,file_id) DO UPDATE SET
            outcome_score=excluded.outcome_score,quality_score=excluded.quality_score,confidence=excluded.confidence,
            explanation=excluded.explanation,source_revision=excluded.source_revision,model_version=excluded.model_version,
            preferred=excluded.preferred,stale=excluded.stale
            """, arguments: [score.eventID, score.fileID, score.outcomeScore, score.qualityScore, score.confidence,
                             score.explanation, score.sourceRevision, score.modelVersion, score.preferred, score.stale])
    }

    private static func journal<T: Encodable>(_ db: GRDB.Database, kind: String, eventID: String, fileID: Int64?, before: T?, after: T?) throws {
        let id = UUID().uuidString
        let encoder = JSONEncoder()
        let beforeJSON = String(decoding: try encoder.encode(before), as: UTF8.self)
        let afterJSON = String(decoding: try encoder.encode(after), as: UTF8.self)
        let plan = String(decoding: try encoder.encode(["eventID": eventID, "fileID": fileID.map(String.init) ?? ""]), as: UTF8.self)
        let now = Date().timeIntervalSince1970
        try db.execute(sql: "INSERT INTO catalog_corrections(id,file_id,kind,before_json,after_json,created_at) VALUES(?,?,?,?,?,?)",
                       arguments: [id, fileID, kind, beforeJSON, afterJSON, now])
        try db.execute(sql: "INSERT INTO catalog_operations(id,plan_json,inverse_json,state,created_at) VALUES(?,?,?,'completed',?)",
                       arguments: [id, plan, beforeJSON, now])
    }

    private static func correction(_ db: GRDB.Database, kind: String, eventID: String, fileID: Int64?) throws -> (id: String, before: String, after: String) {
        let idText = fileID.map(String.init) ?? ""
        guard let row = try Row.fetchOne(db, sql: """
            SELECT o.id,c.before_json,c.after_json FROM catalog_operations o
            JOIN catalog_corrections c ON c.id=o.id
            WHERE c.kind=? AND o.state='completed' AND json_extract(o.plan_json,'$.eventID')=?
              AND json_extract(o.plan_json,'$.fileID')=?
            ORDER BY o.rowid DESC LIMIT 1
            """, arguments: [kind, eventID, idText]) else { throw CatalogStore.InvalidRequest() }
        return (row["id"], row["before_json"], row["after_json"])
    }

    private static func decode<T: Decodable>(_ type: T.Type, _ text: String) throws -> T {
        try JSONDecoder().decode(type, from: Data(text.utf8))
    }
}
