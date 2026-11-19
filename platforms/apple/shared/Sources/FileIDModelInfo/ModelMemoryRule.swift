import Foundation

/// The engine's memory admission (`util/model_memory.rs`), mirrored for the screens that show a model before the engine answers.
/// `shared/test-corpus/model-memory-admission.json` and `model-recommendation.json` pin both copies; the engine's own `listModels` verdict wins whenever it is available.
public enum ModelMemoryRule {
    public static func reserveMB(totalMB: UInt64) -> UInt64 {
        min(8192, max(2048, totalMB / 4))
    }

    public static func freeFloorMB(totalMB: UInt64) -> UInt64 {
        min(2048, max(512, totalMB / 16))
    }

    public static func fitsBudget(totalMB: UInt64, requestedMB: UInt64) -> Bool {
        requestedMB > 0 && totalMB > reserveMB(totalMB: totalMB) && requestedMB <= totalMB - reserveMB(totalMB: totalMB)
    }

    public static func fitsNow(totalMB: UInt64, availableMB: UInt64, requestedMB: UInt64) -> Bool {
        guard totalMB > 0, availableMB > 0, fitsBudget(totalMB: totalMB, requestedMB: requestedMB) else { return false }
        let floor = freeFloorMB(totalMB: totalMB)
        let available = min(availableMB, totalMB)
        return available >= floor && requestedMB <= available - floor
    }

    /// First of `ladder` (best first) that loads now; else the smallest the machine could ever run.
    public static func recommend(ladder: [(id: String, requestedMB: UInt64)], totalMB: UInt64, availableMB: UInt64) -> String? {
        ladder.first { fitsNow(totalMB: totalMB, availableMB: availableMB, requestedMB: $0.requestedMB) }?.id
            ?? ladder.filter { fitsBudget(totalMB: totalMB, requestedMB: $0.requestedMB) }.min { $0.requestedMB < $1.requestedMB }?.id
    }
}
