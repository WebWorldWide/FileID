import Foundation

public struct CatalogChapter: Codable, Sendable, Equatable {
    public var id: String
    public var fileID: Int64
    public var startSeconds: Double
    public var endSeconds: Double
    public var title: String
    public var summary: String
    public var sourceRevision: String
    public var modelVersion: String
    public var confidence: Double
    public var userEdited: Bool
    public var stale: Bool
    public init(id: String, fileID: Int64, startSeconds: Double, endSeconds: Double, title: String, summary: String, sourceRevision: String, modelVersion: String, confidence: Double, userEdited: Bool, stale: Bool) {
        self.id = id
        self.fileID = fileID
        self.startSeconds = startSeconds
        self.endSeconds = endSeconds
        self.title = title
        self.summary = summary
        self.sourceRevision = sourceRevision
        self.modelVersion = modelVersion
        self.confidence = confidence
        self.userEdited = userEdited
        self.stale = stale
    }
}

public struct CatalogHit: Codable, Sendable, Equatable {
    public var fileID: Int64
    public var path: String
    public var kind: String
    public var text: String
    public var evidenceID: String?
    public var startSeconds: Double?
    public var page: Int?
    public init(fileID: Int64, path: String, kind: String, text: String, evidenceID: String? = nil, startSeconds: Double? = nil, page: Int? = nil) {
        self.fileID = fileID
        self.path = path
        self.kind = kind
        self.text = text
        self.evidenceID = evidenceID
        self.startSeconds = startSeconds
        self.page = page
    }
}

public struct CatalogJob: Codable, Sendable, Equatable {
    public var id: String
    public var kind: String
    public var fileIDs: [Int64]
    public var state: String
    public var progress: Double
    public var error: String?
    public var createdAt: Double
    public var updatedAt: Double
    public init(id: String, kind: String, fileIDs: [Int64] = [], state: String, progress: Double, error: String? = nil, createdAt: Double, updatedAt: Double) {
        self.id = id
        self.kind = kind
        self.fileIDs = fileIDs
        self.state = state
        self.progress = progress
        self.error = error
        self.createdAt = createdAt
        self.updatedAt = updatedAt
    }
}

public struct CatalogEvent: Codable, Sendable, Equatable {
    public var id: String
    public var title: String
    public var goal: String
    public var fileIDs: [Int64]
    public var userEdited: Bool

    public init(id: String, title: String, goal: String, fileIDs: [Int64], userEdited: Bool = true) {
        self.id = id
        self.title = title
        self.goal = goal
        self.fileIDs = fileIDs
        self.userEdited = userEdited
    }
}

public struct CatalogTakeFeedback: Codable, Sendable, Equatable {
    public var eventID: String
    public var fileID: Int64
    public var outcomeScore: Double?
    public var preferred: Bool

    public init(eventID: String, fileID: Int64, outcomeScore: Double? = nil, preferred: Bool = false) {
        self.eventID = eventID
        self.fileID = fileID
        self.outcomeScore = outcomeScore
        self.preferred = preferred
    }
}

public struct CatalogTake: Codable, Sendable, Equatable {
    public var eventID: String
    public var fileID: Int64
    public var path: String
    public var outcomeScore: Double?
    public var qualityScore: Double?
    public var confidence: Double?
    public var explanation: String?
    public var sourceRevision: String?
    public var modelVersion: String?
    public var preferred: Bool
    public var stale: Bool

    public init(eventID: String, fileID: Int64, path: String, outcomeScore: Double? = nil,
                qualityScore: Double? = nil, confidence: Double? = nil, explanation: String? = nil,
                sourceRevision: String? = nil, modelVersion: String? = nil, preferred: Bool = false, stale: Bool = false) {
        self.eventID = eventID
        self.fileID = fileID
        self.path = path
        self.outcomeScore = outcomeScore
        self.qualityScore = qualityScore
        self.confidence = confidence
        self.explanation = explanation
        self.sourceRevision = sourceRevision
        self.modelVersion = modelVersion
        self.preferred = preferred
        self.stale = stale
    }
}

public struct CatalogTakeRecommendation: Codable, Sendable, Equatable {
    public var eventID: String
    public var status: String
    public var fileIDs: [Int64]
    public var reason: String

    public init(eventID: String, status: String, fileIDs: [Int64], reason: String) {
        self.eventID = eventID
        self.status = status
        self.fileIDs = fileIDs
        self.reason = reason
    }
}

public struct CatalogTakeGroupMember: Codable, Sendable, Equatable {
    public var fileID: Int64
    public var path: String

    public init(fileID: Int64, path: String) {
        self.fileID = fileID
        self.path = path
    }
}

public struct CatalogTakeGroupSuggestion: Codable, Sendable, Equatable {
    public var members: [CatalogTakeGroupMember]
    public var similarity: Double
    public var reason: String

    public init(members: [CatalogTakeGroupMember], similarity: Double, reason: String) {
        self.members = members
        self.similarity = similarity
        self.reason = reason
    }
}

public struct CatalogTakeBox: Codable, Sendable, Equatable {
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double
    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x; self.y = y; self.width = width; self.height = height
    }
}

public struct CatalogTakeShot: Codable, Sendable, Equatable, Hashable {
    public var primaryFileID: Int64
    public var fileIDs: [Int64]
    public var path: String
    public var capturedAt: Double?
    public init(primaryFileID: Int64, fileIDs: [Int64], path: String, capturedAt: Double? = nil) {
        self.primaryFileID = primaryFileID; self.fileIDs = fileIDs; self.path = path; self.capturedAt = capturedAt
    }
}

public struct CatalogTakeAutoGroup: Codable, Sendable, Equatable, Identifiable {
    public var groupKey: String
    public var kind: String
    public var status: String
    public var capturedAt: Double
    public var similarity: Double
    public var shots: [CatalogTakeShot]
    public var id: String { groupKey }
    public var fileIDs: [Int64] { shots.flatMap(\.fileIDs) }
    public init(groupKey: String, kind: String, status: String, capturedAt: Double, similarity: Double, shots: [CatalogTakeShot]) {
        self.groupKey = groupKey; self.kind = kind; self.status = status
        self.capturedAt = capturedAt; self.similarity = similarity; self.shots = shots
    }
    private enum CodingKeys: String, CodingKey { case groupKey, kind, status, capturedAt, similarity, shots }
}

public struct CatalogTakeQuality: Codable, Sendable, Equatable {
    public var primaryFileID: Int64
    public var fileIDs: [Int64]
    public var path: String
    public var score: Double
    public var rank: Int
    public var recommended: Bool
    public var reasons: [String]
    public var sharpness: Double?
    public var exposure: Double?
    public var motionBlur: Bool?
    public var faceCount: Int?
    public var eyesClosed: Bool?
    public var faceCutOff: Bool?
    public var stability: Double?
    public var action: Double?
    public var peakSeconds: Double?
    public var subject: CatalogTakeBox?
    public var faces: [CatalogTakeBox]
    public init(primaryFileID: Int64, fileIDs: [Int64], path: String, score: Double, rank: Int, recommended: Bool, reasons: [String],
                sharpness: Double? = nil, exposure: Double? = nil, motionBlur: Bool? = nil, faceCount: Int? = nil, eyesClosed: Bool? = nil,
                faceCutOff: Bool? = nil, stability: Double? = nil, action: Double? = nil, peakSeconds: Double? = nil,
                subject: CatalogTakeBox? = nil, faces: [CatalogTakeBox] = []) {
        self.primaryFileID = primaryFileID; self.fileIDs = fileIDs; self.path = path; self.score = score; self.rank = rank
        self.recommended = recommended; self.reasons = reasons; self.sharpness = sharpness; self.exposure = exposure
        self.motionBlur = motionBlur; self.faceCount = faceCount; self.eyesClosed = eyesClosed; self.faceCutOff = faceCutOff
        self.stability = stability; self.action = action; self.peakSeconds = peakSeconds; self.subject = subject; self.faces = faces
    }
}

public struct CatalogTakeGoalRanking: Codable, Sendable, Equatable {
    public var goal: String
    public var status: String
    public var order: [Int64]
    public var metFileIDs: [Int64]
    public var summary: String
    public var model: String?
    public init(goal: String, status: String, order: [Int64], metFileIDs: [Int64], summary: String, model: String? = nil) {
        self.goal = goal; self.status = status; self.order = order; self.metFileIDs = metFileIDs; self.summary = summary; self.model = model
    }
}

public struct CatalogTakeDecision: Codable, Sendable, Equatable {
    public var groupKey: String
    public var decision: String
    public var fileIDs: [Int64]
    public init(groupKey: String, decision: String, fileIDs: [Int64]) {
        self.groupKey = groupKey; self.decision = decision; self.fileIDs = fileIDs
    }
}

public struct CatalogRequest: Codable, Sendable, Equatable {
    public var requestID: String
    public var action: String
    public var query: String?
    public var fileID: Int64?
    public var chapter: CatalogChapter?
    public var chapterID: String?
    public var jobID: String?
    public var fileIDs: [Int64]?
    public var resultScope: String?
    public var searchMode: String?
    public var queryVector: [Float]?
    public var embeddingModel: String?
    public var limit: Int?
    public var timelineMode: String?
    public var event: CatalogEvent?
    public var eventID: String?
    public var takeFeedback: CatalogTakeFeedback?
    public var takeDecision: CatalogTakeDecision?
    public init(requestID: String, action: String, query: String? = nil, fileID: Int64? = nil, chapter: CatalogChapter? = nil, chapterID: String? = nil, jobID: String? = nil, fileIDs: [Int64]? = nil, searchMode: String? = nil, queryVector: [Float]? = nil, embeddingModel: String? = nil, limit: Int? = nil, resultScope: String? = nil, timelineMode: String? = nil, event: CatalogEvent? = nil, eventID: String? = nil, takeFeedback: CatalogTakeFeedback? = nil, takeDecision: CatalogTakeDecision? = nil) {
        self.requestID = requestID
        self.action = action
        self.query = query
        self.fileID = fileID
        self.chapter = chapter
        self.chapterID = chapterID
        self.jobID = jobID
        self.fileIDs = fileIDs
        self.resultScope = resultScope
        self.searchMode = searchMode
        self.queryVector = queryVector
        self.embeddingModel = embeddingModel
        self.limit = limit
        self.timelineMode = timelineMode
        self.event = event
        self.eventID = eventID
        self.takeFeedback = takeFeedback
        self.takeDecision = takeDecision
    }
}

public struct CatalogResponse: Codable, Sendable, Equatable {
    public var requestID: String
    public var status: String
    public var message: String?
    public var hits: [CatalogHit]
    public var chapters: [CatalogChapter]
    public var jobs: [CatalogJob]
    public var events: [CatalogEvent]?
    public var takes: [CatalogTake]?
    public var recommendation: CatalogTakeRecommendation?
    public var suggestedTakeGroups: [CatalogTakeGroupSuggestion]?
    public var takeGroups: [CatalogTakeAutoGroup]?
    public var takeQualities: [CatalogTakeQuality]?
    public var takeGoalRanking: CatalogTakeGoalRanking?
    public init(requestID: String, status: String, message: String? = nil, hits: [CatalogHit] = [], chapters: [CatalogChapter] = [], jobs: [CatalogJob] = [], events: [CatalogEvent]? = nil, takes: [CatalogTake]? = nil, recommendation: CatalogTakeRecommendation? = nil, suggestedTakeGroups: [CatalogTakeGroupSuggestion]? = nil, takeGroups: [CatalogTakeAutoGroup]? = nil, takeQualities: [CatalogTakeQuality]? = nil, takeGoalRanking: CatalogTakeGoalRanking? = nil) {
        self.requestID = requestID
        self.status = status
        self.message = message
        self.hits = hits
        self.chapters = chapters
        self.jobs = jobs
        self.events = events
        self.takes = takes
        self.recommendation = recommendation
        self.suggestedTakeGroups = suggestedTakeGroups
        self.takeGroups = takeGroups
        self.takeQualities = takeQualities
        self.takeGoalRanking = takeGoalRanking
    }
}
