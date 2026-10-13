import Testing
@testable import FileID

@Suite("Atmosphere motion lifecycle")
struct MotionLifecycleTests {
    @Test("unattached atmosphere starts at a settled frame")
    func initiallySuspended() {
        let clock = AtmosphereMotionClock()
        #expect(clock.isPaused)
        #expect(clock.elapsed(at: 50_000) == 0)
    }

    @Test("user pause freezes phase and resumes without a wall-clock jump")
    func pauseResume() {
        var clock = AtmosphereMotionClock()
        clock.setSuspension([], at: 100)
        #expect(clock.elapsed(at: 104) == 4)
        clock.setSuspension(.userPaused, at: 104)
        #expect(clock.elapsed(at: 10_000) == 4)
        clock.setSuspension([], at: 10_000)
        #expect(clock.elapsed(at: 10_000) == 4)
        #expect(clock.elapsed(at: 10_002) == 6)
    }

    @Test("resume cannot override live reduced motion or window inactivity")
    func overlappingReasons() {
        var clock = AtmosphereMotionClock()
        clock.setSuspension([], at: 10)
        clock.setSuspension(.userPaused, at: 13)
        clock.setSuspension([.userPaused, .reduceMotion, .windowInactive], at: 14)
        clock.setSuspension([.reduceMotion, .windowInactive], at: 20)
        #expect(clock.isPaused)
        #expect(clock.elapsed(at: 30) == 3)
        clock.setSuspension(.windowInactive, at: 30)
        #expect(clock.isPaused)
        clock.setSuspension([], at: 40)
        #expect(clock.elapsed(at: 41) == 4)
    }

    @Test("repeated notifications neither reset nor double-count elapsed time")
    func repeatedNotifications() {
        var clock = AtmosphereMotionClock()
        clock.setSuspension([], at: 10)
        clock.setSuspension([], at: 12)
        #expect(clock.elapsed(at: 13) == 3)
        clock.setSuspension(.windowInactive, at: 13)
        clock.setSuspension(.windowInactive, at: 20)
        #expect(clock.elapsed(at: 100) == 3)
        clock.setSuspension([], at: 100)
        clock.setSuspension([], at: 101)
        #expect(clock.elapsed(at: 102) == 5)
    }

    @Test("unmount and window rebind retain phase while detached")
    func reattachment() {
        var clock = AtmosphereMotionClock()
        clock.setSuspension([], at: 1)
        clock.setSuspension(.windowInactive, at: 4)
        clock.setSuspension([.windowInactive, .unmounted], at: 5)
        clock.setSuspension(.windowInactive, at: 100)
        #expect(clock.elapsed(at: 101) == 3)
        clock.setSuspension([], at: 102)
        #expect(clock.elapsed(at: 103) == 4)
    }

    @Test("rapid preference and pause changes only accumulate active intervals")
    func rapidToggles() {
        var clock = AtmosphereMotionClock()
        for index in 0..<100 {
            let time = Double(index * 10)
            clock.setSuspension([], at: time)
            clock.setSuspension(.reduceMotion, at: time + 1)
            clock.setSuspension([.reduceMotion, .userPaused], at: time + 2)
            clock.setSuspension(.userPaused, at: time + 3)
        }
        #expect(clock.isPaused)
        #expect(clock.elapsed(at: 10_000) == 100)
    }

    @MainActor
    @Test("deferred false-to-true eligibility does not count its inactive interval")
    func deferredEligibilityPhase() {
        var actions: [AtmosphereActivityDelivery.ScheduledAction] = []
        let delivery = AtmosphereActivityDelivery(schedule: { actions.append($0) })
        let window = MotionWindowIdentity()
        let identity = ObjectIdentifier(window)
        var reports = 0
        delivery.setLocalSuspension([], at: 0)
        let generation = delivery.bind(to: identity, at: 0) { _ in reports += 1 }
        delivery.record(true, for: identity, generation: generation, at: 1)
        actions.removeFirst()()
        delivery.record(false, for: identity, generation: generation, at: 4)
        delivery.record(true, for: identity, generation: generation, at: 100)
        #expect(actions.count == 1)
        #expect(reports == 1)
        #expect(delivery.elapsed(at: 100) == 3)
        actions.removeFirst()()
        #expect(reports == 2)
        #expect(delivery.elapsed(at: 102) == 5)
    }

    @MainActor
    @Test("local suspension overlaps are accounted before deferred window publication")
    func deferredEligibilityWithLocalSuspension() {
        var actions: [AtmosphereActivityDelivery.ScheduledAction] = []
        let delivery = AtmosphereActivityDelivery(schedule: { actions.append($0) })
        let window = MotionWindowIdentity()
        let identity = ObjectIdentifier(window)
        var reports = 0
        delivery.setLocalSuspension([], at: 0)
        let generation = delivery.bind(to: identity, at: 0) { _ in reports += 1 }
        delivery.record(true, for: identity, generation: generation, at: 1)
        delivery.record(false, for: identity, generation: generation, at: 3)
        delivery.setLocalSuspension(.userPaused, at: 4)
        delivery.record(true, for: identity, generation: generation, at: 5)
        delivery.setLocalSuspension([.userPaused, .reduceMotion], at: 6)
        delivery.setLocalSuspension(.reduceMotion, at: 7)
        #expect(delivery.isPaused)
        #expect(delivery.elapsed(at: 8) == 2)
        delivery.setLocalSuspension(.unmounted, at: 8)
        delivery.setLocalSuspension([], at: 9)
        #expect(reports == 0)
        #expect(actions.count == 1)
        actions.removeFirst()()
        #expect(reports == 1)
        #expect(delivery.elapsed(at: 10) == 3)
    }

    @MainActor
    @Test("clearing the final user pause rebuilds an active schedule before clock synchronization")
    func finalUserPauseClearResumesSchedule() {
        verifyFinalLocalResume(.userPaused)
    }

    @MainActor
    @Test("clearing the final Reduce Motion reason rebuilds an active schedule before clock synchronization")
    func finalReduceMotionClearResumesSchedule() {
        verifyFinalLocalResume(.reduceMotion)
    }

    @MainActor
    private func verifyFinalLocalResume(_ finalReason: AtmosphereSuspension) {
        var actions: [AtmosphereActivityDelivery.ScheduledAction] = []
        let delivery = AtmosphereActivityDelivery(schedule: { actions.append($0) })
        let window = MotionWindowIdentity()
        let identity = ObjectIdentifier(window)
        var windowActive = false
        delivery.setLocalSuspension([], at: 0)
        let generation = delivery.bind(to: identity, at: 0) { windowActive = $0.isActive }
        delivery.record(true, for: identity, generation: generation, at: 1)
        actions.removeFirst()()
        #expect(!AtmosphereMotionScheduling.isPaused(localSuspension: [], windowActive: windowActive))

        delivery.setLocalSuspension([.userPaused, .reduceMotion], at: 3)
        delivery.setLocalSuspension(finalReason, at: 4)
        #expect(AtmosphereMotionScheduling.isPaused(localSuspension: finalReason, windowActive: windowActive))
        #expect(delivery.isPaused)
        #expect(actions.isEmpty)

        // This is the body evaluation before onChange updates the non-observable clock.
        #expect(!AtmosphereMotionScheduling.isPaused(localSuspension: [], windowActive: windowActive))
        #expect(delivery.isPaused)
        #expect(delivery.elapsed(at: 100) == 2)
        delivery.setLocalSuspension([], at: 100)
        #expect(delivery.elapsed(at: 100) == 2)
        #expect(delivery.elapsed(at: 102) == 4)
        #expect(actions.isEmpty)
    }
}

private final class MotionWindowIdentity {}
