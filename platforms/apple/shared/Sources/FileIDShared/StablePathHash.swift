// Cross-platform `files.path_hash` — bit-identical port of the Windows
// engine's `stable_path_hash` (engine/src/util/path_safety.rs): Rust's
// DefaultHasher is SipHash-1-3 with zero keys, and `str::hash` feeds the
// UTF-8 bytes plus a single 0xFF length-extension terminator. Input is
// ASCII-lowercased first (multi-byte UTF-8 sequences are untouched) so a
// re-scan after a path-case change on a case-insensitive volume produces
// the same hash. Pinned against Rust-computed vectors in
// StablePathHashTests; if either side drifts, the shared-schema contract
// (same library DB readable by both engines) breaks silently.
public enum StablePathHash {

    public static func hash(_ path: String) -> Int64 {
        var bytes = Array(path.utf8)
        for i in bytes.indices where bytes[i] >= 0x41 && bytes[i] <= 0x5A {
            bytes[i] |= 0x20
        }
        bytes.append(0xFF)
        return Int64(bitPattern: sipHash13(bytes))
    }

    private static func sipHash13(_ data: [UInt8]) -> UInt64 {
        var v0: UInt64 = 0x736f_6d65_7073_6575
        var v1: UInt64 = 0x646f_7261_6e64_6f6d
        var v2: UInt64 = 0x6c79_6765_6e65_7261
        var v3: UInt64 = 0x7465_6462_7974_6573

        func round() {
            v0 = v0 &+ v1; v1 = (v1 << 13) | (v1 >> 51); v1 ^= v0
            v0 = (v0 << 32) | (v0 >> 32)
            v2 = v2 &+ v3; v3 = (v3 << 16) | (v3 >> 48); v3 ^= v2
            v0 = v0 &+ v3; v3 = (v3 << 21) | (v3 >> 43); v3 ^= v0
            v2 = v2 &+ v1; v1 = (v1 << 17) | (v1 >> 47); v1 ^= v2
            v2 = (v2 << 32) | (v2 >> 32)
        }

        let len = data.count
        let blocks = len / 8
        for i in 0..<blocks {
            var m: UInt64 = 0
            for j in 0..<8 { m |= UInt64(data[i * 8 + j]) << (8 * UInt64(j)) }
            v3 ^= m
            round()
            v0 ^= m
        }
        var b = UInt64(len & 0xFF) << 56
        for j in 0..<(len % 8) { b |= UInt64(data[blocks * 8 + j]) << (8 * UInt64(j)) }
        v3 ^= b
        round()
        v0 ^= b
        v2 ^= 0xFF
        round(); round(); round()
        return v0 ^ v1 ^ v2 ^ v3
    }
}
