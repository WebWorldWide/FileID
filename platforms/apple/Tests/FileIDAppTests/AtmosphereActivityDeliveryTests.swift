import Testing
@testable import FileID

@MainActor
private final class ActivityDeliveryScheduler {
    private(set) var actions: [AtmosphereActivityDelivery.ScheduledAction] = []

    func enqueue(_ action: @escaping AtmosphereActivityDelivery.ScheduledAction) {
        actions.append(action)
    }

    func runNext() {
        let action = actions.removeFirst()
        action()
    }
}

private final class ActivityWindowIdentity {}

@MainActor
@Suite("Atmosphere deferred activity delivery")
struct AtmosphereActivityDeliveryTests {
    @Test("publication is deferred and a thousand transitions use one pending action")
    func boundedDeferredPublication() {
        let scheduler = ActivityDeliveryScheduler()
        let delivery = AtmosphereActivityDelivery(schedule: scheduler.enqueue)
        let window = ActivityWindowIdentity()
        let identity = ObjectIdentifier(window)
        var reports: [AtmosphereActivityDelivery.Publication] = []
        delivery.setLocalSuspension([], at: 0)
        let generation = delivery.bind(to: identity, at: 0) { reports.append($0) }
        for index in 1...1_000 {
            delivery.record(index % 2 == 1, for: identity, generation: generation, at: Double(index))
        }
        #expect(reports.isEmpty)
        #expect(scheduler.actions.count == 1)
        #expect(delivery.isPaused)
        #expect(delivery.elapsed(at: 10_000) == 500)
        scheduler.runNext()
        #expect(reports.count == 1)
        #expect(reports.first?.isActive == false)
        #expect(reports.first?.time == 1_000)
        #expect(scheduler.actions.isEmpty)
    }

    @Test("a queued action publishes only the newest window binding")
    func latestWindowIdentity() {
        let scheduler = ActivityDeliveryScheduler()
        let delivery = AtmosphereActivityDelivery(schedule: scheduler.enqueue)
        let a = ActivityWindowIdentity(), b = ActivityWindowIdentity()
        let firstIdentity = ObjectIdentifier(a), secondIdentity = ObjectIdentifier(b)
        var firstReports = 0
        var secondReports: [AtmosphereActivityDelivery.Publication] = []
        delivery.setLocalSuspension([], at: 0)
        let first = delivery.bind(to: firstIdentity, at: 0) { _ in firstReports += 1 }
        delivery.record(true, for: firstIdentity, generation: first, at: 1)
        let second = delivery.bind(to: secondIdentity, at: 3) { secondReports.append($0) }
        delivery.record(true, for: secondIdentity, generation: second, at: 7)
        delivery.record(false, for: firstIdentity, generation: first, at: 8)
        delivery.invalidate(generation: first, at: 9)
        #expect(scheduler.actions.count == 1)
        #expect(delivery.elapsed(at: 10) == 5)
        scheduler.runNext()
        #expect(firstReports == 0)
        #expect(secondReports.count == 1)
        #expect(secondReports.first?.windowIdentity == secondIdentity)
        #expect(secondReports.first?.generation == second)
        #expect(secondReports.first?.isActive == true)
    }

    @Test("same-window rebinding rejects stale records and stale teardown")
    func sameWindowNewGeneration() {
        let scheduler = ActivityDeliveryScheduler()
        let delivery = AtmosphereActivityDelivery(schedule: scheduler.enqueue)
        let window = ActivityWindowIdentity()
        let identity = ObjectIdentifier(window)
        var oldReports = 0
        var newReports = 0
        delivery.setLocalSuspension([], at: 0)
        let old = delivery.bind(to: identity, at: 0) { _ in oldReports += 1 }
        delivery.record(true, for: identity, generation: old, at: 1)
        delivery.invalidate(generation: old, at: 3)
        let new = delivery.bind(to: identity, at: 10) { _ in newReports += 1 }
        delivery.record(true, for: identity, generation: new, at: 11)
        delivery.record(false, for: identity, generation: old, at: 12)
        delivery.invalidate(generation: old, at: 13)
        #expect(!delivery.isPaused)
        #expect(delivery.elapsed(at: 14) == 5)
        #expect(scheduler.actions.count == 1)
        scheduler.runNext()
        #expect(oldReports == 0)
        #expect(newReports == 1)
    }

    @Test("teardown suppresses pending publication and freezes phase at teardown time")
    func teardownSuppression() {
        let scheduler = ActivityDeliveryScheduler()
        let delivery = AtmosphereActivityDelivery(schedule: scheduler.enqueue)
        let window = ActivityWindowIdentity()
        let identity = ObjectIdentifier(window)
        var reports = 0
        delivery.setLocalSuspension([], at: 0)
        let generation = delivery.bind(to: identity, at: 0) { _ in reports += 1 }
        delivery.record(true, for: identity, generation: generation, at: 1)
        delivery.invalidate(generation: generation, at: 4)
        delivery.record(true, for: identity, generation: generation, at: 5)
        #expect(reports == 0)
        #expect(delivery.elapsed(at: 100) == 3)
        scheduler.runNext()
        #expect(reports == 0)
        #expect(scheduler.actions.isEmpty)
    }

    @Test("repeated notifications do not enqueue or alter transition time")
    func repeatedNotifications() {
        let scheduler = ActivityDeliveryScheduler()
        let delivery = AtmosphereActivityDelivery(schedule: scheduler.enqueue)
        let window = ActivityWindowIdentity()
        let identity = ObjectIdentifier(window)
        var reports: [AtmosphereActivityDelivery.Publication] = []
        delivery.setLocalSuspension([], at: 0)
        let generation = delivery.bind(to: identity, at: 0) { reports.append($0) }
        delivery.record(true, for: identity, generation: generation, at: 1)
        delivery.record(true, for: identity, generation: generation, at: 2)
        scheduler.runNext()
        #expect(reports.first?.time == 1)
        delivery.record(true, for: identity, generation: generation, at: 3)
        #expect(scheduler.actions.isEmpty)
        delivery.record(false, for: identity, generation: generation, at: 4)
        delivery.record(false, for: identity, generation: generation, at: 5)
        #expect(scheduler.actions.count == 1)
        #expect(delivery.elapsed(at: 100) == 3)
        scheduler.runNext()
        #expect(reports.count == 2)
        #expect(reports.last?.time == 4)
    }

    @Test("pending work retains neither delivery nor the identity object")
    func weakQueuedLifetime() {
        let scheduler = ActivityDeliveryScheduler()
        var delivery: AtmosphereActivityDelivery? = AtmosphereActivityDelivery(schedule: scheduler.enqueue)
        weak var weakDelivery = delivery
        var window: ActivityWindowIdentity? = ActivityWindowIdentity()
        weak var weakWindow = window
        var reports = 0
        if let window, let delivery {
            delivery.bind(to: ObjectIdentifier(window), at: 0) { _ in reports += 1 }
        }
        window = nil
        #expect(weakWindow == nil)
        delivery = nil
        #expect(weakDelivery == nil)
        #expect(scheduler.actions.count == 1)
        scheduler.runNext()
        #expect(reports == 0)
    }

    @Test("independent deliveries cannot change another window's phase")
    func independentWindows() {
        let firstScheduler = ActivityDeliveryScheduler(), secondScheduler = ActivityDeliveryScheduler()
        let first = AtmosphereActivityDelivery(schedule: firstScheduler.enqueue)
        let second = AtmosphereActivityDelivery(schedule: secondScheduler.enqueue)
        let a = ActivityWindowIdentity(), b = ActivityWindowIdentity()
        var firstReports = 0, secondReports = 0
        first.setLocalSuspension([], at: 0)
        second.setLocalSuspension([], at: 0)
        let firstGeneration = first.bind(to: ObjectIdentifier(a), at: 0) { _ in firstReports += 1 }
        let secondGeneration = second.bind(to: ObjectIdentifier(b), at: 0) { _ in secondReports += 1 }
        first.record(true, for: ObjectIdentifier(a), generation: firstGeneration, at: 1)
        second.record(true, for: ObjectIdentifier(b), generation: secondGeneration, at: 2)
        first.invalidate(generation: firstGeneration, at: 4)
        firstScheduler.runNext()
        secondScheduler.runNext()
        #expect(firstReports == 0)
        #expect(secondReports == 1)
        #expect(first.elapsed(at: 10) == 3)
        #expect(second.elapsed(at: 10) == 8)
    }

    @Test("published-active live detachment defers an inactive scheduling snapshot")
    func publishedActiveLiveDetach() {
        let scheduler = ActivityDeliveryScheduler()
        let delivery = AtmosphereActivityDelivery(schedule: scheduler.enqueue)
        let window = ActivityWindowIdentity()
        let identity = ObjectIdentifier(window)
        var windowActive = false
        var reports: [AtmosphereActivityDelivery.Publication] = []
        delivery.setLocalSuspension([], at: 0)
        let bound = delivery.bind(to: identity, at: 0) {
            reports.append($0)
            windowActive = $0.isActive
        }
        delivery.record(true, for: identity, generation: bound, at: 1)
        scheduler.runNext()
        #expect(windowActive)
        #expect(!AtmosphereMotionScheduling.isPaused(localSuspension: [], windowActive: windowActive))

        let detached = delivery.detach(generation: bound, at: 4)
        #expect(detached != nil)
        #expect(windowActive)
        #expect(reports.count == 1)
        #expect(scheduler.actions.count == 1)
        #expect(delivery.elapsed(at: 100) == 3)
        if let detached {
            delivery.record(true, for: identity, generation: detached, at: 5)
        }
        delivery.record(true, for: identity, generation: bound, at: 6)
        #expect(delivery.isPaused)
        scheduler.runNext()
        #expect(!windowActive)
        #expect(AtmosphereMotionScheduling.isPaused(localSuspension: [], windowActive: windowActive))
        #expect(reports.count == 2)
        #expect(reports.last?.isDetached == true)
        #expect(reports.last?.windowIdentity == identity)
        #expect(reports.last?.generation == detached)
        #expect(reports.last?.time == 4)
        #expect(delivery.elapsed(at: 100) == 3)
        #expect(scheduler.actions.isEmpty)
    }

    @Test("terminal teardown after published-active live detach suppresses its pending snapshot")
    func publishedActiveTerminalDismantle() {
        let scheduler = ActivityDeliveryScheduler()
        let delivery = AtmosphereActivityDelivery(schedule: scheduler.enqueue)
        let window = ActivityWindowIdentity()
        let identity = ObjectIdentifier(window)
        var reports: [AtmosphereActivityDelivery.Publication] = []
        delivery.setLocalSuspension([], at: 0)
        let bound = delivery.bind(to: identity, at: 0) { reports.append($0) }
        delivery.record(true, for: identity, generation: bound, at: 1)
        scheduler.runNext()
        #expect(reports.last?.isActive == true)
        let detached = delivery.detach(generation: bound, at: 4)
        if let detached {
            delivery.invalidate(generation: detached, at: 5)
            delivery.record(true, for: identity, generation: detached, at: 6)
        }
        #expect(detached != nil)
        #expect(scheduler.actions.count == 1)
        scheduler.runNext()
        #expect(reports.count == 1)
        #expect(delivery.isPaused)
        #expect(delivery.elapsed(at: 100) == 3)
        #expect(scheduler.actions.isEmpty)
    }

    @Test("new binding suppresses the old published-active window's detached snapshot")
    func publishedActiveDetachRebind() {
        let scheduler = ActivityDeliveryScheduler()
        let delivery = AtmosphereActivityDelivery(schedule: scheduler.enqueue)
        let a = ActivityWindowIdentity(), b = ActivityWindowIdentity()
        let firstIdentity = ObjectIdentifier(a), secondIdentity = ObjectIdentifier(b)
        var firstReports: [AtmosphereActivityDelivery.Publication] = []
        var secondReports: [AtmosphereActivityDelivery.Publication] = []
        var windowActive = false
        delivery.setLocalSuspension([], at: 0)
        let first = delivery.bind(to: firstIdentity, at: 0) {
            firstReports.append($0)
            windowActive = $0.isActive
        }
        delivery.record(true, for: firstIdentity, generation: first, at: 1)
        scheduler.runNext()
        #expect(windowActive)
        let detached = delivery.detach(generation: first, at: 4)
        let second = delivery.bind(to: secondIdentity, at: 10) {
            secondReports.append($0)
            windowActive = $0.isActive
        }
        delivery.record(true, for: secondIdentity, generation: second, at: 11)
        if let detached {
            delivery.invalidate(generation: detached, at: 12)
        }
        #expect(detached != nil)
        #expect(scheduler.actions.count == 1)
        scheduler.runNext()
        #expect(firstReports.count == 1)
        #expect(secondReports.count == 1)
        #expect(secondReports.last?.isDetached == false)
        #expect(secondReports.last?.windowIdentity == secondIdentity)
        #expect(secondReports.last?.generation == second)
        #expect(windowActive)
        #expect(!AtmosphereMotionScheduling.isPaused(localSuspension: [], windowActive: windowActive))
        #expect(delivery.elapsed(at: 12) == 4)
        #expect(scheduler.actions.isEmpty)
    }
}
