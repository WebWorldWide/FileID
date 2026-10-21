import Foundation

public struct ChatMessage: Codable, Sendable, Equatable, Identifiable {
    public let id: String
    public let role: String
    public let text: String
    public let createdAt: Double
    public init(id: String, role: String, text: String, createdAt: Double) {
        self.id = id; self.role = role; self.text = text; self.createdAt = createdAt
    }
}

public struct ChatContext: Codable, Sendable, Equatable {
    public var page: String?
    public var fileIDs: [Int64]?
    public var personIDs: [Int64]?
    public var folder: String?
    public var libraryRoot: String?
    public init(page: String? = nil, fileIDs: [Int64]? = nil, personIDs: [Int64]? = nil, folder: String? = nil, libraryRoot: String? = nil) {
        self.page = page; self.fileIDs = fileIDs; self.personIDs = personIDs; self.folder = folder; self.libraryRoot = libraryRoot
    }
}

public struct ChatRequest: Codable, Sendable, Equatable {
    public let requestID: String
    public let conversationID: String
    public let action: String
    public let text: String?
    public let useModel: Bool?
    public let modelKind: String?
    public let context: ChatContext?
    public let queryVector: [Float]?
    public let embeddingModel: String?
    public init(requestID: String, conversationID: String, action: String, text: String? = nil, useModel: Bool? = nil, modelKind: String? = nil,
                context: ChatContext? = nil, queryVector: [Float]? = nil, embeddingModel: String? = nil) {
        self.requestID = requestID; self.conversationID = conversationID; self.action = action; self.text = text; self.useModel = useModel; self.modelKind = modelKind
        self.context = context; self.queryVector = queryVector; self.embeddingModel = embeddingModel
    }
}

public struct ChatRename: Codable, Sendable, Equatable {
    public let fileID: Int64
    public let newName: String
    public init(fileID: Int64, newName: String) { self.fileID = fileID; self.newName = newName }
}

/// A proposed FileID action carried by a chat reply. Clients run it only
/// through the existing command it maps to, after a confirmation card when
/// `requiresConfirmation` is set.
public struct ChatAction: Codable, Sendable, Equatable, Identifiable {
    public static let kinds = [
        "scanFolder", "rescan", "describeFiles", "stopJob", "findDuplicates", "moveDuplicatesToTrash",
        "suggestFolderLayout", "applyFolderLayout", "undoLastMove", "renameToSuggested", "namePerson",
        "mergePeople", "findMoments", "exportFiles", "openPage", "search", "changeSetting"
    ]
    public static let pages = ["library", "people", "cleanup", "deepAnalyze", "restructure", "settings", "search", "tools", "chat"]
    public static let jobs = ["scan", "describe", "organize", "moments", "all"]
    public static let settings = ["cleanupAutoTag", "backgroundMotion", "chatSummaries"]

    public var id: String
    public var kind: String
    public var title: String
    public var detail: String
    public var requiresConfirmation: Bool
    public var source: String
    public var undo: String?
    public var fileCount: Int?
    public var byteCount: Int64?
    public var examples: [String]?
    public var fileIDs: [Int64]?
    public var keeperFileIDs: [Int64]?
    public var path: String?
    public var query: String?
    public var kinds: [String]?
    public var page: String?
    public var personID: Int64?
    public var personIDs: [Int64]?
    public var name: String?
    public var job: String?
    public var setting: String?
    public var value: Bool?
    public var renames: [ChatRename]?

    public init(id: String, kind: String, title: String, detail: String, requiresConfirmation: Bool, source: String = "parser",
                undo: String? = nil, fileCount: Int? = nil, byteCount: Int64? = nil, examples: [String]? = nil,
                fileIDs: [Int64]? = nil, keeperFileIDs: [Int64]? = nil, path: String? = nil, query: String? = nil,
                kinds: [String]? = nil, page: String? = nil, personID: Int64? = nil, personIDs: [Int64]? = nil,
                name: String? = nil, job: String? = nil, setting: String? = nil, value: Bool? = nil, renames: [ChatRename]? = nil) {
        self.id = id; self.kind = kind; self.title = title; self.detail = detail; self.requiresConfirmation = requiresConfirmation
        self.source = source; self.undo = undo; self.fileCount = fileCount; self.byteCount = byteCount; self.examples = examples
        self.fileIDs = fileIDs; self.keeperFileIDs = keeperFileIDs; self.path = path; self.query = query; self.kinds = kinds
        self.page = page; self.personID = personID; self.personIDs = personIDs; self.name = name; self.job = job
        self.setting = setting; self.value = value; self.renames = renames
    }
}

public struct ChatResponse: Codable, Sendable, Equatable {
    public let requestID: String
    public let conversationID: String
    public let status: String
    public let message: String
    public let messages: [ChatMessage]
    public let hits: [CatalogHit]
    public let actions: [ChatAction]?
    public init(requestID: String, conversationID: String, status: String, message: String, messages: [ChatMessage] = [], hits: [CatalogHit] = [], actions: [ChatAction]? = nil) {
        self.requestID = requestID; self.conversationID = conversationID; self.status = status; self.message = message; self.messages = messages; self.hits = hits
        self.actions = actions?.isEmpty == true ? nil : actions
    }
}
