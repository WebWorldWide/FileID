// Thread-safe non-blocking writer for IPC events.
//
// Architecture:
//   producers (DBWriter, ScanCoordinator, dispatch) → emit(payload)
//     → bounded buffer → single drainer task → IPC wire
//       (the fd-2 pipe, dup'ed aside by IPCTransport.bootstrap so
//        library stderr chatter can't splice into the event stream)
//
// The pipe write is synchronous and blocking — if the parent stops draining
// the pipe fills and the write stalls. So emit() never writes: it enqueues
// non-blockingly, and a single drainer task performs the blocking write on a
// dedicated serial queue OFF the actor (`writeBlocking`), which SUSPENDS — not
// blocks — the actor while a full pipe backs up, so emit()/DBWriter.commitBatch
// keep running. When the buffer is full, new
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
    private let maxFrameBytes = 64 * 1024 * 1024

    /// Wake the drainer when emit() adds work.
    private var drainerContinuation: CheckedContinuation<Void, Never>?

    /// The wire the events go to. Captured at init: the U4 bootstrap dups the
    /// wire fd aside before the singleton exists; tests inject a pipe to capture
    /// the byte stream. Retained for the process lifetime so its file descriptor
    /// (`wireFD`) stays open — all actual writes go through the fd off-actor.
    private let wire: FileHandle
    /// The wire's raw descriptor, captured once. Every write (the drainer's and
    /// `drainAndClose`'s terminal flush) goes through this fd on the dedicated
    /// serial `writeQueue` instead of the actor-isolated `FileHandle`, so a
    /// stalled write parks only that queue's thread and never holds the actor's
    /// isolation.
    private let wireFD: Int32

    public init(wire: FileHandle? = nil) {
        let handle = wire ?? IPCTransport.wireHandle
        self.wire = handle
        self.wireFD = handle.fileDescriptor
    }

    public func emit(_ payload: IPCEvent.Payload) {
        guard !closed else { return }
        startDrainerIfNeeded()
        let event = IPCEvent(payload: payload)
        var line: Data
        do {
            line = try IPCCoder.encodeLine(event)
            if line.count > maxFrameBytes {
                let replacement = IPCEvent(payload: .error(EngineError(
                    kind: "ipc_frame_too_large",
                    message: "The engine refused to send an IPC event larger than 64 MiB. Narrow the requested operation and try again."
                )))
                line = try IPCCoder.encodeLine(replacement)
            }
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

    /// Flush every buffered line to the wire, then close. Called once on graceful
    /// shutdown right before `Darwin._exit(0)`, and by tests right before closing
    /// the pipe's write end: the detached drainer can be parked between batches
    /// (or mid-250 ms timeout) with a terminal event still buffered, and `_exit`
    /// would drop it (F-C3-040).
    ///
    /// The drainer's own writes now run OFF the actor on `writeQueue`, so this
    /// routes through that SAME serial queue via `.sync` — which (a) serializes
    /// the final flush behind any in-flight drainer write so bytes never
    /// interleave, and (b) acts as a BARRIER: because `writeQueue` is serial and
    /// FIFO, the `.sync` block can't run until every write the drainer already
    /// enqueued has completed, so on return the queue is fully drained and the
    /// caller can close the write end (or `_exit`) without truncating in-flight
    /// bytes. This runs with no `await`, so the (suspended) drainer can't take a
    /// new batch mid-call; `buffer` is emptied here, so if it resumes afterward it
    /// sees `closed && buffer.isEmpty` and exits without re-writing. Idempotent.
    public func drainAndClose() {
        let blob: Data? = buffer.isEmpty ? nil : buffer.reduce(Data(), +)
        buffer.removeAll()
        Self.writeQueue.sync {
            if let blob { Self.writeAllSync(blob, fd: wireFD) }
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

    /// Drain loop. Pulls a batch of buffered lines under the actor, then hands
    /// the blocking pipe write to a dedicated serial queue and `await`s it. That
    /// `await` is a real suspension point — the actor's executor is released
    /// while the write is in flight, so emit() (and every other actor-isolated
    /// caller, e.g. DBWriter.commitBatch) keeps running even when the parent has
    /// stopped draining and the pipe is full. One drainer pulling batches in
    /// order + one serial write queue keeps frames ordered.
    ///
    /// (Previously this write ran directly ON the actor via `wire.write`; the
    /// old "only THIS task blocks — the actor is free" claim was FALSE — a
    /// detached Task calling an actor method still runs under actor isolation,
    /// so a full pipe held the actor and wedged the whole pipeline. audit HIGH #3.)
    private func drainLoop() async {
        while !self.isDoneDraining() {
            let batch = self.takeBatch()
            if batch.isEmpty {
                // Wait for emit() to wake us. Cap with a 250 ms timeout so
                // we periodically re-check `closed` (and don't strand on a
                // shutdown that beat us to the channel).
                await self.parkUntilWoken(timeoutMs: 250)
                continue
            }
            // Concatenate then write once — fewer syscalls. The write runs off
            // the actor on `writeQueue`; this task SUSPENDS (not blocks) on it.
            let blob = batch.reduce(Data(), +)
            await Self.writeBlocking(blob, fd: wireFD)
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

    // MARK: - Off-actor writer

    /// Dedicated serial queue for the ONE blocking pipe write. Runs OFF the
    /// actor's executor so a stalled write (parent not draining, pipe full)
    /// parks only this thread. Serial + a single drainer that awaits each write
    /// keeps frames ordered.
    private static let writeQueue = DispatchQueue(
        label: "com.fileid.ipcsink.write", qos: .userInitiated)

    /// Blocking write of one concatenated batch on `writeQueue`; the calling
    /// actor task SUSPENDS (does not block) until it finishes. The newline-
    /// delimited JSON contract is preserved: each buffered line is already
    /// newline-terminated and the bytes are written in order.
    private static func writeBlocking(_ data: Data, fd: Int32) async {
        await withCheckedContinuation { (cont: CheckedContinuation<Void, Never>) in
            writeQueue.async {
                writeAllSync(data, fd: fd)
                cont.resume()
            }
        }
    }

    /// Synchronous byte-exact write of `data` to `fd`. Loops on partial writes
    /// and EINTR; any other error (EPIPE — parent gone, etc.) drops the remainder,
    /// matching the prior `try? wire.write` fire-and-forget behavior. MUST be
    /// called on `writeQueue` (directly, or via `writeQueue.sync`) so the drainer
    /// and the `drainAndClose` terminal flush can never interleave bytes.
    private static func writeAllSync(_ data: Data, fd: Int32) {
        data.withUnsafeBytes { (raw: UnsafeRawBufferPointer) in
            guard let base = raw.baseAddress, raw.count > 0 else { return }
            var offset = 0
            let total = raw.count
            while offset < total {
                let n = Darwin.write(fd, base + offset, total - offset)
                if n > 0 {
                    offset += n
                } else if n < 0 && errno == EINTR {
                    continue
                } else {
                    break   // EPIPE / fatal — parent gone; drop the rest.
                }
            }
        }
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
    // Each needle matches a JSON *object key* (followed by `:{`) so that user-data
    // strings (a tag named "ready", a caption containing "error", a path component
    // named "scanComplete") never trigger a false-positive critical pin. Swift's
    // default Codable encoding for enum cases always emits `"caseName":{...}`.
    private static let criticalNeedles: [Data] = [
        Data("\"ready\":{".utf8),
        Data("\"error\":{".utf8),
        Data("\"scanComplete\":{".utf8),
        Data("\"deepAnalyzeComplete\":{".utf8),
        Data("\"faceClusteringComplete\":{".utf8),
        Data("\"restructurePlan\":{".utf8),
        Data("\"restructureApplyResult\":{".utf8),
        Data("\"discoveryComplete\":{".utf8),
        Data("\"phaseChanged\":{".utf8),
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
