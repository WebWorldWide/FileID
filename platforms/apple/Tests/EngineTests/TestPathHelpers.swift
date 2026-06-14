import Foundation
@testable import FileIDEngine

/// Captures everything written to an `IPCSink` over a pipe, WITHOUT parking a
/// cooperative-pool thread on a blocking read. The prior per-test pattern —
/// `let reader = Task.detached { while true { pipe…availableData … } }` — held a
/// Swift concurrency thread on the blocking `availableData`; several of those
/// running in parallel across suites starved the executor and intermittently
/// wedged the swift-testing harness to the 12-min CI SIGALRM. A GCD
/// `readabilityHandler` reads on GCD's own threads (off the cooperative pool) and
/// appends to a lock-guarded buffer in arrival order (the handler fires serially
/// per handle), so no actor `await` and no thread parking.
final class WireCapture: @unchecked Sendable {
    /// Lock-guarded byte buffer captured BY the readability handler. The handler
    /// must be `@Sendable`, so it captures this Sendable box — NOT `self` (a
    /// `[weak self]` capture trips "non-sendable function value" on the CI Swift
    /// toolchain even though `WireCapture` is @unchecked Sendable).
    private final class Box: @unchecked Sendable {
        private let lock = NSLock()
        private var buffer = Data()
        func append(_ d: Data) { lock.lock(); buffer.append(d); lock.unlock() }
        func bytes() -> Data { lock.lock(); defer { lock.unlock() }; return buffer }
    }

    let sink: IPCSink
    private let pipe = Pipe()
    private let box = Box()

    init() {
        sink = IPCSink(wire: pipe.fileHandleForWriting)
        let box = self.box   // capture the Sendable box, not self
        // Explicit @Sendable type: the older CI Swift toolchain does not infer
        // the readability-handler closure as @Sendable from context, so annotate
        // it. The only capture is `box` (an @unchecked Sendable class), so the
        // check passes.
        let handler: @Sendable (FileHandle) -> Void = { handle in
            let chunk = handle.availableData
            guard !chunk.isEmpty else { handle.readabilityHandler = nil; return }
            box.append(chunk)
        }
        pipe.fileHandleForReading.readabilityHandler = handler
    }

    /// All bytes received so far (synchronous; poll this in a deadline loop).
    func bytes() -> Data { box.bytes() }

    /// Close the sink and the write end so the reader sees EOF and deregisters.
    func finish() async {
        await sink.close()
        try? pipe.fileHandleForWriting.close()
    }
}

/// Resolve a URL to its REAL filesystem path via `realpath(3)`.
///
/// Unlike Foundation's `URL.resolvingSymlinksInPath()` — which applies a macOS
/// special case that STRIPS a leading `/private` — `realpath` returns the fully
/// resolved path INCLUDING `/private` (e.g. `/var/folders/…` → `/private/var/folders/…`).
/// That matters in tests that use `FileManager.temporaryDirectory` (under the
/// `/var` → `/private/var` symlink): the `FileManager` directory enumerator emits
/// `/private/var/…` paths, so a test root resolved with `resolvingSymlinksInPath`
/// (`/var/…`) would NOT match the enumerated paths and the incremental skip-set
/// range/lookup would silently miss. Real scan roots (`/Users/…`, `/Volumes/…`)
/// never hit `/private`, so this only affects the temp-dir test environment.
func realResolved(_ url: URL) -> URL {
    guard let resolved = realpath(url.path, nil) else { return url }
    defer { free(resolved) }
    return URL(fileURLWithPath: String(cString: resolved))
}
