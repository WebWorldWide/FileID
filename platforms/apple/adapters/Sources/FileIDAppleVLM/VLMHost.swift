// Request/reply surface the C exports in FileIDAppleKit/VLMExports.swift call.
// Each request is one JSON object; it runs on its own task against the Swift
// engine's DeepAnalyze actor (Engine/DeepAnalyze.swift) and ends with exactly
// one `done` or `failed` event. The host decides what to run; this file only
// maps requests onto the engine's MLX code.
import AVFoundation
import CoreImage
import Foundation
import ImageIO
import MLX

public enum VLMEventKind: UInt32, Sendable {
    case token = 1
    case progress = 2
    case done = 3
    case failed = 4
}

struct VLMRequest: Decodable, Sendable {
    let op: String
    let model: VLMModelSpec?
    let path: String?
    let paths: [String]?
    let times: [Double]?
    let goal: String?
    let prompt: String?
    let maxTokens: Int?
    let imageSide: Int?
    let maxPixelSize: Int?
    let mediaKind: String?
    let documentText: String?
    let faceNames: [String]?
    let proposeFilename: Bool?
    let tagsOnly: Bool?
    let sceneCaption: Bool?
    let stream: Bool?
    let testCard: Bool?
    let quickLook: Bool?
}

struct VLMFailure: LocalizedError {
    let message: String
    var errorDescription: String? { message }
}

public final class VLMHost: @unchecked Sendable {
    public static let shared = VLMHost()

    private let lock = NSLock()
    private var tasks: [UInt64: Task<Void, Never>] = [:]
    private var next: UInt64 = 0
    private var closed = false

    public typealias Emit = @Sendable (VLMEventKind, Data) -> Void

    /// Starts `request`; returns its handle, or 0 when the host is shut down.
    /// `emit` is called from any thread and receives exactly one terminal event.
    public func submit(_ request: Data, emit: @escaping Emit) -> UInt64 {
        lock.lock()
        guard !closed else {
            lock.unlock()
            return 0
        }
        next &+= 1
        let handle = next
        let task = Task.detached(priority: .userInitiated) { [weak self] in
            await Self.run(request, emit: emit)
            self?.finish(handle)
        }
        tasks[handle] = task
        lock.unlock()
        return handle
    }

    public func cancel(_ handle: UInt64) {
        lock.lock()
        let task = tasks[handle]
        lock.unlock()
        task?.cancel()
    }

    /// Cancels every request and stops accepting new ones before the hard exit.
    public func shutdown() {
        lock.lock()
        closed = true
        let running = Array(tasks.values)
        lock.unlock()
        running.forEach { $0.cancel() }
    }

    var activeCount: Int {
        lock.lock(); defer { lock.unlock() }
        return tasks.count
    }

    private func finish(_ handle: UInt64) {
        lock.lock()
        tasks[handle] = nil
        lock.unlock()
    }

    private static func run(_ data: Data, emit: @escaping Emit) async {
        do {
            let request = try JSONDecoder().decode(VLMRequest.self, from: data)
            let reply = try await perform(request, emit: emit)
            // MLX ends a generation stream quietly when its task is cancelled.
            try Task.checkCancellation()
            emit(.done, try JSONSerialization.data(withJSONObject: reply, options: [.sortedKeys]))
        } catch {
            let cancelled = error is CancellationError || Task.isCancelled
            let message = cancelled ? "Cancelled." : error.localizedDescription
            let body: [String: Any] = ["message": message, "cancelled": cancelled]
            emit(.failed, (try? JSONSerialization.data(withJSONObject: body, options: [.sortedKeys])) ?? Data())
        }
    }

    static func perform(_ request: VLMRequest, emit: @escaping Emit) async throws -> [String: Any] {
        let analyzer = DeepAnalyze.shared
        switch request.op {
        case "status":
            return await status()
        case "load":
            guard let spec = request.model else { throw VLMFailure(message: "No model to load.") }
            VLMModelRegistry.shared.register(spec)
            try await analyzer.ensureLoaded(kind: AIModelKind(rawValue: spec.kind)) { fraction, message, done, total in
                let body: [String: Any] = ["fraction": fraction, "message": message, "bytesDone": done, "totalBytes": total]
                if let data = try? JSONSerialization.data(withJSONObject: body, options: [.sortedKeys]) { emit(.progress, data) }
            }
            try Task.checkCancellation()
            return await status()
        case "unload":
            await analyzer.unload()
            return await status()
        case "prefetch":
            guard let path = request.path else { throw VLMFailure(message: "No file.") }
            DecodePrefetch.shared.start(URL(fileURLWithPath: path), maxPixelSize: request.maxPixelSize ?? 768)
            return [:]
        case "analyze":
            guard let path = request.path else { throw VLMFailure(message: "No file.") }
            let streamed: @Sendable (String) async -> Void = { chunk in emit(.token, Data(chunk.utf8)) }
            let onToken = request.stream == true ? streamed : nil
            let result = await analyzer.analyze(
                imageURL: URL(fileURLWithPath: path),
                mediaKind: request.mediaKind.flatMap(FileKind.init(rawValue:)) ?? .image,
                documentText: request.documentText,
                faceNames: request.faceNames ?? [],
                proposeFilename: request.proposeFilename ?? true,
                tagsOnly: request.tagsOnly ?? false,
                sceneCaption: request.sceneCaption ?? false,
                onToken: onToken
            )
            try Task.checkCancellation()
            var reply: [String: Any] = ["description": result.description, "tags": result.tags]
            if let name = result.proposedName { reply["proposedName"] = name }
            return reply
        case "generate":
            let image: CIImage?
            if request.testCard == true {
                image = ModelBenchmarkRunner.syntheticImage()
            } else if let path = request.path {
                guard let decoded = await decode(path: path, maxPixelSize: request.maxPixelSize) else {
                    throw VLMFailure(message: "This file couldn't be read.")
                }
                image = decoded
            } else {
                image = nil
            }
            let sample = try await analyzer.benchmarkPass(
                prompt: request.prompt ?? "",
                image: image,
                maxTokens: request.maxTokens ?? 96,
                imageSide: request.imageSide ?? 448
            )
            return [
                "output": sample.output,
                "promptTokensPerSecond": finite(sample.promptTokensPerSecond),
                "generatedTokensPerSecond": finite(sample.generatedTokensPerSecond),
                "wallSeconds": sample.wallSeconds,
            ]
        case "chat":
            let answer = try await analyzer.answerCatalog(prompt: request.prompt ?? "") { chunk in
                emit(.token, Data(chunk.utf8))
            }
            return ["answer": answer]
        case "transcribe":
            guard let path = request.path else { throw VLMFailure(message: "No file.") }
            let url = URL(fileURLWithPath: path)
            let image: CGImage? = request.quickLook == true
                ? DeepAnalyze.quickLookThumbnail(url: url, maxPixelSize: request.maxPixelSize ?? 1536)
                : await DeepAnalyze.loadCGImage(url: url, maxPixelSize: request.maxPixelSize ?? 1536)
            guard let image else { throw VLMFailure(message: "This file couldn't be read.") }
            return ["text": try await analyzer.transcribeText(image)]
        case "sequence":
            let urls = (request.paths ?? []).map { URL(fileURLWithPath: $0) }
            let times = request.times ?? []
            let result = if let goal = request.goal {
                await analyzer.analyzeTakeOutcomeSequence(imageURLs: urls, times: times, goal: goal)
            } else {
                await analyzer.analyzeMomentSequence(imageURLs: urls, times: times)
            }
            try Task.checkCancellation()
            return ["description": result.description]
        case "benchmark":
            guard let spec = request.model else { throw VLMFailure(message: "No model to benchmark.") }
            VLMModelRegistry.shared.register(spec)
            let result = try await ModelBenchmarkRunner.run(kind: AIModelKind(rawValue: spec.kind)) { phase, done, total in
                let body: [String: Any] = ["phase": phase, "completed": done, "total": total]
                if let data = try? JSONSerialization.data(withJSONObject: body, options: [.sortedKeys]) { emit(.progress, data) }
            }
            let encoded = try JSONEncoder().encode(result)
            return ["benchmark": try JSONSerialization.jsonObject(with: encoded)]
        default:
            throw VLMFailure(message: "Unknown request \(request.op).")
        }
    }

    static func status() async -> [String: Any] {
        let analyzer = DeepAnalyze.shared
        var reply: [String: Any] = [
            "residentBytes": Memory.activeMemory + Memory.cacheMemory,
            "activeBytes": Memory.activeMemory,
            "cacheBytes": Memory.cacheMemory,
            "peakBytes": Memory.peakMemory,
            "footprintBytes": ModelBenchmarkRunner.physicalFootprintBytes(),
            "retainedMB": await ResourceScheduler.shared.retainedMB,
            "parked": await analyzer.isParked,
            "analyzing": await analyzer.isAnalyzing,
        ]
        if let kind = await analyzer.loadedModelKind { reply["loaded"] = kind.rawValue }
        return reply
    }

    private static func finite(_ value: Double) -> Any { value.isFinite ? value : NSNull() }

    /// The Swift engine's selection-check decode (`ChatSelection.image`): a
    /// bounded ImageIO decode, or the middle frame of a video. With no size,
    /// the whole image (a contact sheet the host drew).
    static func decode(path: String, maxPixelSize: Int?) async -> CIImage? {
        let url = URL(fileURLWithPath: path)
        if let side = maxPixelSize, DeepAnalyze.isVideoExtension(url.pathExtension) {
            let asset = AVURLAsset(url: url)
            guard let duration = try? await asset.load(.duration), duration.seconds.isFinite, duration.seconds > 0 else { return nil }
            let generator = AVAssetImageGenerator(asset: asset)
            generator.appliesPreferredTrackTransform = true
            generator.maximumSize = CGSize(width: side, height: side)
            guard let frame = try? await generator.image(at: CMTime(seconds: duration.seconds / 2, preferredTimescale: 600)).image else { return nil }
            return CIImage(cgImage: frame)
        }
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { return nil }
        if let side = maxPixelSize {
            return decodeBoundedImage(source, path: path, maxPixelSize: side).map { CIImage(cgImage: $0) }
        }
        return CGImageSourceCreateImageAtIndex(source, 0, nil).map { CIImage(cgImage: $0) }
    }
}
