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
    public init(requestID: String, action: String, query: String? = nil, fileID: Int64? = nil, chapter: CatalogChapter? = nil, chapterID: String? = nil, jobID: String? = nil, fileIDs: [Int64]? = nil, searchMode: String? = nil, queryVector: [Float]? = nil, embeddingModel: String? = nil, limit: Int? = nil, resultScope: String? = nil) {
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
    }
}

public struct CatalogResponse: Codable, Sendable, Equatable {
    public var requestID: String
    public var status: String
    public var message: String?
    public var hits: [CatalogHit]
    public var chapters: [CatalogChapter]
    public var jobs: [CatalogJob]
    public init(requestID: String, status: String, message: String? = nil, hits: [CatalogHit] = [], chapters: [CatalogChapter] = [], jobs: [CatalogJob] = []) {
        self.requestID = requestID
        self.status = status
        self.message = message
        self.hits = hits
        self.chapters = chapters
        self.jobs = jobs
    }
}
