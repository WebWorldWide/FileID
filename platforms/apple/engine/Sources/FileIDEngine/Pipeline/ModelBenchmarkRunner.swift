import Foundation
import CoreImage
import FileIDShared
import MLX

/// On-device MLX benchmark in the spirit of Magnitude's hardware scoring:
/// fixed prompts, a cold load, fresh runs, medians, peak memory and a
/// hardware fingerprint. Never downloads; only installed models qualify.
enum ModelBenchmarkRunner {
    static let runsPerWorkload = 2

    struct Sample: Sendable {
        let promptTokensPerSecond: Double
        let generatedTokensPerSecond: Double
        let wallSeconds: Double
        let output: String
    }

    struct Workload: Sendable {
        let phase: String
        let prompt: String
        let withImage: Bool
        let maxTokens: Int
        let expectsJSON: Bool
        let expectedAnswer: String?
    }

    static func workloads(vision: Bool) -> [Workload] {
        var list = [
            Workload(phase: "text", prompt: "Return only a JSON object with keys \"title\" (a short title) and \"tags\" (an array of exactly three lowercase words) for a family photo taken at the beach at sunset. No prose.", withImage: false, maxTokens: 96, expectsJSON: true, expectedAnswer: nil),
            Workload(phase: "rename", prompt: "Suggest one filename stem of 3 to 5 lowercase words joined by hyphens for a scanned electricity bill from March. Reply with the stem only.", withImage: false, maxTokens: 32, expectsJSON: false, expectedAnswer: nil),
        ]
        if vision {
            list.append(Workload(phase: "vision", prompt: "Return only a JSON object with keys \"description\" (one sentence) and \"tags\" (an array of up to five lowercase words) for this image. No prose.", withImage: true, maxTokens: 96, expectsJSON: true, expectedAnswer: nil))
            list.append(Workload(phase: "probe", prompt: "What colour is the large circle in this image? Answer with one word.", withImage: true, maxTokens: 8, expectsJSON: false, expectedAnswer: "red"))
        }
        return list
    }

    static func median(_ values: [Double]) -> Double? {
        let sorted = values.filter(\.isFinite).sorted()
        guard !sorted.isEmpty else { return nil }
        let mid = sorted.count / 2
        return sorted.count.isMultiple(of: 2) ? (sorted[mid - 1] + sorted[mid]) / 2 : sorted[mid]
    }

    /// Accepts a bare JSON object, optionally wrapped in a Markdown fence.
    static func isValidJSONObject(_ raw: String) -> Bool {
        var text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if text.hasPrefix("```") {
            text = text.components(separatedBy: "\n").dropFirst().joined(separator: "\n")
            if let fence = text.range(of: "```", options: .backwards) { text = String(text[..<fence.lowerBound]) }
        }
        guard let start = text.firstIndex(of: "{"), let end = text.lastIndex(of: "}"), start < end,
              let data = String(text[start...end]).data(using: .utf8) else { return false }
        return (try? JSONSerialization.jsonObject(with: data)) is [String: Any]
    }

    static func answerMatches(_ raw: String, expected: String) -> Bool {
        raw.lowercased().split(whereSeparator: { !$0.isLetter }).contains { $0 == Substring(expected) }
    }

    /// e.g. "Apple M1 Pro · 16 GB · 10 cores · macOS 15.5"
    static func hardwareFingerprint() -> String {
        var size = 0
        sysctlbyname("machdep.cpu.brand_string", nil, &size, nil, 0)
        var chip = "Mac"
        if size > 0 {
            var buffer = [CChar](repeating: 0, count: size)
            if sysctlbyname("machdep.cpu.brand_string", &buffer, &size, nil, 0) == 0 {
                chip = String(cString: buffer)
            }
        }
        let info = ProcessInfo.processInfo
        let memoryGB = Int((Double(info.physicalMemory) / 1_073_741_824).rounded())
        let os = info.operatingSystemVersion
        return "\(chip) · \(memoryGB) GB · \(info.activeProcessorCount) cores · macOS \(os.majorVersion).\(os.minorVersion)"
    }

    /// Deterministic 448×448 test card: red circle on a blue field with a
    /// yellow bar, so the colour probe has one right answer.
    static func syntheticImage() -> CIImage {
        let extent = CGRect(x: 0, y: 0, width: 448, height: 448)
        let background = CIImage(color: CIColor(red: 0.12, green: 0.32, blue: 0.78)).cropped(to: extent)
        let bar = CIImage(color: CIColor(red: 0.98, green: 0.82, blue: 0.1))
            .cropped(to: CGRect(x: 32, y: 24, width: 384, height: 40))
        let circle = CIFilter(name: "CIRadialGradient", parameters: [
            "inputCenter": CIVector(x: 224, y: 248),
            "inputRadius0": 120, "inputRadius1": 121,
            "inputColor0": CIColor(red: 0.9, green: 0.08, blue: 0.08, alpha: 1),
            "inputColor1": CIColor(red: 0, green: 0, blue: 0, alpha: 0),
        ])?.outputImage?.cropped(to: extent) ?? CIImage.empty()
        return circle.composited(over: bar.composited(over: background))
    }

    // MARK: - Run

    enum Failure: LocalizedError {
        case notInstalled(String)
        case busy
        var errorDescription: String? {
            switch self {
            case .notInstalled(let name): return "\(name) isn't downloaded. Benchmarks only run on installed models and never download."
            case .busy: return "Deep Analyze is running. Benchmark after it finishes."
            }
        }
    }

    static func run(
        kind: AIModelKind,
        isInstalled: (AIModelKind) -> Bool = { $0.isInstalledOnDisk() },
        progress: @escaping @Sendable (String, Int, Int) async -> Void
    ) async throws -> ModelBenchmark {
        guard isInstalled(kind) else { throw Failure.notInstalled(kind.displayName) }
        let analyzer = DeepAnalyze.shared
        guard await !analyzer.isAnalyzing else { throw Failure.busy }
        let jobs = workloads(vision: kind.supportsVision)
        let total = 1 + jobs.count * runsPerWorkload
        let previous = await analyzer.loadedModelKind

        await progress("load", 0, total)
        await analyzer.unload()
        Memory.peakMemory = 0
        let loadStart = ContinuousClock.now
        try await analyzer.ensureLoaded(kind: kind)
        let loadSeconds = durationSeconds(ContinuousClock.now - loadStart)
        await progress("load", 1, total)

        let image = syntheticImage()
        var prefill: [Double] = [], decode: [Double] = [], perImage: [Double] = []
        var jsonChecks = 0, jsonValid = 0, probeChecks = 0, probeHits = 0
        var completed = 1
        do {
            for job in jobs {
                for _ in 0..<runsPerWorkload {
                    try Task.checkCancellation()
                    await progress(job.phase, completed, total)
                    let sample = try await analyzer.benchmarkPass(prompt: job.prompt, image: job.withImage ? image : nil, maxTokens: job.maxTokens)
                    prefill.append(sample.promptTokensPerSecond)
                    decode.append(sample.generatedTokensPerSecond)
                    if job.withImage, job.expectedAnswer == nil { perImage.append(sample.wallSeconds) }
                    if job.expectsJSON { jsonChecks += 1; if isValidJSONObject(sample.output) { jsonValid += 1 } }
                    if let expected = job.expectedAnswer { probeChecks += 1; if answerMatches(sample.output, expected: expected) { probeHits += 1 } }
                    completed += 1
                }
            }
        } catch {
            if previous != kind { await analyzer.unload() }
            throw error
        }
        let peak = Double(Memory.peakMemory) / 1_073_741_824
        if previous != kind { await analyzer.unload() }
        await progress("done", total, total)

        return ModelBenchmark(
            measuredAt: ISO8601DateFormatter().string(from: Date()),
            hardware: hardwareFingerprint(),
            loadSeconds: loadSeconds,
            promptTokensPerSecond: median(prefill),
            generatedTokensPerSecond: median(decode),
            secondsPerImage: median(perImage),
            peakResidentGB: peak > 0 ? peak : nil,
            jsonValidRate: jsonChecks > 0 ? Double(jsonValid) / Double(jsonChecks) : nil,
            probeAccuracy: probeChecks > 0 ? Double(probeHits) / Double(probeChecks) : nil,
            tunedArguments: nil
        )
    }

    static func durationSeconds(_ duration: Duration) -> Double {
        let parts = duration.components
        return Double(parts.seconds) + Double(parts.attoseconds) / 1e18
    }
}

/// Last benchmark per model for this Mac, kept beside the engine's other state.
enum ModelBenchmarkStore {
    static var fileURL: URL { EngineStateRoot.directory.appendingPathComponent("model-benchmarks.json") }
    private static let lock = NSLock()

    static func all(at url: URL = fileURL) -> [String: ModelBenchmark] {
        lock.lock(); defer { lock.unlock() }
        guard let data = FileManager.default.contents(atPath: url.path),
              let decoded = try? JSONDecoder().decode([String: ModelBenchmark].self, from: data) else { return [:] }
        let hardware = ModelBenchmarkRunner.hardwareFingerprint()
        return decoded.filter { $0.value.hardware == hardware }
    }

    static func save(_ benchmark: ModelBenchmark, for kind: String, at url: URL = fileURL) {
        var current = all(at: url)
        current[kind] = benchmark
        lock.lock(); defer { lock.unlock() }
        do {
            try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            try encoder.encode(current).write(to: url, options: .atomic)
        } catch {
            JSONLog.shared.warn(ev: "benchmark_store_failed", error: "\((error as NSError).domain) \((error as NSError).code)")
        }
    }
}

/// One benchmark at a time; `cancelBenchmark` cancels it.
actor ModelBenchmarkSlot {
    static let shared = ModelBenchmarkSlot()
    private var task: Task<Void, Never>?

    func claim(_ make: () -> Task<Void, Never>) -> Bool {
        guard task == nil else { return false }
        task = make()
        return true
    }

    func finish() { task = nil }
    func cancel() { task?.cancel() }
}
