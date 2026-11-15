import Foundation

/// Frees the MLX scan models (CLIP, RAM++) after a quiet period and when a
/// description model is about to load, as the Rust engine's `AdapterModels`
/// does. A scan holds them for its whole run, including the after-scan tagging
/// pass; `touch()` marks use outside a scan. The models reload on next use.
package final class ScanModelResidency: @unchecked Sendable {
    package static let idleSeconds: TimeInterval = 120
    package static let shared = ScanModelResidency()

    private let lock = NSLock()
    private let idle: TimeInterval
    private let now: @Sendable () -> Date
    private let schedule: @Sendable (TimeInterval, @escaping @Sendable () -> Void) -> Void
    private var unloaders: [@Sendable () -> Void] = []
    private var holds = 0
    private var lastUse: Date
    private var resident = false
    private var sweepScheduled = false
    private var unloads = 0

    private static let timers = DispatchQueue(label: "com.fileid.scan-model-residency", qos: .utility)

    package init(idle: TimeInterval = ScanModelResidency.idleSeconds,
                 now: @escaping @Sendable () -> Date = { Date() },
                 schedule: @escaping @Sendable (TimeInterval, @escaping @Sendable () -> Void) -> Void = { delay, work in
                     ScanModelResidency.timers.asyncAfter(deadline: .now() + delay, execute: work)
                 }) {
        self.idle = idle
        self.now = now
        self.schedule = schedule
        lastUse = now()
    }

    /// Registers what to free; called once per model service.
    package func register(_ unload: @escaping @Sendable () -> Void) {
        lock.lock(); defer { lock.unlock() }
        unloaders.append(unload)
    }

    /// Keeps the models loaded until `end()`.
    package func begin() {
        lock.lock()
        holds += 1
        resident = true
        lastUse = now()
        lock.unlock()
    }

    package func end() {
        lock.lock()
        holds = max(0, holds - 1)
        lastUse = now()
        scheduleSweepLocked()
        lock.unlock()
    }

    /// A model was used or loaded: restart the idle period.
    package func touch() {
        lock.lock()
        resident = true
        lastUse = now()
        scheduleSweepLocked()
        lock.unlock()
    }

    package var holders: Int { lock.lock(); defer { lock.unlock() }; return holds }
    package var unloadCount: Int { lock.lock(); defer { lock.unlock() }; return unloads }

    /// Frees the models unless a scan holds them or they were used within the
    /// idle period. Returns whether it freed them.
    @discardableResult
    package func sweep() -> Bool {
        lock.lock()
        sweepScheduled = false
        guard resident, holds == 0 else { lock.unlock(); return false }
        if now().timeIntervalSince(lastUse) < idle {
            scheduleSweepLocked()
            lock.unlock()
            return false
        }
        return freeLocked()
    }

    /// A description model is about to load: free the scan models first,
    /// unless a scan is running.
    @discardableResult
    package func releaseForOtherModel() -> Bool {
        lock.lock()
        guard resident, holds == 0 else { lock.unlock(); return false }
        return freeLocked()
    }

    private func freeLocked() -> Bool {
        resident = false
        unloads += 1
        let work = unloaders
        lock.unlock()
        for unload in work { unload() }
        return true
    }

    private func scheduleSweepLocked() {
        guard !sweepScheduled, holds == 0 else { return }
        sweepScheduled = true
        let wait = max(0.001, idle - now().timeIntervalSince(lastUse))
        schedule(wait) { [weak self] in _ = self?.sweep() }
    }
}
