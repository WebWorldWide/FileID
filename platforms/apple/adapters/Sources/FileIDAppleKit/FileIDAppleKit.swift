// C entry points the shared Rust engine calls when the Mac app hosts it
// (`fileidd`). The interface is `adapters/include/fileid_apple.h`; the Rust
// mirror is `platforms/windows/src/engine/src/apple/ffi.rs`. Change all three
// together and bump `fileIDAppleABIVersion`.
import FileIDAppleVLM
import Foundation

public let fileIDAppleABIVersion: UInt32 = 1

@_cdecl("fileid_apple_abi_version")
public func fileid_apple_abi_version() -> UInt32 {
    fileIDAppleABIVersion
}

@_cdecl("fileid_apple_shutdown")
public func fileid_apple_shutdown() {
    SleepAssertions.shared.releaseAll()
    BookmarkLeases.shared.closeAll()
    VLMHost.shared.shutdown()
}

@_cdecl("fileid_apple_free")
public func fileid_apple_free(_ pointer: UnsafeMutableRawPointer?) {
    free(pointer)
}

@_cdecl("fileid_apple_sleep_begin")
public func fileid_apple_sleep_begin(_ reason: UnsafePointer<CChar>?) -> UInt32 {
    let text = reason.map { String(cString: $0) } ?? "FileID is working"
    return SleepAssertions.shared.begin(reason: text)
}

@_cdecl("fileid_apple_sleep_end")
public func fileid_apple_sleep_end(_ assertion: UInt32) {
    SleepAssertions.shared.end(assertion)
}

@_cdecl("fileid_apple_bookmark_open")
public func fileid_apple_bookmark_open(
    _ data: UnsafePointer<UInt8>?,
    _ length: Int,
    _ outPath: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?,
    _ outStale: UnsafeMutablePointer<Bool>?
) -> UInt64 {
    outPath?.pointee = nil
    outStale?.pointee = false
    guard let data, length > 0 else { return 0 }
    let bookmark = Data(bytes: data, count: length)
    guard let lease = BookmarkLeases.shared.open(bookmark) else { return 0 }
    guard let path = strdup(lease.path) else {
        BookmarkLeases.shared.close(lease.id)
        return 0
    }
    outPath?.pointee = path
    outStale?.pointee = lease.stale
    return lease.id
}

@_cdecl("fileid_apple_bookmark_close")
public func fileid_apple_bookmark_close(_ lease: UInt64) {
    BookmarkLeases.shared.close(lease)
}
