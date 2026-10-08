import Foundation

public struct ToolRecipe: Codable, Sendable, Equatable {
    public var kind: String
    public var format: String
    public var maxDimension: Int
    public var allowUpscale: Bool?
    public init(kind: String, format: String, maxDimension: Int = 4096, allowUpscale: Bool? = nil) {
        self.kind = kind; self.format = format; self.maxDimension = maxDimension; self.allowUpscale = allowUpscale
    }
}

public struct ToolCapability: Codable, Sendable, Equatable {
    public var id: String
    public var available: Bool
    public var inputFormats: [String]
    public var outputFormats: [String]
    public var detail: String
    public init(id: String, available: Bool, inputFormats: [String], outputFormats: [String], detail: String) {
        self.id = id; self.available = available; self.inputFormats = inputFormats; self.outputFormats = outputFormats; self.detail = detail
    }
}

public struct ToolOutput: Codable, Sendable, Equatable {
    public var fileID: Int64
    public var sourcePath: String
    public var outputPath: String
    public var state: String
    public var message: String
    public init(fileID: Int64, sourcePath: String, outputPath: String, state: String = "pending", message: String = "") {
        self.fileID = fileID; self.sourcePath = sourcePath; self.outputPath = outputPath; self.state = state; self.message = message
    }
}

public struct ToolRequest: Codable, Sendable, Equatable {
    public var requestID: String
    public var action: String
    public var fileIDs: [Int64]?
    public var destination: String?
    public var recipe: ToolRecipe?
    public var operationID: String?
    public var destinationBookmark: Data?
    public init(
        requestID: String,
        action: String,
        fileIDs: [Int64]? = nil,
        destination: String? = nil,
        recipe: ToolRecipe? = nil,
        operationID: String? = nil,
        destinationBookmark: Data? = nil
    ) {
        self.requestID = requestID
        self.action = action
        self.fileIDs = fileIDs
        self.destination = destination
        self.recipe = recipe
        self.operationID = operationID
        self.destinationBookmark = destinationBookmark
    }
}

public struct ToolResponse: Codable, Sendable, Equatable {
    public var requestID: String
    public var status: String
    public var message: String
    public var operationID: String?
    public var outputs: [ToolOutput]
    public var capabilities: [ToolCapability]
    public init(requestID: String, status: String = "ok", message: String = "", operationID: String? = nil, outputs: [ToolOutput] = [], capabilities: [ToolCapability] = []) {
        self.requestID = requestID; self.status = status; self.message = message; self.operationID = operationID; self.outputs = outputs; self.capabilities = capabilities
    }
}
