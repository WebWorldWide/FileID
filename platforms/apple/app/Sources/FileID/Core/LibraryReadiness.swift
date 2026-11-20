import Foundation
import FileIDShared

/// What catalog-backed pages (Chat, Search & Moments) need: at least one scanned file. Both engines answer during a scan, so Chat opens on the first files read.
enum LibraryReadiness: Equatable {
    case needsScan
    case reading
    case ready
    case readyWhileReading

    /// `catalogedFiles` is nil while the count is unknown (not loaded yet, or the reader failed). Unknown is treated as
    /// having files: a library that has files must not flash "Scan a folder" at launch, and the engine can answer even
    /// when the app's reader cannot open the catalog.
    init(catalogedFiles: Int?, scanPhase: ScanPhase?) {
        let reading = [.discovering, .tagging, .postScan].contains(scanPhase)
        switch ((catalogedFiles ?? 1) > 0, reading) {
        case (false, false): self = .needsScan
        case (false, true): self = .reading
        case (true, false): self = .ready
        case (true, true): self = .readyWhileReading
        }
    }

    var hasFiles: Bool { self == .ready || self == .readyWhileReading }

    var chatComposerPlaceholder: String? {
        switch self {
        case .needsScan: return "Scan a folder first"
        case .reading: return "Chat opens when the first files are read"
        case .ready, .readyWhileReading: return nil
        }
    }

    var chatTitle: String {
        self == .reading ? "Reading your files" : "Scan a folder to start chatting"
    }

    func chatMessage(chosenFolder: String?) -> String {
        switch self {
        case .needsScan:
            let lead = "Chat answers questions about the files FileID has scanned."
            if let chosenFolder {
                return "\(lead) FileID hasn’t read \(chosenFolder) yet. Scan it and FileID reads it on this Mac. Nothing is moved or changed."
            }
            return "\(lead) Choose a folder and FileID reads it on this Mac. Nothing is moved or changed."
        case .reading:
            return "FileID is reading your files. Chat opens as soon as the first files are read, and answers cover the files read so far."
        case .ready, .readyWhileReading:
            return ""
        }
    }

    static func scanButtonTitle(chosenFolder: String?) -> String {
        chosenFolder.map { "Scan \($0)" } ?? "Choose a folder to scan"
    }

    var readingNote: String? {
        self == .readyWhileReading ? "FileID is still reading your files. Answers cover the files read so far." : nil
    }
}

/// Decides when to re-count the catalog during a scan so Chat can open on the files read so far.
/// The count itself runs off the main thread; this only limits how often it is asked for.
struct ScanCounterThrottle {
    static let interval: TimeInterval = 2
    private var lastRefresh: Date?

    mutating func shouldRefresh(scanPhase: ScanPhase?, now: Date) -> Bool {
        guard [.discovering, .tagging, .postScan].contains(scanPhase) else {
            lastRefresh = nil
            return false
        }
        if let lastRefresh, now.timeIntervalSince(lastRefresh) < Self.interval { return false }
        lastRefresh = now
        return true
    }
}
