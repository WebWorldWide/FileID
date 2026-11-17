import Foundation
import FileIDImaging
import FileIDShared

/// Tells the app, once per engine run, that the accurate text tier stopped after repeated failures
/// (the fast read and Describe Files still work). Kind `ocr_accurate_disabled`.
final class AccurateTextNotice: @unchecked Sendable {
    static let notice = EngineError(
        kind: "ocr_accurate_disabled",
        message: "FileID's careful text reading stopped working after repeated failures, so it is using the faster read for now. Scans still finish; text in some photos may be missed. Describe Files can read what it misses.")

    private let lock = NSLock()
    private var sent = false

    func install(on breaker: AccurateOCRBreaker, emit: @escaping @Sendable (EngineError) -> Void) {
        breaker.onTrip { [self] in
            guard claim() else { return }
            emit(Self.notice)
        }
    }

    private func claim() -> Bool {
        lock.lock(); defer { lock.unlock() }
        if sent { return false }
        sent = true
        return true
    }
}
