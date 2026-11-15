import Foundation
import Testing
import FileIDImaging

private final class Clock: @unchecked Sendable {
    private let lock = NSLock()
    private var t = Date(timeIntervalSince1970: 1_000)
    private var scheduled: [() -> Void] = []
    var date: Date { lock.lock(); defer { lock.unlock() }; return t }
    func advance(_ s: TimeInterval) { lock.lock(); t = t.addingTimeInterval(s); lock.unlock() }
    func defer_(_ work: @escaping () -> Void) { lock.lock(); scheduled.append(work); lock.unlock() }
    var pending: Int { lock.lock(); defer { lock.unlock() }; return scheduled.count }
    func fire() {
        lock.lock(); let work = scheduled; scheduled = []; lock.unlock()
        for w in work { w() }
    }
}

private final class Freed: @unchecked Sendable {
    private let lock = NSLock()
    private var n = 0
    func note() { lock.lock(); n += 1; lock.unlock() }
    var count: Int { lock.lock(); defer { lock.unlock() }; return n }
}

@Suite("Scan model residency")
struct ScanModelResidencyTests {
    private func make(_ clock: Clock, _ freed: Freed) -> ScanModelResidency {
        let r = ScanModelResidency(idle: 120, now: { clock.date }, schedule: { _, work in clock.defer_(work) })
        r.register { freed.note() }
        return r
    }

    @Test func freesAfterTheIdlePeriodAndNotBefore() {
        let clock = Clock(), freed = Freed()
        let r = make(clock, freed)
        r.touch()
        clock.advance(119); clock.fire()
        #expect(freed.count == 0)
        clock.advance(2); clock.fire()
        #expect(freed.count == 1)
        #expect(clock.pending == 0)
    }

    @Test func useRestartsTheIdlePeriod() {
        let clock = Clock(), freed = Freed()
        let r = make(clock, freed)
        r.touch()
        clock.advance(100)
        r.touch()
        clock.advance(100); clock.fire()
        #expect(freed.count == 0)
        clock.advance(30); clock.fire()
        #expect(freed.count == 1)
    }

    @Test func aRunningScanKeepsTheModelsAndTheIdlePeriodStartsAtItsEnd() {
        let clock = Clock(), freed = Freed()
        let r = make(clock, freed)
        r.begin()
        clock.advance(1_000); clock.fire()
        #expect(r.sweep() == false)
        #expect(!r.releaseForOtherModel())
        #expect(freed.count == 0)
        r.end()
        clock.advance(100); clock.fire()
        #expect(freed.count == 0)
        clock.advance(21); clock.fire()
        #expect(freed.count == 1)
    }

    @Test func nestedHoldsNeedEveryEnd() {
        let clock = Clock(), freed = Freed()
        let r = make(clock, freed)
        r.begin(); r.begin()
        r.end()
        #expect(r.holders == 1)
        clock.advance(500); clock.fire()
        #expect(freed.count == 0)
        r.end()
        clock.advance(121); clock.fire()
        #expect(freed.count == 1)
    }

    @Test func aDescriptionModelLoadFreesIdleModelsOnceAndReloadStartsFresh() {
        let clock = Clock(), freed = Freed()
        let r = make(clock, freed)
        #expect(!r.releaseForOtherModel())
        r.touch()
        #expect(r.releaseForOtherModel())
        #expect(!r.releaseForOtherModel())
        #expect(freed.count == 1)
        r.touch()
        clock.advance(121); clock.fire()
        #expect(freed.count == 2)
        #expect(r.unloadCount == 2)
    }

    @Test func aSweepWithNothingLoadedDoesNothing() {
        let clock = Clock(), freed = Freed()
        let r = make(clock, freed)
        #expect(!r.sweep())
        #expect(freed.count == 0)
    }
}
