// Reads newline-delimited JSON from a FileHandle, yielding one decoded value per line via an AsyncThrowingStream.
// Uses GCD's readabilityHandler: a sync read loop doesn't reliably wake on parent writes when the app is launched via LaunchServices.
// Single reader only: one task should consume from a handle.
import Foundation

public struct LineOverflowError: Error, CustomStringConvertible {
    public let capBytes: Int
    public init(capBytes: Int) { self.capBytes = capBytes }
    public var description: String { "IPC line exceeded \(capBytes / (1024 * 1024)) MiB cap" }
}

public enum LineReader {
    /// Yields decoded values until EOF or error.
    public static func read<T: Decodable & Sendable>(
        from handle: FileHandle,
        as type: T.Type
    ) -> AsyncThrowingStream<T, Error> {
        AsyncThrowingStream<T, Error> { continuation in
            let buffer = LineBuffer()
            handle.readabilityHandler = { handle in
                let chunk = handle.availableData
                if chunk.isEmpty {
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

    /// Like `read`, but per-line decode errors surface as `.failure`, so the engine survives a version-skewed app sending unknown IPCCommand cases.
    /// A line-buffer overflow still finishes the stream: the parent should respawn.
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

/// Lock-protected line buffer; readabilityHandler can fire on any thread. `internal` only so SharedTests can reach the framing logic.
final class LineBuffer: @unchecked Sendable {
    /// Caps a single in-flight line at 64 MiB: a peer that never sends a newline would otherwise grow the buffer without bound.
    /// Symmetric with the IPC frame caps, so a whole-library applyRestructure (~200k moves) fits.
    static let maxLineBytes = 64 * 1024 * 1024

    private let lock = NSLock()
    private var buffer = Data()
    /// Count of leading bytes known to contain no newline, so `append` resumes the search instead of rescanning (O(total), not O(n²)).
    private var scannedPrefix = 0

    init() { buffer.reserveCapacity(64 * 1024) }

    /// Appends a chunk and returns whole lines. Throws `LineOverflowError` if one un-terminated line exceeds `maxLineBytes`; the caller finishes the stream with it so the parent can respawn.
    func append(_ chunk: Data) throws -> [Data] {
        lock.lock()
        defer { lock.unlock() }
        buffer.append(chunk)
        var lines: [Data] = []
        while true {
            let searchStart = buffer.startIndex + scannedPrefix
            guard searchStart < buffer.endIndex else { break }
            guard let nl = buffer[searchStart...].firstIndex(of: 0x0A) else {
                // No newline in the tail: the next chunk resumes the search from the end.
                scannedPrefix = buffer.count
                break
            }
            let line = buffer.subdata(in: buffer.startIndex..<nl)
            buffer.removeSubrange(buffer.startIndex...nl)
            scannedPrefix = 0
            if line.count > Self.maxLineBytes {
                buffer.removeAll(keepingCapacity: false)
                throw LineOverflowError(capBytes: Self.maxLineBytes)
            }
            if !line.isEmpty { lines.append(line) }
        }
        if buffer.count > Self.maxLineBytes {
            buffer.removeAll(keepingCapacity: false)
            scannedPrefix = 0
            throw LineOverflowError(capBytes: Self.maxLineBytes)
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
        scannedPrefix = 0
        return out
    }
}
