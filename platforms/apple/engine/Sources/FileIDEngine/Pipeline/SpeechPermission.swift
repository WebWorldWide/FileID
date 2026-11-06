import Foundation
import Speech

/// Speech recognition permission. macOS kills a process that asks without an
/// `NSSpeechRecognitionUsageDescription` in its bundle (a bare engine binary run
/// outside FileID.app), so asking is skipped there and speech is treated as denied.
enum SpeechPermission {
    static func canAsk(bundle: Bundle = .main) -> Bool {
        (bundle.object(forInfoDictionaryKey: "NSSpeechRecognitionUsageDescription") as? String)?.isEmpty == false
    }

    static func authorized() async -> Bool {
        let status = SFSpeechRecognizer.authorizationStatus()
        if status == .authorized { return true }
        guard status == .notDetermined, canAsk() else { return false }
        return await withCheckedContinuation { (continuation: CheckedContinuation<Bool, Never>) in
            SFSpeechRecognizer.requestAuthorization { continuation.resume(returning: $0 == .authorized) }
        }
    }
}
