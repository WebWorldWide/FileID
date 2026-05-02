// JSON encoder/decoder configured for IPC. Centralized so engine and app
// always agree on date format, key strategy, and float precision.
import Foundation

public enum IPCCoder {
    public static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.dateEncodingStrategy = .iso8601
        // .sortedKeys: deterministic byte-for-byte output. Required so
        // round-trip tests can compare bytes, and so different macOS
        // releases (which changed default key order in macOS 26) emit
        // the same wire frames.
        e.outputFormatting = [.sortedKeys]
        return e
    }()

    public static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.dateDecodingStrategy = .iso8601
        return d
    }()

    /// Encode a value as a single line of JSON terminated by '\n'. Caller writes
    /// the resulting Data to the wire (stdout / pipe / socket).
    public static func encodeLine<T: Encodable>(_ value: T) throws -> Data {
        var data = try encoder.encode(value)
        data.append(0x0A) // '\n'
        return data
    }
}
