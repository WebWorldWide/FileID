// Thread-safe non-blocking writer for IPC events.
//
// Architecture:
//   producers (DBWriter, ScanCoordinator, dispatch) → emit(payload)
//     → bounded buffer → single drainer task → IPC wire
//       (the fd-2 pipe, dup'ed aside by IPCTransport.bootstrap so
//        library stderr chatter can't splice into the event stream)
//
// `FileHandle.write(...)` is synchronous — if the parent stops draining
// the pipe, every emit() blocks behind it and the engine wedges.
// emit() enqueues non-blockingly; when the buffer is full, new
// `progress` events overwrite the latest one in place and other
// non-critical events drop the oldest. Critical events (`error`,
// `scanComplete`, `ready`, `faceClusteringComplete`) are always kept.
import Foundation
import Darwin
import FileIDShared

public actor IPCSink {
    public static let shared = IPCSink()
    private var closed = false
    private var drainerStarted = false

    /// In-memory buffer. Kept small — IPC events are <1 KB; 1024 entries
    /// = ~1 MB worst case. Anything beyond means the parent is dead-slow,
    /// in which case dropping is the right answer.
    private var buffer: [Data] = []
    private let maxBuffer = 1024

    /// Wake the drainer when emit() adds work.
    private var drainerContinuation: CheckedContinuation<Void, Never>?

    /// Where events are written. Captured at init: the U4 bootstrap dups
    /// the wire fd aside before the singleton exists; tests inject a pipe
    /// to capture the byte stream.
    private let wire: FileHandle

    public init(wire: FileHandle? = nil) {
        self.wire = wire ?? IPCTransport.wireHandle
    }

    public func emit(_ payload: IPCEvent.Payload) {
        guard !closed else { return }
        startDrainerIfNeeded()
        let event = IPCEvent(payload: payload)
        let line: Data
        do {
            line = try IPCCoder.encodeLine(event)
        } catch {
            return
        }

        // Backpressure policy: if buffer is full, drop oldest non-critical
        // entry. Critical events go to the front of the line.
        if buffer.count >= maxBuffer {
            let isCritical = Self.isCritical(payload)
            if isCritical {
                // Find the oldest non-critical-looking entry to evict.
                if let dropIdx = buffer.firstIndex(where: { !Self.entryLooksCritical($0) }) {
                    buffer.remove(at: dropIdx)
                } else {
                    // All-critical buffer (shouldn't happen). Drop oldest.
                    buffer.removeFirst()
                }
            } else {
                // Non-critical can be dropped. For progress events, overwrite
                // the most recent buffered PROGRESS entry instead of growing.
                //
                // The old predicate used `"\"progress\"".utf8.first!` — the
                // FIRST byte of the literal, i.e. the `"` (0x22) character.
                // `Data.contains(_: UInt8)` then just asked "does this line
                // contain a double-quote?" — true for EVERY JSON line — so it
                // overwrote the newest buffered entry of ANY kind, including a
                // buffered scanComplete / faceClusteringComplete, which then
                // never reached the app (UI stuck mid-scan). Match the full
                // byte needle and never clobber a critical-looking entry.
                if case .progress = payload,
                   let lastProgressIdx = buffer.lastIndex(where: {
                       $0.range(of: Self.progressNeedle) != nil && !Self.entryLooksCritical($0)
                   }) {
                    buffer[lastProgressIdx] = line
                    return
                }
                // Otherwise drop oldest.
                buffer.removeFirst()
            }
        }
        buffer.append(line)
        // Wake the drainer if it's waiting.
        drainerContinuation?.resume()
        drainerContinuation = nil
    }

    public func close() {
        closed = true
        drainerContinuation?.resume()
        drainerContinuation = nil
    }

    /// Spawn the single background drainer the first time emit is called.
    private func startDrainerIfNeeded() {
        guard !drainerStarted else { return }
        drainerStarted = true
        Task.detached(priority: .userInitiated) { [weak self] in
            await self?.drainLoop()
        }
    }

    /// Drain loop. Pulls a batch of buffered lines under the actor, then
    /// performs ONE blocking write outside the actor's hot path. Even if the
    /// parent is glacial, only this one task waits — emit() never does.
    private func drainLoop() async {
        while await !self.isDoneDraining() {
            let batch = await self.takeBatch()
            if batch.isEmpty {
                // Wait for emit() to wake us. Cap with a 250 ms timeout so
                // we periodically re-check `closed` (and don't strand on a
                // shutdown that beat us to the channel).
                await self.parkUntilWoken(timeoutMs: 250)
                continue
            }
            // Concatenate then write once — fewer syscalls.
            let blob = batch.reduce(Data(), +)
            // BLOCKING write — but only THIS task blocks. The actor is free
            // to keep accepting new emit() calls while we wait.
            do { try wire.write(contentsOf: blob) } catch { /* parent gone */ }
        }
    }

    private func isDoneDraining() -> Bool {
        return closed && buffer.isEmpty
    }

    /// Atomically take everything in the buffer (up to a sane chunk cap).
    private func takeBatch() -> [Data] {
        let cap = 64
        let n = min(buffer.count, cap)
        guard n > 0 else { return [] }
        let head = Array(buffer.prefix(n))
        buffer.removeFirst(n)
        return head
    }

    /// Suspend the drainer until emit() wakes it, OR a fallback timeout.
    private func parkUntilWoken(timeoutMs: Int) async {
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            // If a previous waiter is somehow still installed, resume it
            // immediately — only one waiter at a time.
            drainerContinuation?.resume()
            drainerContinuation = cont
            // Backstop timeout — wake after `timeoutMs` even if no emit fires.
            Task { [weak self] in
                try? await Task.sleep(nanoseconds: UInt64(timeoutMs) * 1_000_000)
                await self?.wakeDrainerIfWaiting()
            }
        }
    }

    private func wakeDrainerIfWaiting() {
        drainerContinuation?.resume()
        drainerContinuation = nil
    }

    // MARK: - Critical-event policy

    private static func isCritical(_ p: IPCEvent.Payload) -> Bool {
        switch p {
        case .ready, .error, .scanComplete, .faceClusteringComplete, .discoveryComplete, .phaseChanged:
            return true
        default:
            return false
        }
    }

    /// Heuristic: spot a serialized critical event by byte-level needle
    /// search instead of UTF-8 decode + 6 substring scans. Called in the
    /// hot path of buffer eviction — `Data.range(of:)` matches the bytes
    /// directly without allocating a String.
    private static let criticalNeedles: [Data] = [
        Data("\"ready\"".utf8),
        Data("\"error\"".utf8),
        Data("\"scanComplete\"".utf8),
        Data("\"faceClusteringComplete\"".utf8),
        Data("\"discoveryComplete\"".utf8),
        Data("\"phaseChanged\"".utf8),
    ]
    private static func entryLooksCritical(_ data: Data) -> Bool {
        for needle in criticalNeedles {
            if data.range(of: needle) != nil { return true }
        }
        return false
    }

    /// Byte needle for the serialized `progress` event variant. Used by the
    /// full-buffer coalescing path to find the most recent progress line.
    private static let progressNeedle = Data("\"progress\"".utf8)
}
