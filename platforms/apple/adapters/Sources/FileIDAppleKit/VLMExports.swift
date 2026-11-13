// Stage 7 C entry points: MLX vision-language model sessions (Deep Analyze,
// chat answers, selection checks, goal ranking, benchmarks, transcription,
// timeline captions). See fileid_apple.h for the contract.
import FileIDAppleVLM
import Foundation

public typealias FileIDAppleVLMCallback = @convention(c) (UnsafeMutableRawPointer?, UInt32, UnsafePointer<UInt8>?, Int) -> Void

private struct CallbackTarget: @unchecked Sendable {
    let callback: FileIDAppleVLMCallback
    let context: UnsafeMutableRawPointer?
}

@_cdecl("fileid_apple_vlm_submit")
public func fileid_apple_vlm_submit(
    _ request: UnsafePointer<UInt8>?,
    _ length: Int,
    _ callback: FileIDAppleVLMCallback?,
    _ context: UnsafeMutableRawPointer?
) -> UInt64 {
    guard let request, length > 0, let callback else { return 0 }
    let body = Data(bytes: request, count: length)
    let target = CallbackTarget(callback: callback, context: context)
    return VLMHost.shared.submit(body) { kind, payload in
        payload.withUnsafeBytes { raw in
            target.callback(target.context, kind.rawValue, raw.bindMemory(to: UInt8.self).baseAddress, raw.count)
        }
    }
}

@_cdecl("fileid_apple_vlm_cancel")
public func fileid_apple_vlm_cancel(_ handle: UInt64) {
    VLMHost.shared.cancel(handle)
}
