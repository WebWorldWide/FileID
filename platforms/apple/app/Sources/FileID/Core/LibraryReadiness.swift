import FileIDShared

/// What pages that answer from the catalog (Chat, Search & Moments) can do given how much of the library FileID has read; they need
/// at least one scanned file. Both engines answer chat requests while a scan runs, from the files read so far.
enum LibraryReadiness: Equatable {
    case needsScan
    case reading
    case ready
    case readyWhileReading

    init(catalogedFiles: Int, scanPhase: ScanPhase?) {
        let reading = [.discovering, .tagging, .postScan].contains(scanPhase)
        switch (catalogedFiles > 0, reading) {
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
        case .reading: return "Waiting for the first files…"
        case .ready, .readyWhileReading: return nil
        }
    }

    var chatTitle: String {
        self == .reading ? "Reading your files" : "Scan a folder to start chatting"
    }

    var chatMessage: String {
        switch self {
        case .needsScan:
            return "Chat answers questions about the files FileID has scanned. Choose a folder and FileID reads it on this Mac. Nothing is moved or changed."
        case .reading:
            return "Chat opens as soon as FileID has read the first files, and answers cover the files read so far."
        case .ready, .readyWhileReading:
            return ""
        }
    }

    var readingNote: String? {
        self == .readyWhileReading ? "FileID is still reading your files. Answers cover the files read so far." : nil
    }
}
