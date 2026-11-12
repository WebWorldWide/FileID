import Foundation

// IPC DTOs (shared/ipc-schema) the MLX code in FileIDVLMCore returns. They live
// here, below FileIDShared, so that code needs no FileIDShared; codegen
// `--check` reads this file with the other Swift mirrors.

public struct ModelBenchmark: Codable, Sendable, Equatable {
    public let measuredAt: String
    public let hardware: String
    public let loadSeconds: Double
    public let promptTokensPerSecond: Double?
    public let generatedTokensPerSecond: Double?
    public let secondsPerImage: Double?
    public let peakResidentGB: Double?
    public let jsonValidRate: Double?
    public let probeAccuracy: Double?
    public let tunedArguments: [String]?

    public init(measuredAt: String, hardware: String, loadSeconds: Double, promptTokensPerSecond: Double? = nil,
                generatedTokensPerSecond: Double? = nil, secondsPerImage: Double? = nil, peakResidentGB: Double? = nil,
                jsonValidRate: Double? = nil, probeAccuracy: Double? = nil, tunedArguments: [String]? = nil) {
        self.measuredAt = measuredAt
        self.hardware = hardware
        self.loadSeconds = loadSeconds
        self.promptTokensPerSecond = promptTokensPerSecond
        self.generatedTokensPerSecond = generatedTokensPerSecond
        self.secondsPerImage = secondsPerImage
        self.peakResidentGB = peakResidentGB
        self.jsonValidRate = jsonValidRate
        self.probeAccuracy = probeAccuracy
        self.tunedArguments = tunedArguments
    }
}

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
