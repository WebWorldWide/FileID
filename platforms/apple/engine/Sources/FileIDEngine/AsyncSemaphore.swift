// Async-friendly counting semaphore. Awaitable — never blocks a thread.
//
// Used to bound concurrent ANE access:
//  - 3 in-flight Vision requests (face + classify + saliency bundle)
//  - 2 in-flight CLIP embeds (separate ANE budget so Vision and CLIP don't
//    compete; v1 lesson: flooding ANE with 14 concurrent requests collapses
//    throughput).
//
// Implementation: actor-protected counter + waiter queue. Continuations are
// resumed in arrival order on signal.
import Foundation

public actor AsyncSemaphore {
    private var available: Int
    private var waiters: [CheckedContinuation<Void, Never>] = []

    public init(value: Int) {
        precondition(value >= 0, "AsyncSemaphore: initial value must be non-negative")
        self.available = value
    }

    /// Acquire one permit. Suspends if no permits available.
    public func wait() async {
        if available > 0 {
            available -= 1
            return
        }
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            waiters.append(cont)
        }
    }

    /// Release one permit. Wakes the next waiter in FIFO order, if any.
    public func signal() {
        if !waiters.isEmpty {
            let cont = waiters.removeFirst()
            cont.resume()
        } else {
            available += 1
        }
    }

    /// `try await sem.with { ... }` — convenience for scoped acquire/release
    /// that always releases even on throw. We call `signal()` directly
    /// (not via `Task { await ... }`) since `with` is itself an actor
    /// method — the unstructured `Task { }` wrap was unnecessary AND
    /// risked reordering signals relative to acquisitions, briefly
    /// allowing more concurrent ANE access than the semaphore intends.
    public func with<T>(_ body: () async throws -> T) async rethrows -> T {
        await wait()
        do {
            let result = try await body()
            signal()
            return result
        } catch {
            signal()
            throw error
        }
    }
}
