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
        /// Absolute filesystem `rootPath`, an optional human-readable
        /// `rootDisplay` (defaults to `rootPath` when nil), and `rescan`
        /// (force every file to be reprocessed even when already current).
        /// Mirrors the schema's StartScan shape byte-for-byte — the app
        /// resolves the security-scoped bookmark to a path before sending.
        case startScan(rootPath: String, rootDisplay: String?, rescan: Bool)
        case pauseScan
        case resumeScan
        case cancelScan
        case requestStatus
        case shutdown
        case runFaceClustering
        case deepAnalyzeFile(fileID: Int64, modelKind: String)
        case deepAnalyzeFolder(pathPrefix: String, modelKind: String)
        /// `tagsOnly` runs the fast one-VLM-call/file pass (background
        /// auto-tag chain) instead of full caption + smart-rename + tags.
        /// The schema marks it optional (defaults false); the Windows
        /// engine always serializes it, so a Windows-emitted command
        /// decodes cleanly. Any extra schema key (e.g. `proposeRenames`)
        /// is ignored by Swift's keyed decoder.
        case deepAnalyzeAll(modelKind: String, skipExisting: Bool, tagsOnly: Bool)
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
        /// Record a user "different people" verdict for a suggested pair so
        /// findMergeSuggestions stops re-suggesting it. Keyed on stable
        /// anchor face ids so it survives re-clustering. Windows-originated;
        /// the mac engine returns the structured not-implemented pointer.
        case markPersonsDifferent(sourcePersonID: Int64, destinationPersonID: Int64, sourceAnchorFaceID: Int64, destinationAnchorFaceID: Int64)
        /// Truncate all learned library state (tags, faces, captions,
        /// embeddings) in-process on the engine's writer connection — no file
        /// deletion. Engine replies with a `libraryWiped` event. Empty payload.
        case wipeLibrary
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

    /// Custom decode so a move emitted by a Windows engine that omits
    /// `confidence` (it's `skip_serializing_if = "String::is_empty"` there)
    /// still decodes — `confidence` defaults to "" when absent, matching the
    /// "empty on older engines" semantics. `tier`/`reason` are optional.
    /// `encode(to:)` stays auto-synthesized.
    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        fileID = try c.decode(Int64.self, forKey: .fileID)
        source = try c.decode(String.self, forKey: .source)
        destination = try c.decode(String.self, forKey: .destination)
        category = try c.decode(String.self, forKey: .category)
        tier = try c.decodeIfPresent(String.self, forKey: .tier)
        confidence = try c.decodeIfPresent(String.self, forKey: .confidence) ?? ""
        reason = try c.decodeIfPresent(String.self, forKey: .reason)
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
        // ── Windows-originated reply events. The mac engine doesn't emit
        //    these yet (the equivalent flows return synchronously on mac),
        //    but the app must DECODE them so a shared/cross-platform engine
        //    or test corpus round-trips. Each mirrors the schema's DTO. ──
        case restructurePlan(RestructurePlan)
        case restructureApplyResult(RestructureApplyResult)
        case bulkActionResult(BulkActionResult)
        case clipTextEmbedding(ClipTextEmbedding)
        case mergeSuggestions(MergeSuggestions)
        case hardwareReprobed(HardwareReprobed)
        case libraryWiped(LibraryWiped)
        case thumbnailGenerated(ThumbnailGenerated)
    }
}

// MARK: - DTOs

public struct EngineInfo: Codable, Sendable {
    public let version: String
    public let pid: Int32
    public let workerCap: Int
    public let physicalMemoryGB: Double
    /// CPU + GPU detection result the engine made on startup. Optional so
    /// older engines (that don't emit it) still decode — `nil` when absent.
    public let hardware: HardwareInfo?

    public init(version: String, pid: Int32, workerCap: Int, physicalMemoryGB: Double,
                hardware: HardwareInfo? = nil) {
        self.version = version
        self.pid = pid
        self.workerCap = workerCap
        self.physicalMemoryGB = physicalMemoryGB
        self.hardware = hardware
    }
}

/// CPU/GPU/NPU detection snapshot. Mirrors the schema's HardwareInfo +
/// the Windows engine's `HardwareInfo`. Every field is optional/defaulted so
/// an older engine that omits the V15.9 adaptive-utilization fields (or omits
/// `hardware` entirely) still decodes cleanly.
public struct HardwareInfo: Codable, Sendable {
    /// "nvidia" / "amd" / "intel" / "qualcomm" / "other" / "none".
    public let gpuVendor: String
    /// Friendly adapter name as reported by the OS graphics API.
    public let adapterName: String?
    /// EP the engine picked: "cuda" / "tensorrt" / "directml" / "openvino"
    /// / "qnn" / "cpu" (mac: "coreml").
    public let executionProvider: String
    public let physicalCpuCores: Int
    public let cudaPackPresent: Bool
    public let openvinoPackPresent: Bool
    public let qnnPackPresent: Bool
    /// Contextual recommendation; empty when already on the optimal path.
    public let recommendation: String
    // ── V15.9 adaptive-utilization diagnostics (all optional/defaulted). ──
    public let pCores: Int
    public let eCores: Int
    public let logicalCpuCores: Int
    public let workerCap: Int
    public let ramTotalMB: Int
    public let ramAvailableMB: Int
    public let memoryTier: String
    public let vramMB: Int
    public let npuPresent: Bool
    public let powerSource: String
    public let batteryPercent: Int?
    public let activeProfile: String

    public init(
        gpuVendor: String = "none",
        adapterName: String? = nil,
        executionProvider: String = "cpu",
        physicalCpuCores: Int = 0,
        cudaPackPresent: Bool = false,
        openvinoPackPresent: Bool = false,
        qnnPackPresent: Bool = false,
        recommendation: String = "",
        pCores: Int = 0,
        eCores: Int = 0,
        logicalCpuCores: Int = 0,
        workerCap: Int = 0,
        ramTotalMB: Int = 0,
        ramAvailableMB: Int = 0,
        memoryTier: String = "",
        vramMB: Int = 0,
        npuPresent: Bool = false,
        powerSource: String = "",
        batteryPercent: Int? = nil,
        activeProfile: String = ""
    ) {
        self.gpuVendor = gpuVendor
        self.adapterName = adapterName
        self.executionProvider = executionProvider
        self.physicalCpuCores = physicalCpuCores
        self.cudaPackPresent = cudaPackPresent
        self.openvinoPackPresent = openvinoPackPresent
        self.qnnPackPresent = qnnPackPresent
        self.recommendation = recommendation
        self.pCores = pCores
        self.eCores = eCores
        self.logicalCpuCores = logicalCpuCores
        self.workerCap = workerCap
        self.ramTotalMB = ramTotalMB
        self.ramAvailableMB = ramAvailableMB
        self.memoryTier = memoryTier
        self.vramMB = vramMB
        self.npuPresent = npuPresent
        self.powerSource = powerSource
        self.batteryPercent = batteryPercent
        self.activeProfile = activeProfile
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        gpuVendor = try c.decodeIfPresent(String.self, forKey: .gpuVendor) ?? "none"
        adapterName = try c.decodeIfPresent(String.self, forKey: .adapterName)
        executionProvider = try c.decodeIfPresent(String.self, forKey: .executionProvider) ?? "cpu"
        physicalCpuCores = try c.decodeIfPresent(Int.self, forKey: .physicalCpuCores) ?? 0
        cudaPackPresent = try c.decodeIfPresent(Bool.self, forKey: .cudaPackPresent) ?? false
        openvinoPackPresent = try c.decodeIfPresent(Bool.self, forKey: .openvinoPackPresent) ?? false
        qnnPackPresent = try c.decodeIfPresent(Bool.self, forKey: .qnnPackPresent) ?? false
        recommendation = try c.decodeIfPresent(String.self, forKey: .recommendation) ?? ""
        pCores = try c.decodeIfPresent(Int.self, forKey: .pCores) ?? 0
        eCores = try c.decodeIfPresent(Int.self, forKey: .eCores) ?? 0
        logicalCpuCores = try c.decodeIfPresent(Int.self, forKey: .logicalCpuCores) ?? 0
        workerCap = try c.decodeIfPresent(Int.self, forKey: .workerCap) ?? 0
        ramTotalMB = try c.decodeIfPresent(Int.self, forKey: .ramTotalMB) ?? 0
        ramAvailableMB = try c.decodeIfPresent(Int.self, forKey: .ramAvailableMB) ?? 0
        memoryTier = try c.decodeIfPresent(String.self, forKey: .memoryTier) ?? ""
        vramMB = try c.decodeIfPresent(Int.self, forKey: .vramMB) ?? 0
        npuPresent = try c.decodeIfPresent(Bool.self, forKey: .npuPresent) ?? false
        powerSource = try c.decodeIfPresent(String.self, forKey: .powerSource) ?? ""
        batteryPercent = try c.decodeIfPresent(Int.self, forKey: .batteryPercent)
        activeProfile = try c.decodeIfPresent(String.self, forKey: .activeProfile) ?? ""
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
    public let kind: String          // image|video|pdf|doc|audio|other
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
    /// For errors pertaining to a specific model/pack install (e.g.
    /// `mobileclip_s2`, `cuda_pack_x64`), the model id — lets the app route
    /// the error to the right install slot. Optional/`nil` when absent.
    public let modelKind: String?

    public init(kind: String, message: String, path: String? = nil, modelKind: String? = nil) {
        self.kind = kind
        self.message = message
        self.path = path
        self.modelKind = modelKind
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

// MARK: - Windows-originated reply DTOs
//
// These mirror the schema's $defs (and the Windows engine's IPC structs)
// so the macOS app can DECODE events emitted by a cross-platform engine /
// the shared test corpus. The mac engine doesn't emit them yet.

public struct RestructureCategoryCount: Codable, Sendable {
    public let category: String
    public let count: Int

    public init(category: String, count: Int) {
        self.category = category
        self.count = count
    }
}

public struct FolderClassificationCounts: Codable, Sendable {
    public let anchorFolders: Int
    public let mixedFolders: Int
    public let junkFolders: Int

    public init(anchorFolders: Int, mixedFolders: Int, junkFolders: Int) {
        self.anchorFolders = anchorFolders
        self.mixedFolders = mixedFolders
        self.junkFolders = junkFolders
    }
}

public struct RestructurePlan: Codable, Sendable {
    public let libraryRoot: String
    public let moves: [RestructureMove]
    public let categoryCounts: [RestructureCategoryCount]
    /// Engine-authoritative folder classification counts. Nil on older engines.
    public let folderClassifications: FolderClassificationCounts?

    public init(libraryRoot: String, moves: [RestructureMove],
                categoryCounts: [RestructureCategoryCount],
                folderClassifications: FolderClassificationCounts? = nil) {
        self.libraryRoot = libraryRoot
        self.moves = moves
        self.categoryCounts = categoryCounts
        self.folderClassifications = folderClassifications
    }
}

public struct RestructureApplyResult: Codable, Sendable {
    public let applied: Int
    public let failed: Int
    /// Surfaces a "Developer Mode required for symlinks" message; nil otherwise.
    public let privilegeError: String?

    public init(applied: Int, failed: Int, privilegeError: String? = nil) {
        self.applied = applied
        self.failed = failed
        self.privilegeError = privilegeError
    }
}

public struct BulkActionItem: Codable, Sendable {
    public let fileID: Int64?
    public let ok: Bool
    public let message: String?

    public init(fileID: Int64? = nil, ok: Bool, message: String? = nil) {
        self.fileID = fileID
        self.ok = ok
        self.message = message
    }
}

public struct BulkActionResult: Codable, Sendable {
    /// Originating command's discriminator; the trashFiles reply additionally
    /// carries the undo batch id as a ":<uuid>" suffix.
    public let action: String
    public let succeeded: Int
    public let failed: Int
    public let messages: [BulkActionItem]

    public init(action: String, succeeded: Int, failed: Int, messages: [BulkActionItem]) {
        self.action = action
        self.succeeded = succeeded
        self.failed = failed
        self.messages = messages
    }
}

public struct ClipTextEmbedding: Codable, Sendable {
    public let queryID: String
    public let query: String
    /// 512-d L2-normalized float32 embedding from the CLIP text encoder.
    public let embedding: [Float]

    public init(queryID: String, query: String, embedding: [Float]) {
        self.queryID = queryID
        self.query = query
        self.embedding = embedding
    }
}

public struct MergeSuggestionPair: Codable, Sendable {
    public let sourcePersonID: Int64
    public let destinationPersonID: Int64
    public let similarity: Double
    public let sourceAnchorFaceID: Int64
    public let destinationAnchorFaceID: Int64
    public let sourceMemberCount: Int
    public let destinationMemberCount: Int

    public init(sourcePersonID: Int64, destinationPersonID: Int64, similarity: Double,
                sourceAnchorFaceID: Int64, destinationAnchorFaceID: Int64,
                sourceMemberCount: Int, destinationMemberCount: Int) {
        self.sourcePersonID = sourcePersonID
        self.destinationPersonID = destinationPersonID
        self.similarity = similarity
        self.sourceAnchorFaceID = sourceAnchorFaceID
        self.destinationAnchorFaceID = destinationAnchorFaceID
        self.sourceMemberCount = sourceMemberCount
        self.destinationMemberCount = destinationMemberCount
    }
}

public struct MergeSuggestions: Codable, Sendable {
    public let pairs: [MergeSuggestionPair]

    public init(pairs: [MergeSuggestionPair]) {
        self.pairs = pairs
    }
}

/// Reply to `verifyCudaPack`. Fresh `HardwareInfo` snapshot + an optional
/// human-readable `diagnostics` string explaining a negative probe.
public struct HardwareReprobed: Codable, Sendable {
    public let hardware: HardwareInfo
    public let diagnostics: String?

    public init(hardware: HardwareInfo, diagnostics: String? = nil) {
        self.hardware = hardware
        self.diagnostics = diagnostics
    }
}

/// Reply to `wipeLibrary`. `ok` is true when every table was truncated;
/// `message` carries the error on failure.
public struct LibraryWiped: Codable, Sendable {
    public let ok: Bool
    public let message: String?

    public init(ok: Bool, message: String? = nil) {
        self.ok = ok
        self.message = message
    }
}

/// Reply to `generateVideoThumbnail`. `bytes` is a base64-encoded 192px JPEG
/// (aspect-preserved, long side = 192) — a base64 String, NOT a number array.
public struct ThumbnailGenerated: Codable, Sendable {
    public let path: String
    public let modifiedAt: Double?
    public let bytes: String

    public init(path: String, modifiedAt: Double? = nil, bytes: String) {
        self.path = path
        self.modifiedAt = modifiedAt
        self.bytes = bytes
    }
}
