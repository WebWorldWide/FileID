import CryptoKit
import Darwin
import Foundation

public enum ExactFileDigest {
    public struct CacheKey: Hashable, Sendable {
        public let path: String
        public let size: UInt64
        public let modifiedSeconds: Int64
        public let modifiedNanoseconds: Int64
        public let device: UInt64
        public let inode: UInt64
    }

    public struct Match: Sendable {
        public let first: CacheKey
        public let second: CacheKey
    }

    private static let chunkSize = 1024 * 1024

    public static func cacheKey(url: URL, expectedSize: Int64) -> CacheKey? {
        guard expectedSize >= 0,
              let key = pathIdentity(url: url),
              key.size == UInt64(expectedSize) else { return nil }
        return key
    }

    public static func compute(url: URL, expectedSize: Int64) -> Data? {
        computeWithIdentity(url: url, expectedSize: expectedSize)?.digest
    }

    public static func computeWithIdentity(
        url: URL, expectedSize: Int64
    ) -> (digest: Data, identity: CacheKey)? {
        guard expectedSize >= 0,
              let handle = try? FileHandle(forReadingFrom: url),
              let before = handleIdentity(handle: handle, path: url.path),
              before.size == UInt64(expectedSize),
              pathIdentity(url: url) == before else { return nil }
        defer { try? handle.close() }
        var digest = SHA256()
        var bytesRead: UInt64 = 0
        do {
            while let data = try handle.read(upToCount: chunkSize), !data.isEmpty {
                bytesRead += UInt64(data.count)
                guard bytesRead <= before.size else { return nil }
                digest.update(data: data)
            }
        } catch {
            return nil
        }
        guard bytesRead == before.size,
              handleIdentity(handle: handle, path: url.path) == before,
              pathIdentity(url: url) == before else { return nil }
        return (Data(digest.finalize()), before)
    }

    public static func match(
        first: URL, second: URL, expectedSize: Int64
    ) -> Bool {
        matchWithIdentity(
            first: first, second: second, expectedSize: expectedSize) != nil
    }

    public static func matchWithIdentity(
        first: URL, second: URL, expectedSize: Int64
    ) -> Match? {
        guard let firstResult = computeWithIdentity(
                  url: first, expectedSize: expectedSize),
              let secondResult = computeWithIdentity(
                  url: second, expectedSize: expectedSize),
              firstResult.digest == secondResult.digest else { return nil }
        return Match(first: firstResult.identity, second: secondResult.identity)
    }

    public static func pathStillMatches(_ key: CacheKey) -> Bool {
        pathIdentity(url: URL(fileURLWithPath: key.path)) == key
    }

    private static func pathIdentity(url: URL) -> CacheKey? {
        var info = stat()
        let found = url.withUnsafeFileSystemRepresentation { path in
            guard let path else { return false }
            return lstat(path, &info) == 0
        }
        guard found else { return nil }
        return key(path: url.path, info: info)
    }

    private static func handleIdentity(handle: FileHandle, path: String) -> CacheKey? {
        var info = stat()
        guard fstat(handle.fileDescriptor, &info) == 0 else { return nil }
        return key(path: path, info: info)
    }

    private static func key(path: String, info: stat) -> CacheKey? {
        guard (info.st_mode & S_IFMT) == S_IFREG, info.st_size >= 0 else { return nil }
        return CacheKey(
            path: path,
            size: UInt64(info.st_size),
            modifiedSeconds: Int64(info.st_mtimespec.tv_sec),
            modifiedNanoseconds: Int64(info.st_mtimespec.tv_nsec),
            device: UInt64(info.st_dev),
            inode: UInt64(info.st_ino))
    }
}
