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

@_cdecl("fileid_apple_trash_item")
public func fileid_apple_trash_item(
    _ path: UnsafePointer<CChar>?,
    _ outLanded: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?,
    _ outError: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    outLanded?.pointee = nil
    outError?.pointee = nil
    guard let path else {
        setOut(outError, "no path")
        return -1
    }
    do {
        if let landed = try TrashMover.trash(URL(fileURLWithPath: String(cString: path))) {
            setOut(outLanded, landed.path)
        }
        return 0
    } catch {
        setOut(outError, errorText(error))
        return -1
    }
}

@_cdecl("fileid_apple_tags_read")
public func fileid_apple_tags_read(
    _ path: UnsafePointer<CChar>?,
    _ outError: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> UnsafeMutablePointer<CChar>? {
    outError?.pointee = nil
    guard let path else {
        setOut(outError, "no path")
        return nil
    }
    do {
        let url = URL(fileURLWithPath: String(cString: path))
        guard FileManager.default.fileExists(atPath: url.path) else {
            throw CocoaError(.fileReadNoSuchFile, userInfo: [NSFilePathErrorKey: url.path])
        }
        let json = try JSONEncoder().encode(try FinderTags.read(url))
        return strdup(String(decoding: json, as: UTF8.self))
    } catch {
        setOut(outError, errorText(error))
        return nil
    }
}

@_cdecl("fileid_apple_tags_write")
public func fileid_apple_tags_write(
    _ path: UnsafePointer<CChar>?,
    _ tags: UnsafePointer<UnsafePointer<CChar>?>?,
    _ count: Int,
    _ outError: UnsafeMutablePointer<UnsafeMutablePointer<CChar>?>?
) -> Int32 {
    outError?.pointee = nil
    guard let path, count >= 0, count == 0 || tags != nil else {
        setOut(outError, "no path or tags")
        return -1
    }
    let names = (0..<count).compactMap { index in tags?[index].map { String(cString: $0) } }
    do {
        try FinderTags.write(names, to: URL(fileURLWithPath: String(cString: path)))
        return 0
    } catch {
        setOut(outError, errorText(error))
        return -1
    }
}
