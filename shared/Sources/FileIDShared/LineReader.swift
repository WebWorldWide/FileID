// Reads newline-delimited JSON from a FileHandle. Yields one decoded
// value per line via an AsyncThrowingStream.
//
// Uses GCD's `readabilityHandler` rather than a sync read loop because
// the latter doesn't reliably wake on parent writes when the process
// runs as the child of a SwiftUI .app launched via LaunchServices.
//
// Single-reader contract: only one task should consume from a handle.
import Foundation

public enum LineReader {
    /// Yields decoded `T` values, one per newline-terminated JSON line, until
    /// the handle reports EOF or an error.
    public static func read<T: Decodable & Sendable>(
        from handle: FileHandle,
        as type: T.Type
    ) -> AsyncThrowingStream<T, Error> {
        AsyncThrowingStream<T, Error> { continuation in
            let buffer = LineBuffer()
            handle.readabilityHandler = { handle in
                let chunk = handle.availableData
                if chunk.isEmpty {
                    // EOF — flush any trailing line without newline.
                    if let trailing = buffer.flushAll(),
                       let value = try? IPCCoder.decoder.decode(T.self, from: trailing) {
                        continuation.yield(value)
                    }
                    handle.readabilityHandler = nil
                    continuation.finish()
                    return
                }
                let lines = buffer.append(chunk)
                for lineData in lines {
                    do {
                        let value = try IPCCoder.decoder.decode(T.self, from: lineData)
                        continuation.yield(value)
                    } catch {
                        // Malformed line — surface as a stream error and stop.
                        handle.readabilityHandler = nil
                        continuation.finish(throwing: error)
                        return
                    }
                }
            }
            continuation.onTermination = { _ in
                handle.readabilityHandler = nil
            }
        }
    }
}

/// Lock-protected line buffer (readabilityHandler can fire on any thread).
private final class LineBuffer: @unchecked Sendable {
    private let lock = NSLock()
    private var buffer = Data()

    init() { buffer.reserveCapacity(64 * 1024) }

    /// Append a chunk, return whole lines (newline-terminated, '\n' stripped).
    func append(_ chunk: Data) -> [Data] {
        lock.lock()
        defer { lock.unlock() }
        buffer.append(chunk)
        var lines: [Data] = []
        while let nl = buffer.firstIndex(of: 0x0A) {
            let line = buffer.subdata(in: buffer.startIndex..<nl)
            buffer.removeSubrange(buffer.startIndex...nl)
            if !line.isEmpty { lines.append(line) }
        }
        return lines
    }

    /// On EOF, return whatever is left in the buffer (no trailing newline).
    func flushAll() -> Data? {
        lock.lock()
        defer { lock.unlock() }
        guard !buffer.isEmpty else { return nil }
        let out = buffer
        buffer = Data()
        return out
    }
}
