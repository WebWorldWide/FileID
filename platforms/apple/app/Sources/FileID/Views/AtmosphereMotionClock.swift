import SwiftUI

struct AtmosphereSuspension: OptionSet, Equatable {
    let rawValue: UInt8

    static let unmounted = Self(rawValue: 1 << 0)
    static let windowInactive = Self(rawValue: 1 << 1)
    static let reduceMotion = Self(rawValue: 1 << 2)
    static let userPaused = Self(rawValue: 1 << 3)
}

struct AtmosphereMotionClock {
    private(set) var suspension: AtmosphereSuspension = [.unmounted, .windowInactive]
    private var accumulated: TimeInterval = 0
    private var startedAt: TimeInterval?

    var isPaused: Bool { !suspension.isEmpty }

    func elapsed(at time: TimeInterval) -> TimeInterval {
        accumulated + (startedAt.map { max(0, time - $0) } ?? 0)
    }

    mutating func setSuspension(_ reasons: AtmosphereSuspension, at time: TimeInterval) {
        guard reasons != suspension else { return }
        if let startedAt {
            accumulated += max(0, time - startedAt)
        }
        suspension = reasons
        startedAt = reasons.isEmpty ? time : nil
    }
}

enum AtmosphereMotionScheduling {
    static func isPaused(localSuspension: AtmosphereSuspension, windowActive: Bool) -> Bool {
        !localSuspension.isEmpty || !windowActive
    }
}

private struct AtmospherePausedKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    var atmospherePaused: Bool {
        get { self[AtmospherePausedKey.self] }
        set { self[AtmospherePausedKey.self] = newValue }
    }
}

@MainActor
struct AtmosphereTimeline<Content: View>: View {
    var paused = false
    @ViewBuilder var content: (TimeInterval) -> Content
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.atmospherePaused) private var userPaused
    @State private var delivery = AtmosphereActivityDelivery()
    @State private var mounted = false
    @State private var windowActive = false

    private var suspension: AtmosphereSuspension {
        var reasons: AtmosphereSuspension = []
        if !mounted { reasons.insert(.unmounted) }
        if reduceMotion { reasons.insert(.reduceMotion) }
        if paused || userPaused { reasons.insert(.userPaused) }
        return reasons
    }

    var body: some View {
        // The wax drifts over tens of seconds; 30 fps is visually identical to
        // 120 and quarters the GPU/compositor work behind every other view.
        TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: AtmosphereMotionScheduling.isPaused(
            localSuspension: suspension, windowActive: windowActive
        ))) { _ in
            content(delivery.elapsed(at: ProcessInfo.processInfo.systemUptime))
        }
        .background {
            AtmosphereWindowActivity(delivery: delivery) { active in
                windowActive = active
            }
            .allowsHitTesting(false)
            .accessibilityHidden(true)
        }
        .onAppear {
            mounted = true
            synchronize()
        }
        .onDisappear {
            mounted = false
            synchronize()
        }
        .onChange(of: suspension) { _, _ in synchronize() }
    }

    private func synchronize() {
        delivery.setLocalSuspension(suspension, at: ProcessInfo.processInfo.systemUptime)
    }
}
