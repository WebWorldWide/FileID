import Foundation
import Darwin
import MLX
import MLXLMCommon

/// Per-call VLM stage timings, written to the local JSONL log only.
final class VLMPassStats: @unchecked Sendable {
    private let lock = NSLock()
    private var values: [String: Double] = [:]

    func record(_ pass: String, _ info: GenerateCompletionInfo) {
        lock.lock(); defer { lock.unlock() }
        values[pass + "PromptTokens", default: 0] += Double(info.promptTokenCount)
        values[pass + "PromptMs", default: 0] += info.promptTime * 1000
        values[pass + "GenTokens", default: 0] += Double(info.generationTokenCount)
        values[pass + "GenMs", default: 0] += info.generateTime * 1000
    }

    func set(_ key: String, _ value: Double) {
        lock.lock(); values[key] = value; lock.unlock()
    }

    func snapshot() -> [String: Double] {
        lock.lock(); defer { lock.unlock() }; return values
    }
}

enum VLMTiming {
    static func loadAverage() -> Double {
        var load = [Double](repeating: 0, count: 3)
        return getloadavg(&load, 3) > 0 ? load[0] : -1
    }

    static func milliseconds(since start: ContinuousClock.Instant) -> Double {
        let elapsed = ContinuousClock.now - start
        return Double(elapsed.components.seconds) * 1000 + Double(elapsed.components.attoseconds) / 1e15
    }

    static func log(_ event: String, stats: VLMPassStats, totalMs: Double) {
        var extra: [String: AnyCodable] = [:]
        for (key, value) in stats.snapshot() { extra[key] = AnyCodable((value * 10).rounded() / 10) }
        extra["totalMs"] = AnyCodable((totalMs * 10).rounded() / 10)
        extra["load1"] = AnyCodable(loadAverage())
        extra["gpuActiveMB"] = AnyCodable(Memory.activeMemory / 1_048_576)
        extra["gpuPeakMB"] = AnyCodable(Memory.peakMemory / 1_048_576)
        JSONLog.shared.info(ev: event, extra: extra)
    }
}
