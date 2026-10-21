import Foundation

/// Raw little-endian Float32 storage for embeddings and centroids, shared
/// with the Windows engine's DB format.
public enum FloatBlob {
    public static func encode(_ values: [Float]) -> Data {
        values.withUnsafeBufferPointer { Data(buffer: $0) }
    }

    /// Trailing bytes that don't fill a Float are ignored; an empty or short
    /// blob decodes to an empty array.
    public static func decode(_ data: Data) -> [Float] {
        let count = data.count / MemoryLayout<Float>.stride
        guard count > 0 else { return [] }
        return [Float](unsafeUninitializedCapacity: count) { buffer, initialized in
            data.withUnsafeBytes { raw in
                UnsafeMutableRawBufferPointer(buffer).copyMemory(
                    from: UnsafeRawBufferPointer(rebasing: raw[0..<count * MemoryLayout<Float>.stride]))
            }
            initialized = count
        }
    }
}
