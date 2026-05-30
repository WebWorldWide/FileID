// IPC protocol between the FileID app and the FileIDEngine CLI.
// Wire format: newline-delimited JSON. Each line is an envelope
// (`IPCCommand` from app→engine, `IPCEvent` from engine→app); the
// `payload` enum picks the variant.
import Foundation

// MARK: - Envelope

public struct IPCCommand: Codable, Sendable {
    public let id: String          // app-assigned UUID; engine echoes in any reply
    public let payload: Payload

    public init(id: String = UUID().uuidString, payload: Payload) {
        self.id = id
        self.payload = payload
    }

    public enum Payload: Codable, Sendable {
        case startScan(rootBookmark: Data, rootPathDisplay: String)
        case pauseScan
        case resumeScan
        case cancelScan
        case requestStatus
        case shutdown
        case runFaceClustering
        case deepAnalyzeFile(fileID: Int64, modelKind: String)
        case deepAnalyzeFolder(pathPrefix: String, modelKind: String)
        case deepAnalyzeAll(modelKind: String, skipExisting: Bool)
        case deepAnalyzeCancel
        /// Pre-fetch a VLM's weights into the swift-transformers HF
        /// cache without running inference. Used by the welcome-sheet
        /// onboarding flow to download the recommended VLM up front
        /// instead of having the user wait at first Deep Analyze run.
        case prewarmModel(modelKind: String)
        /// Cancel an in-flight prewarmModel. Lands at the next
        /// Task.checkCancellation point inside swift-transformers'
        /// fetch loop — usually within ~1 s. Safe no-op if no prewarm
        /// is active.
        case cancelPrewarm

        // ── Windows-originated commands ───────────────────────────
        // These land on mac only when the schema needs to round-trip
        // them (cross-platform tooling, shared test corpus). The mac
        // engine dispatcher returns a structured "not_implemented_yet"
        // error for each; equivalent flows on macOS go through their
        // pre-existing per-tab actions.
        case planRestructure(libraryRoot: String)
        case applyRestructure(libraryRoot: String, moves: [RestructureMove], useSymlinks: Bool)
        case applyTags(fileIDs: [Int64], tags: [String], mode: String)
        case renameFiles(renames: [RenameEntry])
        case trashFiles(fileIDs: [Int64])
        case mergeClusters(sourcePersonID: Int64, destinationPersonID: Int64)
        case embedTextQuery(query: String, queryID: String)
        case renamePerson(personID: Int64, title: String?, firstName: String?, middleName: String?, lastName: String?, suffix: String?)
        case markPersonsAsUnknown(personIDs: [Int64])
        case findMergeSuggestions
        case embedImageQuery(fileID: Int64, queryID: String)
        case restoreFromTrash(batchID: String)
        case revertMerge(sourcePersonID: Int64, destinationPersonID: Int64, faceIDsToRevert: [Int64])
        /// Windows-only: re-probe CUDA + cuDNN. Always returns
        /// `not_applicable_on_platform` on mac.
        case verifyCudaPack
    }
}

public struct RestructureMove: Codable, Sendable {
    public let fileID: Int64
    public let source: String
    public let destination: String
    public let category: String
    /// Source-folder tier — "Anchor" / "Mixed" / "Junk".
    public let tier: String?
    /// Butler confidence band — "auto" / "review" / "ask" (RESTRUCTURE.md §6).
    /// Empty when the engine didn't stamp one.
    public let confidence: String
    /// Plain-language "why filed here", shown in the drill-down.
    public let reason: String?

    public init(fileID: Int64, source: String, destination: String,
                category: String, tier: String? = nil,
                confidence: String = "", reason: String? = nil) {
        self.fileID = fileID
        self.source = source
        self.destination = destination
        self.category = category
        self.tier = tier
        self.confidence = confidence
        self.reason = reason
    }
}

public struct RenameEntry: Codable, Sendable {
    public let fileID: Int64
    public let newName: String

    public init(fileID: Int64, newName: String) {
        self.fileID = fileID
        self.newName = newName
    }
}

public struct IPCEvent: Codable, Sendable {
    public let t: Date
    public let payload: Payload

    public init(t: Date = Date(), payload: Payload) {
        self.t = t
        self.payload = payload
    }

    public enum Payload: Codable, Sendable {
        case ready(EngineInfo)
        case progress(ScanProgress)
        case phaseChanged(ScanPhase)
        case discoveryComplete(totalFiles: Int)
        case fileDone(FileDoneEvent)
        case batchSummary(BatchSummary)
        case scanComplete(ScanComplete)
        case error(EngineError)
        case log(LogLine)
        case faceClusteringComplete(FaceClusteringResult)
        case deepAnalyzeStarting(DeepAnalyzeStarting)
        case deepAnalyzeProgress(DeepAnalyzeProgress)
        case deepAnalyzeFileDone(DeepAnalyzeFileDone)
        case deepAnalyzeComplete(DeepAnalyzeComplete)
        case modelDownloadProgress(ModelDownloadProgress)
        case queueState(QueueState)
    }
}

// MARK: - DTOs

public struct EngineInfo: Codable, Sendable {
    public let version: String
    public let pid: Int32
    public let workerCap: Int
    public let physicalMemoryGB: Double

    public init(version: String, pid: Int32, workerCap: Int, physicalMemoryGB: Double) {
        self.version = version
        self.pid = pid
        self.workerCap = workerCap
        self.physicalMemoryGB = physicalMemoryGB
    }
}

public enum ScanPhase: String, Codable, Sendable {
    case idle
    case discovering
    case tagging
    case postScan          // face clustering, lazy embeds, etc.
    case completed
    case cancelled
    case failed
}

public struct ScanProgress: Codable, Sendable {
    public let sessionID: String
    public let phase: ScanPhase
    public let total: Int            // 0 until discovery completes
    public let discovered: Int
    public let processed: Int
    public let failed: Int
    public let filesPerSecond: Double
    public let etaSeconds: Double?
    public let residentMB: Int
    public let availableMB: Int

    public init(
        sessionID: String,
        phase: ScanPhase,
        total: Int,
        discovered: Int,
        processed: Int,
        failed: Int,
        filesPerSecond: Double,
        etaSeconds: Double?,
        residentMB: Int,
        availableMB: Int
    ) {
        self.sessionID = sessionID
        self.phase = phase
        self.total = total
        self.discovered = discovered
        self.processed = processed
        self.failed = failed
        self.filesPerSecond = filesPerSecond
        self.etaSeconds = etaSeconds
        self.residentMB = residentMB
        self.availableMB = availableMB
    }
}

public struct FileDoneEvent: Codable, Sendable {
    public let path: String
    public let kind: String          // image|video|pdf|doc|other
    public let totalMs: Double
    public let failed: Bool
    public let errorMessage: String?
    /// Pipeline stages skipped because the model didn't load
    /// (e.g. "face_detection"). Empty when every stage ran.
    public let skippedStages: [String]

    public init(path: String, kind: String, totalMs: Double, failed: Bool,
                errorMessage: String? = nil, skippedStages: [String] = []) {
        self.path = path
        self.kind = kind
        self.totalMs = totalMs
        self.failed = failed
        self.errorMessage = errorMessage
        self.skippedStages = skippedStages
    }
}

public struct BatchSummary: Codable, Sendable {
    public let batchIndex: Int
    public let filesInBatch: Int
    public let processedTotal: Int
    public let wallSeconds: Double
    public let filesPerSecond: Double
    public let utilization: Double      // sum(workerWith) / (wallSeconds * workerCap)
    public let visionP50Ms: Double
    public let visionP95Ms: Double
    public let clipP50Ms: Double
    public let clipP95Ms: Double
    public let storeInsertP50Ms: Double
    public let storeInsertP95Ms: Double
    public let residentMB: Int
    public let availableMB: Int

    public init(
        batchIndex: Int, filesInBatch: Int, processedTotal: Int,
        wallSeconds: Double, filesPerSecond: Double, utilization: Double,
        visionP50Ms: Double, visionP95Ms: Double,
        clipP50Ms: Double, clipP95Ms: Double,
        storeInsertP50Ms: Double, storeInsertP95Ms: Double,
        residentMB: Int, availableMB: Int
    ) {
        self.batchIndex = batchIndex
        self.filesInBatch = filesInBatch
        self.processedTotal = processedTotal
        self.wallSeconds = wallSeconds
        self.filesPerSecond = filesPerSecond
        self.utilization = utilization
        self.visionP50Ms = visionP50Ms
        self.visionP95Ms = visionP95Ms
        self.clipP50Ms = clipP50Ms
        self.clipP95Ms = clipP95Ms
        self.storeInsertP50Ms = storeInsertP50Ms
        self.storeInsertP95Ms = storeInsertP95Ms
        self.residentMB = residentMB
        self.availableMB = availableMB
    }
}

public struct ScanComplete: Codable, Sendable {
    public let sessionID: String
    public let totalFiles: Int
    public let processedFiles: Int
    public let failedFiles: Int
    public let totalSeconds: Double

    public init(
        sessionID: String, totalFiles: Int, processedFiles: Int,
        failedFiles: Int, totalSeconds: Double
    ) {
        self.sessionID = sessionID
        self.totalFiles = totalFiles
        self.processedFiles = processedFiles
        self.failedFiles = failedFiles
        self.totalSeconds = totalSeconds
    }
}

public enum JobCategory: String, Codable, Sendable {
    case scan
    case faceCluster
    case deepAnalyze
}

public struct QueuedJob: Codable, Sendable, Identifiable {
    public let id: String
    public let category: JobCategory
    public let title: String          // human-readable, e.g. "Scan Library"
    public let etaSeconds: Double?    // optional estimated wall time when queued

    public init(id: String, category: JobCategory, title: String, etaSeconds: Double?) {
        self.id = id
        self.category = category
        self.title = title
        self.etaSeconds = etaSeconds
    }
}

public struct QueueState: Codable, Sendable {
    public let running: QueuedJob?    // nil if idle
    public let pending: [QueuedJob]   // FIFO order
    public let totalEtaSeconds: Double?  // sum of running + pending if known

    public init(running: QueuedJob?, pending: [QueuedJob], totalEtaSeconds: Double?) {
        self.running = running
        self.pending = pending
        self.totalEtaSeconds = totalEtaSeconds
    }

    public var isIdle: Bool { running == nil && pending.isEmpty }
    public var depth: Int { (running == nil ? 0 : 1) + pending.count }
}

/// Streamed by the engine the moment a Deep Analyze command arrives,
/// then again as the runner advances through model load / target
/// resolution. Lets the UI show progressive feedback during the ~10s
/// VLM cold-load before the first per-file `deepAnalyzeProgress` fires.
public struct DeepAnalyzeStarting: Codable, Sendable {
    public let modelKind: String
    public let phase: Phase
    public let message: String

    public init(modelKind: String, phase: Phase, message: String) {
        self.modelKind = modelKind
        self.phase = phase
        self.message = message
    }

    public enum Phase: String, Codable, Sendable {
        case queued
        case loadingModel
        case resolvingTargets
    }
}

public struct DeepAnalyzeProgress: Codable, Sendable {
    public let processed: Int
    public let total: Int
    public let etaSeconds: Double?
    public let currentPath: String?
    public let modelKind: String
    /// V14.9-L1: partial caption text accumulated from per-token streaming.
    /// Engine throttles emission to 4 Hz so a fast VLM doesn't flood the
    /// sink. nil on pre-inference progress events ("starting file N").
    public let currentCaption: String?

    public init(processed: Int, total: Int, etaSeconds: Double?, currentPath: String?, modelKind: String, currentCaption: String? = nil) {
        self.processed = processed
        self.total = total
        self.etaSeconds = etaSeconds
        self.currentPath = currentPath
        self.modelKind = modelKind
        self.currentCaption = currentCaption
    }
}

public struct DeepAnalyzeFileDone: Codable, Sendable {
    public let fileID: Int64
    public let description: String
    public let proposedName: String?
    public let modelKind: String

    public init(fileID: Int64, description: String, proposedName: String?, modelKind: String) {
        self.fileID = fileID
        self.description = description
        self.proposedName = proposedName
        self.modelKind = modelKind
    }
}

public struct DeepAnalyzeComplete: Codable, Sendable {
    public let processed: Int
    public let failed: Int
    public let totalSeconds: Double
    public let modelKind: String
    public let cancelled: Bool

    public init(processed: Int, failed: Int, totalSeconds: Double, modelKind: String, cancelled: Bool) {
        self.processed = processed
        self.failed = failed
        self.totalSeconds = totalSeconds
        self.modelKind = modelKind
        self.cancelled = cancelled
    }
}

public struct ModelDownloadProgress: Codable, Sendable {
    public let modelKind: String
    public let fraction: Double           // 0..1
    public let message: String
    /// Real bytes downloaded so far. Sourced from swift-transformers'
    /// `Progress.completedUnitCount`. Optional because some progress
    /// callbacks (e.g. legacy ones, or non-byte-unit progresses) may
    /// not have meaningful byte counts.
    public let bytesDone: Int64?
    /// Real total bytes for the download. Sourced from swift-transformers'
    /// `Progress.totalUnitCount`. The welcome sheet's ETA math uses this
    /// instead of the hardcoded `AIModelKind.approxBytes` estimate so
    /// rates and ETAs match reality.
    public let totalBytes: Int64?

    public init(modelKind: String, fraction: Double, message: String,
                bytesDone: Int64? = nil, totalBytes: Int64? = nil) {
        self.modelKind = modelKind
        self.fraction = fraction
        self.message = message
        self.bytesDone = bytesDone
        self.totalBytes = totalBytes
    }
}

public struct FaceClusteringResult: Codable, Sendable {
    public let personCount: Int
    public let faceCount: Int
    public let unmatchedFaces: Int
    public let durationSeconds: Double

    public init(personCount: Int, faceCount: Int, unmatchedFaces: Int, durationSeconds: Double) {
        self.personCount = personCount
        self.faceCount = faceCount
        self.unmatchedFaces = unmatchedFaces
        self.durationSeconds = durationSeconds
    }
}

public struct EngineError: Codable, Sendable, Error {
    public let kind: String        // discovery_failed | vision_failed | db_failed | unknown
    public let message: String
    public let path: String?       // file path if applicable

    public init(kind: String, message: String, path: String? = nil) {
        self.kind = kind
        self.message = message
        self.path = path
    }
}

public struct LogLine: Codable, Sendable {
    public let level: Level
    public let message: String

    public init(level: Level, message: String) {
        self.level = level
        self.message = message
    }

    public enum Level: String, Codable, Sendable {
        case debug, info, warn, error
    }
}
