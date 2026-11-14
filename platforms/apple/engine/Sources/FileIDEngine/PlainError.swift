import Foundation
import FileIDShared

/// Errors whose `errorDescription` is already written for the person using FileID.
protocol PlainSpoken: LocalizedError {}

extension StreamingDownloadError: PlainSpoken {}
extension VLMDownloaderError: PlainSpoken {}
extension ResourceScheduler.AdmissionError: PlainSpoken {}

enum PlainError {
    static let tryAgain = "Check your internet connection and free disk space, then try again."

    /// A sentence that is safe to show. Anything not written for people (decoding,
    /// database and runtime errors) becomes `fallback`; callers log the raw error.
    static func reason(_ error: Error, fallback: String = tryAgain) -> String {
        if let spoken = error as? any PlainSpoken, let text = spoken.errorDescription { return text }
        if error is URLError, let text = StreamingDownloadError.underlying(error).errorDescription { return text }
        let ns = error as NSError
        if ns.domain == "FileID.ModelMemoryAdmission" || ns.domain == NSCocoaErrorDomain { return ns.localizedDescription }
        return fallback
    }

    /// Detail for the local log: no sentence, no file path.
    static func logText(_ error: Error) -> String {
        if error is any PlainSpoken { return String(reflecting: error) }
        let ns = error as NSError
        return "\(ns.domain) \(ns.code)"
    }
}

enum EngineVersion {
    /// The app bundle's version (written from `platforms/windows/VERSION` by
    /// `assemble_app.sh`); the literal covers a bare engine binary run outside FileID.app.
    static let current: String = {
        let bundled = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        return bundled.flatMap { $0.isEmpty ? nil : $0 } ?? "0.1.3"
    }()
}
