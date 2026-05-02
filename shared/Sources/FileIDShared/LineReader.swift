// Reads newline-delimited JSON from a FileHandle. Yields one decoded
// value per line via an AsyncThrowingStream.
//
// Uses GCD's `readabilityHandler` rather than a sync read loop because
// the latter doesn't reliably wake on parent writes when the process
// runs as the child of a SwiftUI .app launched via LaunchServices.
//
// Single-reader contract: only one task should consume from a handle.
import Foundation

public struct LineOverflowError: Error, CustomStringConvertible {
    public let description = "IPC line exceeded 16 MiB cap"
}

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
                let lines: [Data]
                do {
                    lines = try buffer.append(chunk)
                } catch {
                    handle.readabilityHandler = nil
                    continuation.finish(throwing: error)
                    return
                }
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

    /// Like `read`, but surfaces per-line decode errors as `.failure`
    /// instead of tearing the stream down — lets the engine survive
    /// a version-skewed app sending unknown IPCCommand cases. An
    /// overflow on the line buffer itself still finishes the stream
    /// (unrecoverable; parent should respawn).
    public static func readResults<T: Decodable & Sendable>(
        from handle: FileHandle,
        as type: T.Type
    ) -> AsyncStream<Result<T, Error>> {
        AsyncStream<Result<T, Error>> { continuation in
            let buffer = LineBuffer()
            handle.readabilityHandler = { handle in
                let chunk = handle.availableData
                if chunk.isEmpty {
                    if let trailing = buffer.flushAll() {
                        do {
                            let value = try IPCCoder.decoder.decode(T.self, from: trailing)
                            continuation.yield(.success(value))
                        } catch {
                            continuation.yield(.failure(error))
                        }
                    }
                    handle.readabilityHandler = nil
                    continuation.finish()
                    return
                }
                let lines: [Data]
                do {
                    lines = try buffer.append(chunk)
                } catch {
                    continuation.yield(.failure(error))
                    handle.readabilityHandler = nil
                    continuation.finish()
                    return
                }
                for lineData in lines {
                    do {
                        let value = try IPCCoder.decoder.decode(T.self, from: lineData)
                        continuation.yield(.success(value))
                    } catch {
                        continuation.yield(.failure(error))
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
    /// Cap a single in-flight line to 16 MiB. A peer that fails to send
    /// '\n' would otherwise grow the buffer without bound and OOM us.
    private static let maxLineBytes = 16 * 1024 * 1024

    private let lock = NSLock()
    private var buffer = Data()

    init() { buffer.reserveCapacity(64 * 1024) }

    /// Append a chunk, return whole lines (newline-terminated, '\n' stripped).
    /// Throws `LineOverflowError` if a single un-terminated line exceeds
    /// the 16 MiB cap; the caller is expected to finish the stream with
    /// that error so the parent process can decide to respawn.
    func append(_ chunk: Data) throws -> [Data] {
        lock.lock()
        defer { lock.unlock() }
        buffer.append(chunk)
        var lines: [Data] = []
        while let nl = buffer.firstIndex(of: 0x0A) {
            let line = buffer.subdata(in: buffer.startIndex..<nl)
            buffer.removeSubrange(buffer.startIndex...nl)
            if !line.isEmpty { lines.append(line) }
        }
        if buffer.count > Self.maxLineBytes {
            buffer.removeAll(keepingCapacity: false)
            throw LineOverflowError()
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
