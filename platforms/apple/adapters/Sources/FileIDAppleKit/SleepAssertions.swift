import Foundation
import IOKit.pwr_mgt

/// IOPM PreventSystemSleep assertions the Rust engine holds while it works.
/// Each begin is its own assertion; the display may still sleep. On Apple
/// silicon, lid-closed runs on battery still sleep (hardware rule).
final class SleepAssertions: @unchecked Sendable {
    static let shared = SleepAssertions()

    private let lock = NSLock()
    private var held: Set<IOPMAssertionID> = []

    func begin(reason: String) -> UInt32 {
        var id: IOPMAssertionID = 0
        let result = IOPMAssertionCreateWithName(
            "PreventSystemSleep" as CFString,
            IOPMAssertionLevel(kIOPMAssertionLevelOn),
            reason as CFString,
            &id
        )
        guard result == kIOReturnSuccess, id != 0 else { return 0 }
        lock.withLock { _ = held.insert(id) }
        return id
    }

    func end(_ id: UInt32) {
        let wasHeld = lock.withLock { held.remove(id) != nil }
        if wasHeld { IOPMAssertionRelease(id) }
    }

    func releaseAll() {
        let all = lock.withLock { () -> Set<IOPMAssertionID> in
            defer { held.removeAll() }
            return held
        }
        all.forEach { IOPMAssertionRelease($0) }
    }

    var count: Int { lock.withLock { held.count } }
}
