// IPC transport bootstrap (U4).
//
// IPC events travel over fd 2 — the app reads that pipe and decodes one
// JSON event per line. But MLX/Metal/ggml C++ code also writes
// diagnostics to fd 2, and a multi-line diagnostic can splice into the
// middle of a batched event write with no framing recovery. Before
// anything else runs (critically: before any MLX/ONNX/Metal init), dup
// the wire fd aside for the sink's exclusive use and repoint fd 2 at a
// local log file so library chatter can never corrupt the event stream.
// The engine keeps the dup'ed pipe open until process exit, so the
// app's EOF-based death detection is unaffected.
import Foundation
import Darwin

enum IPCTransport {
    /// The wire the IPCSink writes events to. Defaults to stderr so a
    /// failed bootstrap (or a test-constructed sink) behaves exactly
    /// like the legacy shared-fd transport. Set once in bootstrap()
    /// before the first IPCSink is created.
    nonisolated(unsafe) static var wireHandle: FileHandle = .standardError

    static func bootstrap() {
        let saved = dup(STDERR_FILENO)
        guard saved >= 0 else { return }
        _ = fcntl(saved, F_SETFD, FD_CLOEXEC)

        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)
            .first!
            .appendingPathComponent("FileID/logs", isDirectory: true)
        try? FileManager.default.createDirectory(at: base, withIntermediateDirectories: true)
        let logURL = base.appendingPathComponent("engine-stderr.log")

        // Same 32 MB single-generation rotation as JSONLog.
        if let size = try? FileManager.default.attributesOfItem(atPath: logURL.path)[.size] as? UInt64,
           size > 32 * 1024 * 1024 {
            let rotated = base.appendingPathComponent("engine-stderr.log.1")
            try? FileManager.default.removeItem(at: rotated)
            try? FileManager.default.moveItem(at: logURL, to: rotated)
        }

        let logFD = open(logURL.path, O_WRONLY | O_CREAT | O_APPEND, 0o644)
        guard logFD >= 0 else {
            // Unwritable log dir (disk full, sandbox surprise): keep the
            // legacy shared-fd behavior instead of dying before we can
            // report anything.
            close(saved)
            return
        }
        dup2(logFD, STDERR_FILENO)
        close(logFD)
        wireHandle = FileHandle(fileDescriptor: saved, closeOnDealloc: false)
    }
}
