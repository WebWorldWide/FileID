// SleepGuard — prevents the system from sleeping during long-running engine
// jobs (scan, Deep Analyze, clustering). Lets the user close the laptop lid
// and walk away while a multi-hour scan or Deep Analyze runs.
//
// Caveats (Apple Silicon hardware-level constraints — we cannot work around):
//  - On battery, lid-closed always sleeps. Power MUST be connected for
//    lid-closed scans to keep running. The IOPMAssertion below tells the
//    OS to ignore idle-sleep timers, but a MacBook on battery without
//    external display also forcibly sleeps when the lid closes — Apple
//    hardware-level decision, not software.
//  - With AC power and `kIOPMAssertPreventSystemSleep`, the system stays
//    awake even with the lid closed. This is what we want for overnight
//    Deep Analyze.
//
// The assertion is reference-counted: every begin() call must be matched
// by an end(). We use a single named assertion so multiple jobs running
// simultaneously coexist without tripping over each other.
import Foundation
import IOKit
import IOKit.pwr_mgt

public final class SleepGuard: @unchecked Sendable {
    public static let shared = SleepGuard()

    private let lock = NSLock()
    private var refcount: Int = 0
    private var assertionID: IOPMAssertionID = 0

    private init() {}

    /// Acquire the no-sleep assertion. Idempotent — multiple begin() calls
    /// hold one assertion, refcounted.
    public func begin(reason: String) {
        lock.lock(); defer { lock.unlock() }
        refcount += 1
        guard refcount == 1 else { return }
        // PreventSystemSleep keeps the system awake even on battery, even
        // with lid closed (subject to the Apple Silicon hardware caveat).
        // PreventUserIdleDisplaySleep is intentionally NOT set — display
        // can still dim/sleep while the engine works in the background.
        // Constant name comes through as a String literal in Swift's import
        // of IOKit/IOPMLib.h since the underlying #define is a C string.
        let kind = "PreventSystemSleep" as CFString
        let result = IOPMAssertionCreateWithName(
            kind,
            IOPMAssertionLevel(kIOPMAssertionLevelOn),
            "FileID: \(reason)" as CFString,
            &assertionID
        )
        if result != kIOReturnSuccess {
            // Non-fatal — sleep prevention is best-effort.
            JSONLog.shared.warn(ev: "sleep_assertion_failed",
                                error: "IOPMAssertionCreateWithName returned \(result)")
            assertionID = 0
        } else {
            JSONLog.shared.info(ev: "sleep_assertion_begin",
                                extra: ["reason": AnyCodable(reason)])
        }
    }

    /// Release one ref of the no-sleep assertion. When refcount hits 0 the
    /// underlying assertion is released and the system can sleep again.
    public func end() {
        lock.lock(); defer { lock.unlock() }
        guard refcount > 0 else { return }
        refcount -= 1
        guard refcount == 0, assertionID != 0 else { return }
        IOPMAssertionRelease(assertionID)
        assertionID = 0
        JSONLog.shared.info(ev: "sleep_assertion_end")
    }

    /// Convenience: scope an async block under the assertion.
    public func withGuard<T>(reason: String, _ body: () async throws -> T) async rethrows -> T {
        begin(reason: reason)
        defer { end() }
        return try await body()
    }
}
