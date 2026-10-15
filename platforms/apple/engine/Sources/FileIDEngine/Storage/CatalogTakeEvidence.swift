import CryptoKit
import Foundation
import GRDB

struct CatalogTakeEvidence {
    struct Goal: Codable, Sendable, Equatable {
        let eventID: String
        let text: String
    }

    private struct Record: Codable {
        let goal: Goal
        let evidence: TakeOutcomeAnalysis.Evidence
    }

    static func goals(_ db: GRDB.Database, fileID: Int64) throws -> [Goal] {
        try Row.fetchAll(db, sql: "SELECT e.id,e.goal FROM catalog_events e JOIN catalog_event_files ef ON ef.event_id=e.id WHERE ef.file_id=? AND length(trim(e.goal))>0 ORDER BY e.id", arguments: [fileID]).map {
            Goal(eventID: $0["id"], text: $0["goal"])
        }
    }

    static func version(goal: Goal, modelVersion: String) -> String {
        "take-outcome-sequence-v1/" + modelVersion + "/goal=" + digest(goal.text)
    }

    static func windowID(goal: Goal, fileID: Int64, sourceRevision: String, modelVersion: String,
                         window: TimelineMomentAnalysis.Window) -> String {
        "take-window:\(digest(goal.eventID)):\(fileID):\(sourceRevision):\(modelVersion):\(Int(window.start * 1_000))"
    }

    static func current(_ db: GRDB.Database, goal: Goal, fileID: Int64, sourceRevision: String, jobID: String) throws -> Bool {
        guard try String.fetchOne(db, sql: "SELECT state FROM catalog_jobs WHERE id=?", arguments: [jobID]) == "running",
              try CatalogStore.revision(db, fileID: fileID) == sourceRevision else { return false }
        return try Bool.fetchOne(db, sql: "SELECT EXISTS(SELECT 1 FROM catalog_events e JOIN catalog_event_files ef ON ef.event_id=e.id WHERE e.id=? AND e.goal=? AND ef.file_id=?)", arguments: [goal.eventID,goal.text,fileID]) ?? false
    }

    static func processed(_ db: GRDB.Database, id: String, sourceRevision: String, modelVersion: String) throws -> Bool {
        guard let row = try Row.fetchOne(db, sql: "SELECT p.text FROM catalog_passages p JOIN catalog_coverage c ON c.id=p.id WHERE p.id=? AND p.source_revision=? AND p.model_version=? AND p.stale=0 AND c.source_revision=p.source_revision AND c.model_version=p.model_version AND c.status='sampled'", arguments: [id,sourceRevision,modelVersion]),
              let text: String = row["text"],
              let record = try? JSONDecoder().decode(Record.self, from: Data(text.utf8)),
              TakeOutcomeAnalysis.valid(record.evidence),
              modelVersion.hasSuffix("/goal=" + digest(record.goal.text)),
              id.hasPrefix("take-window:" + digest(record.goal.eventID) + ":") else { return false }
        return true
    }

    static func begin(_ db: GRDB.Database, goal: Goal, fileID: Int64, sourceRevision: String,
                      modelVersion: String, window: TimelineMomentAnalysis.Window, jobID: String) throws -> Bool {
        guard try current(db, goal: goal, fileID: fileID, sourceRevision: sourceRevision, jobID: jobID) else { return false }
        let id = windowID(goal: goal, fileID: fileID, sourceRevision: sourceRevision, modelVersion: modelVersion, window: window)
        try db.execute(sql: "INSERT OR REPLACE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?,?,?,?,'incomplete',?,?)", arguments: [id,fileID,window.start,window.end,sourceRevision,modelVersion])
        return true
    }

    static func persist(_ db: GRDB.Database, evidence: TakeOutcomeAnalysis.Evidence, goal: Goal, fileID: Int64,
                        sourceRevision: String, modelVersion: String, window: TimelineMomentAnalysis.Window,
                        jobID: String) throws -> Bool {
        guard try current(db, goal: goal, fileID: fileID, sourceRevision: sourceRevision, jobID: jobID),
              TakeOutcomeAnalysis.valid(evidence), evidence.start == window.start, evidence.end == window.end else { return false }
        let id = windowID(goal: goal, fileID: fileID, sourceRevision: sourceRevision, modelVersion: modelVersion, window: window)
        let json = String(decoding: try JSONEncoder().encode(Record(goal: goal, evidence: evidence)), as: UTF8.self)
        try db.execute(sql: "INSERT OR REPLACE INTO catalog_passages(id,file_id,start_seconds,end_seconds,text,source_revision,model_version,confidence) VALUES(?,?,?,?,?,?,?,?)", arguments: [id,fileID,evidence.observedStart ?? window.start,evidence.observedEnd ?? window.end,json,sourceRevision,modelVersion,evidence.observation.confidence])
        try db.execute(sql: "INSERT OR REPLACE INTO catalog_coverage(id,file_id,start_seconds,end_seconds,status,source_revision,model_version) VALUES(?,?,?,?,'sampled',?,?)", arguments: [id,fileID,window.start,window.end,sourceRevision,modelVersion])
        return true
    }

    static func publish(_ db: GRDB.Database, goal: Goal, fileID: Int64, sourceRevision: String,
                        modelVersion: String, windows: [TimelineMomentAnalysis.Window], jobID: String) throws -> Bool {
        guard try current(db, goal: goal, fileID: fileID, sourceRevision: sourceRevision, jobID: jobID),
              try String.fetchOne(db, sql: "SELECT model_version FROM catalog_take_scores WHERE event_id=? AND file_id=?", arguments: [goal.eventID,fileID]) != "user" else { return false }
        var evidence: [TakeOutcomeAnalysis.Evidence] = []
        for window in windows {
            let id = windowID(goal: goal, fileID: fileID, sourceRevision: sourceRevision, modelVersion: modelVersion, window: window)
            guard try processed(db, id: id, sourceRevision: sourceRevision, modelVersion: modelVersion),
                  let text = try String.fetchOne(db, sql: "SELECT text FROM catalog_passages WHERE id=?", arguments: [id]),
                  let record = try? JSONDecoder().decode(Record.self, from: Data(text.utf8)),
                  record.goal == goal else { continue }
            evidence.append(record.evidence)
        }
        let assessment = TakeOutcomeAnalysis.assess(evidence, expectedWindows: windows)
        try db.execute(sql: """
            INSERT INTO catalog_take_scores(event_id,file_id,outcome_score,quality_score,confidence,explanation,source_revision,model_version,preferred,stale)
            VALUES(?,?,?,NULL,?,?,?,?,0,0)
            ON CONFLICT(event_id,file_id) DO UPDATE SET
              outcome_score=excluded.outcome_score,
              quality_score=CASE WHEN catalog_take_scores.source_revision=excluded.source_revision AND catalog_take_scores.stale=0 THEN catalog_take_scores.quality_score ELSE NULL END,
              confidence=excluded.confidence,explanation=excluded.explanation,source_revision=excluded.source_revision,
              model_version=excluded.model_version,preferred=0,stale=0
            """, arguments: [goal.eventID,fileID,assessment.outcome,assessment.confidence,assessment.explanation,sourceRevision,modelVersion])
        return true
    }

    private static func digest(_ value: String) -> String {
        SHA256.hash(data: Data(value.utf8)).map { String(format: "%02x", $0) }.joined()
    }
}
