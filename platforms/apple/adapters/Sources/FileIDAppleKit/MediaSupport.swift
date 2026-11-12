import Foundation

/// A refusal or failure whose text is shown to the user unchanged.
struct MediaFailure: LocalizedError, Sendable {
    let text: String
    init(_ text: String) { self.text = text }
    var errorDescription: String? { text }
}

/// The caller's cancellation flag (a Rust `AtomicBool`, which has the layout of
/// a C `bool`), polled while long work runs. A single-byte load cannot tear;
/// the non-inlined read keeps the poll from being hoisted out of a loop.
struct CancelFlag: @unchecked Sendable {
    private let pointer: UnsafeRawPointer?
    init(_ pointer: UnsafePointer<Bool>?) { self.pointer = pointer.map(UnsafeRawPointer.init) }
    static let never = CancelFlag(nil)

    var isSet: Bool { pointer.map(Self.load) ?? false }

    @inline(never)
    private static func load(_ pointer: UnsafeRawPointer) -> Bool {
        pointer.load(as: UInt8.self) != 0
    }
}

enum MediaBridge {
    private final class Box<T>: @unchecked Sendable {
        var result: Result<T, Error> = .failure(CancellationError())
    }

    /// Runs async AVFoundation work to completion from a C caller's thread. The
    /// Rust host has no main run loop, so nothing here may wait on the main queue.
    static func wait<T>(_ body: @escaping @Sendable () async throws -> T) throws -> T {
        let box = Box<T>()
        let done = DispatchSemaphore(value: 0)
        Task.detached(priority: .userInitiated) {
            do { box.result = .success(try await body()) } catch { box.result = .failure(error) }
            done.signal()
        }
        done.wait()
        return try box.result.get()
    }

    static func string(_ pointer: UnsafePointer<CChar>?) -> String? {
        pointer.map { String(cString: $0) }
    }

    /// A JSON object as a malloc'd C string the caller frees with `fileid_apple_free`.
    static func reply(_ object: [String: Any]) -> UnsafeMutablePointer<CChar>? {
        guard JSONSerialization.isValidJSONObject(object),
              let data = try? JSONSerialization.data(withJSONObject: object, options: [.sortedKeys]) else {
            return strdup("{\"error\":\"The media adapter could not encode its reply.\"}")
        }
        return strdup(String(decoding: data, as: UTF8.self))
    }

    static func failure(_ error: Error) -> UnsafeMutablePointer<CChar>? {
        let text = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        return reply(["error": String(text.prefix(800))])
    }

    static func copy(_ bytes: [UInt8]) -> UnsafeMutablePointer<UInt8>? {
        guard !bytes.isEmpty, let buffer = malloc(bytes.count)?.assumingMemoryBound(to: UInt8.self) else { return nil }
        bytes.withUnsafeBufferPointer { buffer.update(from: $0.baseAddress!, count: bytes.count) }
        return buffer
    }
}
