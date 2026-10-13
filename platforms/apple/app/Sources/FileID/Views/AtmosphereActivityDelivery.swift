import Foundation
import Dispatch

// Non-observable accounting keeps native timestamps; only SwiftUI publication is deferred.
@MainActor
final class AtmosphereActivityDelivery {
    struct Publication {
        let windowIdentity: ObjectIdentifier
        let generation: UInt64
        let isActive: Bool
        let isDetached: Bool
        let time: TimeInterval
    }

    typealias ScheduledAction = @MainActor @Sendable () -> Void
    typealias Scheduler = @MainActor (@escaping ScheduledAction) -> Void

    private let schedule: Scheduler
    private var clock = AtmosphereMotionClock()
    private var localSuspension: AtmosphereSuspension = .unmounted
    private var windowIdentity: ObjectIdentifier?
    private var generation: UInt64 = 0
    private var active = false
    private var detached = false
    private var scheduled = false
    private var pending: Publication?
    private var onPublish: (@MainActor (Publication) -> Void)?

    init(schedule: @escaping Scheduler = { action in
        DispatchQueue.main.async { action() }
    }) {
        self.schedule = schedule
    }

    var isPaused: Bool { clock.isPaused }

    func elapsed(at time: TimeInterval) -> TimeInterval {
        clock.elapsed(at: time)
    }

    func setLocalSuspension(_ reasons: AtmosphereSuspension, at time: TimeInterval) {
        localSuspension = reasons.subtracting(.windowInactive)
        synchronize(at: time)
    }

    @discardableResult
    func bind(to identity: ObjectIdentifier, at time: TimeInterval,
              onPublish: @escaping @MainActor (Publication) -> Void) -> UInt64 {
        resetBinding(at: time)
        windowIdentity = identity
        self.onPublish = onPublish
        enqueue(at: time)
        return generation
    }

    func detach(generation expected: UInt64, at time: TimeInterval) -> UInt64? {
        guard expected == generation, windowIdentity != nil, !detached else { return nil }
        generation &+= 1
        pending = nil
        detached = true
        active = false
        synchronize(at: time)
        enqueue(at: time)
        return generation
    }

    func invalidate(generation expected: UInt64, at time: TimeInterval) {
        guard expected == generation else { return }
        resetBinding(at: time)
    }

    private func resetBinding(at time: TimeInterval) {
        generation &+= 1
        windowIdentity = nil
        pending = nil
        onPublish = nil
        active = false
        detached = false
        synchronize(at: time)
    }

    func record(_ isActive: Bool, for identity: ObjectIdentifier,
                generation expected: UInt64, at time: TimeInterval) {
        guard expected == generation, windowIdentity == identity, !detached,
              active != isActive else { return }
        active = isActive
        // Account every transition now; only publication is coalesced, not phase history.
        synchronize(at: time)
        enqueue(at: time)
    }

    private func synchronize(at time: TimeInterval) {
        var reasons = localSuspension
        if !active { reasons.insert(.windowInactive) }
        clock.setSuspension(reasons, at: time)
    }

    private func enqueue(at time: TimeInterval) {
        guard let windowIdentity else { return }
        pending = Publication(windowIdentity: windowIdentity, generation: generation,
                              isActive: active, isDetached: detached, time: time)
        guard !scheduled else { return }
        scheduled = true
        schedule { [weak self] in self?.deliverPending() }
    }

    private func deliverPending() {
        scheduled = false
        guard let publication = pending else { return }
        pending = nil
        guard publication.generation == generation,
              publication.windowIdentity == windowIdentity,
              publication.isDetached == detached else { return }
        onPublish?(publication)
    }
}
