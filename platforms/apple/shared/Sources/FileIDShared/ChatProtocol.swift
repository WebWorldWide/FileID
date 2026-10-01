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

public struct ChatRequest: Codable, Sendable, Equatable {
    public let requestID: String
    public let conversationID: String
    public let action: String
    public let text: String?
    public let useModel: Bool?
    public init(requestID: String, conversationID: String, action: String, text: String? = nil, useModel: Bool? = nil) {
        self.requestID = requestID; self.conversationID = conversationID; self.action = action; self.text = text; self.useModel = useModel
    }
}

public struct ChatResponse: Codable, Sendable, Equatable {
    public let requestID: String
    public let conversationID: String
    public let status: String
    public let message: String
    public let messages: [ChatMessage]
    public let hits: [CatalogHit]
    public init(requestID: String, conversationID: String, status: String, message: String, messages: [ChatMessage] = [], hits: [CatalogHit] = []) {
        self.requestID = requestID; self.conversationID = conversationID; self.status = status; self.message = message; self.messages = messages; self.hits = hits
    }
}
