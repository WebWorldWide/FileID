import Foundation

/// A file's modification time as every engine stores it in `files.modified_at`: whole seconds
/// plus nanoseconds divided by 1e9, in that order, the arithmetic of Rust's
/// `Duration::as_secs_f64`. `Date.timeIntervalSince1970` rounds the same instant differently
/// in the last bit for about a quarter of all timestamps, so a stored time is never read from
/// a `Date`.
public enum FileModificationTime {
    /// A time before 1970 (or a malformed one) has no value: the Rust engine's
    /// `duration_since(UNIX_EPOCH)` fails there too.
    public static func unixSeconds(seconds: Int64, nanoseconds: Int64) -> Double? {
        guard seconds >= 0, (0..<1_000_000_000).contains(nanoseconds) else { return nil }
        return Double(seconds) + Double(nanoseconds) / 1_000_000_000.0
    }

    public static func unixSeconds(_ info: stat) -> Double? {
        unixSeconds(seconds: Int64(info.st_mtimespec.tv_sec), nanoseconds: Int64(info.st_mtimespec.tv_nsec))
    }

    /// The path's own time when `followLinks` is false (`lstat`), the target's otherwise.
    public static func unixSeconds(path: String, followLinks: Bool = true) -> Double? {
        var info = stat()
        guard (followLinks ? stat(path, &info) : lstat(path, &info)) == 0 else { return nil }
        return unixSeconds(info)
    }
}

/// Revisions are `"<size>:<IEEE bits of modified_at>"`. Two engines that stored the same
/// instant as doubles one bit apart name the same file state, so a revision read back from a
/// row is compared the way the scanners compare times: same size and a time within a
/// microsecond.
public enum SourceRevisionMatch {
    public static func same(_ lhs: String, _ rhs: String) -> Bool {
        if lhs == rhs { return true }
        let left = lhs.split(separator: ":", maxSplits: 1), right = rhs.split(separator: ":", maxSplits: 1)
        guard left.count == 2, right.count == 2, left[0] == right[0],
              let leftBits = UInt64(left[1]), let rightBits = UInt64(right[1]) else { return false }
        let leftTime = Double(bitPattern: leftBits), rightTime = Double(bitPattern: rightBits)
        return leftTime.isFinite && rightTime.isFinite && abs(leftTime - rightTime) < 0.000_001
    }
}
