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
// non-critical events drop the OLDEST non-critical entry. Critical
// events — every terminal completion (`scanComplete`,
// `deepAnalyzeComplete`, `faceClusteringComplete`, `restructurePlan`,
// `restructureApplyResult`, `error`) plus `ready` / `discoveryComplete`
// / `phaseChanged` — are pinned and never evicted.
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

        // Backpressure policy: a full buffer means the parent is draining
        // slowly. Coalesce a progress flood in place; otherwise evict the
        // OLDEST progress-class (non-critical) entry to make room. A pinned
        // critical event — every terminal completion (scanComplete,
        // deepAnalyzeComplete, faceClusteringComplete, restructurePlan,
        // restructureApplyResult, error) plus ready / discoveryComplete /
        // phaseChanged — is NEVER
        // evicted: dropping a buffered terminal strands that tab's UI forever
        // (F-C3-029/030). The old code's `removeFirst()` ignored criticality
        // and could drop exactly such an entry sitting at the front.
        if buffer.count >= maxBuffer {
            // For progress events, overwrite the most recent buffered PROGRESS
            // entry instead of growing/evicting.
            //
            // The old predicate used `"\"progress\"".utf8.first!` — the FIRST
            // byte of the literal, i.e. the `"` (0x22) character.
            // `Data.contains(_: UInt8)` then just asked "does this line contain
            // a double-quote?" — true for EVERY JSON line — so it overwrote the
            // newest buffered entry of ANY kind, including a buffered terminal
            // event, which then never reached the app (UI stuck mid-scan).
            // Match the full byte needle and never clobber a critical entry.
            if case .progress = payload,
               let lastProgressIdx = buffer.lastIndex(where: {
                   $0.range(of: Self.progressNeedle) != nil && !Self.entryLooksCritical($0)
               }) {
                buffer[lastProgressIdx] = line
                return
            }
            // Make room by evicting the oldest NON-critical entry.
            if let dropIdx = buffer.firstIndex(where: { !Self.entryLooksCritical($0) }) {
                buffer.remove(at: dropIdx)
            } else if !Self.isCritical(payload) {
                // Every buffered entry is pinned and the newcomer isn't — drop
                // the newcomer rather than evict a pinned event or grow.
                return
            }
            // else: the buffer is all-critical AND the newcomer is critical too
            // — fall through and append. Terminal/critical events are bounded (a
            // handful per session), so this can't realistically exceed maxBuffer,
            // and losing a terminal event is by far the worse outcome.
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

    /// Flush every buffered line straight to the wire, then close. Called once
    /// on graceful shutdown right before `Darwin._exit(0)`: the detached drainer
    /// can be parked between batches (or mid-250ms timeout) with a terminal
    /// event still buffered, and `_exit` would drop it (F-C3-040). This runs
    /// under the actor, so it can't race the drainer's own take-then-write
    /// (both are actor-isolated) and never re-writes a batch the drainer
    /// already removed. Idempotent.
    public func drainAndClose() {
        if !buffer.isEmpty {
            let blob = buffer.reduce(Data(), +)
            do { try wire.write(contentsOf: blob) } catch { /* parent gone */ }
            buffer.removeAll()
        }
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

    // `internal` (not `private`) so the eviction-policy regression tests can
    // assert every terminal completion is pinned. (F-C3-029/030)
    static func isCritical(_ p: IPCEvent.Payload) -> Bool {
        switch p {
        // Terminal completions — every one strands a tab's UI if lost.
        // restructurePlan is the success-path terminal reply for planRestructure
        // (its error twin, plan_restructure_failed, is .error and already pinned);
        // omitting it let a successful plan be evicted while a failed one always
        // landed — the asymmetry the re-audit flagged (R-15).
        case .scanComplete, .deepAnalyzeComplete, .faceClusteringComplete,
             .restructurePlan, .restructureApplyResult, .error,
        // Non-terminal but still must never be coalesced away.
             .ready, .discoveryComplete, .phaseChanged:
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
        Data("\"deepAnalyzeComplete\"".utf8),
        Data("\"faceClusteringComplete\"".utf8),
        Data("\"restructurePlan\"".utf8),
        Data("\"restructureApplyResult\"".utf8),
        Data("\"discoveryComplete\"".utf8),
        Data("\"phaseChanged\"".utf8),
    ]
    static func entryLooksCritical(_ data: Data) -> Bool {
        for needle in criticalNeedles {
            if data.range(of: needle) != nil { return true }
        }
        return false
    }

    /// Byte needle for the serialized `progress` event variant. Used by the
    /// full-buffer coalescing path to find the most recent progress line.
    private static let progressNeedle = Data("\"progress\"".utf8)
}
