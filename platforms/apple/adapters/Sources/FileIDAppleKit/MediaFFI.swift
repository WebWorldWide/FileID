// Stage 8 C entry points: AVFoundation probes, exports and frames, and Apple
// Speech, for the Rust engine's tools and timeline. Declared in the
// "Stage 8" block of adapters/include/fileid_apple.h and src/apple/ffi.rs.
// No entry point opens a URL, socket or HTTP client or launches a process.
import Foundation

@_cdecl("fileid_apple_media_probe")
public func fileid_apple_media_probe(_ path: UnsafePointer<CChar>?, _ mimeType: UnsafePointer<CChar>?, _ strict: Bool) -> UnsafeMutablePointer<CChar>? {
    guard let path = MediaBridge.string(path) else { return MediaBridge.failure(MediaFailure("Invalid media path.")) }
    let mime = MediaBridge.string(mimeType)
    do {
        let probe = try MediaBridge.wait { try await MediaInspect.probe(URL(fileURLWithPath: path), mimeType: mime, strict: strict) }
        return MediaBridge.reply(probe.json)
    } catch {
        return MediaBridge.failure(error)
    }
}

@_cdecl("fileid_apple_media_export")
public func fileid_apple_media_export(_ request: UnsafePointer<CChar>?, _ cancel: UnsafePointer<Bool>?) -> UnsafeMutablePointer<CChar>? {
    let flag = CancelFlag(cancel)
    do {
        guard let text = MediaBridge.string(request),
              let object = try JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
              let recipeObject = object["recipe"] as? [String: Any], let source = object["source"] as? String,
              source.hasPrefix("/") else { throw MediaFailure("Invalid media export request.") }
        let recipe = try MediaRecipe(recipeObject)
        let output = (object["output"] as? String).map { URL(fileURLWithPath: $0) }
        let sourceURL = URL(fileURLWithPath: source)
        if recipe.kind == "video" {
            let converted = try MediaBridge.wait { try await MediaExport.video(source: sourceURL, output: output, recipe: recipe, cancel: flag) }
            return MediaBridge.reply(["probe": converted.probe.json, "summary": converted.summary])
        }
        guard let output else { throw MediaFailure("Invalid media export request.") }
        let before = try MediaBridge.wait { try await MediaExport.common(source: sourceURL, output: output, recipe: recipe, cancel: flag) }
        return MediaBridge.reply(["probe": before.json])
    } catch {
        return MediaBridge.failure(error)
    }
}

@_cdecl("fileid_apple_video_frame")
public func fileid_apple_video_frame(
    _ path: UnsafePointer<CChar>?, _ seconds: Double, _ width: UInt32, _ height: UInt32,
    _ outSeconds: UnsafeMutablePointer<Double>?
) -> UnsafeMutablePointer<UInt8>? {
    outSeconds?.pointee = .nan
    guard let path = MediaBridge.string(path) else { return nil }
    guard let frame = try? MediaBridge.wait({
        try await MediaFrames.frame(source: URL(fileURLWithPath: path), seconds: seconds, width: Int(width), height: Int(height))
    }) else { return nil }
    outSeconds?.pointee = frame.seconds
    return MediaBridge.copy(frame.rgb)
}

@_cdecl("fileid_apple_video_pool")
public func fileid_apple_video_pool(
    _ path: UnsafePointer<CChar>?, _ interval: Double, _ maxSide: UInt32, _ directory: UnsafePointer<CChar>?,
    _ cancel: UnsafePointer<Bool>?
) -> Int32 {
    guard let path = MediaBridge.string(path), let directory = MediaBridge.string(directory) else { return MediaFrames.PoolOutcome.invalid.rawValue }
    let flag = CancelFlag(cancel)
    let outcome = (try? MediaBridge.wait {
        await MediaFrames.pool(source: URL(fileURLWithPath: path), interval: interval, maxPixel: Int(maxSide),
                               directory: URL(fileURLWithPath: directory, isDirectory: true), cancel: flag)
    }) ?? .failed
    return outcome.rawValue
}

@_cdecl("fileid_apple_speech_status")
public func fileid_apple_speech_status(_ ask: Bool) -> Int32 {
    ((try? MediaBridge.wait { await MediaSpeech.status(ask: ask) }) ?? .unavailable).rawValue
}

@_cdecl("fileid_apple_speech_model_version")
public func fileid_apple_speech_model_version() -> UnsafeMutablePointer<CChar>? {
    strdup(MediaSpeech.modelVersion)
}

@_cdecl("fileid_apple_speech_transcribe")
public func fileid_apple_speech_transcribe(
    _ path: UnsafePointer<CChar>?, _ start: Double, _ end: Double, _ cancel: UnsafePointer<Bool>?
) -> UnsafeMutablePointer<CChar>? {
    guard let path = MediaBridge.string(path) else { return MediaBridge.failure(MediaFailure("Invalid media path.")) }
    let flag = CancelFlag(cancel)
    do {
        let segments = try MediaBridge.wait { try await MediaSpeech.transcribe(source: URL(fileURLWithPath: path), start: start, end: end, cancel: flag) }
        return MediaBridge.reply(["segments": segments.map {
            ["text": $0.text, "timestamp": $0.timestamp, "duration": $0.duration, "confidence": $0.confidence] as [String: Any]
        }])
    } catch {
        return MediaBridge.failure(error)
    }
}
