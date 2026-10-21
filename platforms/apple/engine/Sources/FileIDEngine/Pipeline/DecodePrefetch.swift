import Foundation
import CoreGraphics

/// Decodes the next Describe Files image on the CPU while the GPU generates for the
/// current one. Holds at most two pending decodes; older unused ones are dropped.
final class DecodePrefetch: @unchecked Sendable {
    static let shared = DecodePrefetch()
    static let waitSeconds: UInt64 = 10

    private struct Decoded: @unchecked Sendable { let image: CGImage? }

    /// Resumes once with whichever finishes first: the decode or the timeout.
    private final class Race: @unchecked Sendable {
        private let lock = NSLock()
        private var continuation: CheckedContinuation<Decoded?, Never>?
        func install(_ c: CheckedContinuation<Decoded?, Never>) { lock.lock(); continuation = c; lock.unlock() }
        func finish(_ value: Decoded?) {
            lock.lock(); let c = continuation; continuation = nil; lock.unlock()
            c?.resume(returning: value)
        }
    }
    private let lock = NSLock()
    private var slots: [(path: String, size: Int, task: Task<Decoded, Never>)] = []

    static func enabled(environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        environment["FILEID_VLM_PREFETCH"] != "0"
    }

    func start(_ url: URL, maxPixelSize: Int) {
        guard Self.enabled() else { return }
        lock.lock(); defer { lock.unlock() }
        if slots.contains(where: { $0.path == url.path && $0.size == maxPixelSize }) { return }
        while slots.count >= 2 { slots.removeFirst().task.cancel() }
        let task = Task.detached(priority: .utility) {
            Decoded(image: Task.isCancelled ? nil : await DeepAnalyze.loadCGImage(url: url, maxPixelSize: maxPixelSize))
        }
        slots.append((url.path, maxPixelSize, task))
    }

    private func claim(_ path: String, _ size: Int) -> Task<Decoded, Never>? {
        lock.withLock {
            guard let index = slots.firstIndex(where: { $0.path == path && $0.size == size }) else { return nil }
            return slots.remove(at: index).task
        }
    }

    /// The prefetched raster for `url`, or `.none` when nothing usable was prefetched
    /// (absent, a different size, or still decoding after `waitSeconds`).
    func take(_ url: URL, maxPixelSize: Int) async -> CGImage?? {
        guard let current = claim(url.path, maxPixelSize) else { return .none }
        let race = Race()
        let decoded: Decoded? = await withCheckedContinuation { continuation in
            race.install(continuation)
            Task { race.finish(await current.value) }
            Task {
                try? await Task.sleep(nanoseconds: Self.waitSeconds * 1_000_000_000)
                race.finish(nil)
            }
        }
        guard let decoded, let image = decoded.image else { return .none }
        return .some(image)
    }
}
