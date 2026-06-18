// Document text extraction for the restructure document-content pass (feeds BGETextService).
// macOS-native readers: `textutil` for Office/RTF/HTML, PDFKit for PDF, a bounded read for
// plain text. Only the document's beginning matters — BGE tokenizes the first 256 tokens —
// so every reader is BOUNDED in both time and size: a stuck file (unresponsive network
// mount, a zip-bomb .docx, a locked file) must never hang the plan, and a pathologically
// large document must never OOM it. The Windows engine extracts the same content via
// `doc_extract`; the readers differ per-platform (the same class as the per-platform ML
// EPs / its WordPiece grapheme handling), so a doc-heavy library round-trips to a near-
// identical, not bit-identical, plan.

import Foundation
import PDFKit

enum DocText {
    /// Cap the bytes any reader materializes. 16 KB comfortably covers BGE's 256-token
    /// window (the caller further trims to `maxChars`) while bounding memory.
    private static let maxBytes = 16_384
    /// Hard wall on a single `textutil` invocation so one stuck file can't hang the plan.
    private static let textutilTimeout: TimeInterval = 8

    /// Extract up to `maxChars` of a document's text, or nil if unsupported / empty /
    /// unreadable. Bounded in time + size.
    static func extract(path: String, maxChars: Int = 4000) -> String? {
        let url = URL(fileURLWithPath: path)
        let raw: String?
        switch url.pathExtension.lowercased() {
        case "txt", "md", "markdown", "csv", "log", "text":
            raw = boundedRead(url)
        case "pdf":
            raw = pdfText(url, maxChars: maxChars)
        case "docx", "doc", "rtf", "rtfd", "html", "htm", "odt", "wordml":
            raw = textutil(url)
        default:
            raw = nil
        }
        guard let t = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty else {
            return nil
        }
        return String(t.prefix(maxChars))
    }

    /// Read at most `maxBytes` of a plain-text file (a multi-GB log can't OOM the plan).
    private static func boundedRead(_ url: URL) -> String? {
        guard let h = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? h.close() }
        guard let data = (try? h.read(upToCount: maxBytes)) ?? nil, !data.isEmpty else { return nil }
        return String(data: data, encoding: .utf8)
    }

    /// Extract PDF text page-by-page, stopping once `maxChars` is reached — never
    /// materializes a whole large PDF's text.
    private static func pdfText(_ url: URL, maxChars: Int) -> String? {
        guard let pdf = PDFDocument(url: url) else { return nil }
        var acc = ""
        for i in 0..<pdf.pageCount {
            if acc.count >= maxChars { break }
            if let s = pdf.page(at: i)?.string { acc += s + "\n" }
        }
        return acc.isEmpty ? nil : acc
    }

    /// Convert a rich document to plain text via the system `textutil`. Bounded by a
    /// watchdog that terminates a stuck conversion and by a `maxBytes` read cap.
    private static func textutil(_ url: URL) -> String? {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/textutil")
        proc.arguments = ["-convert", "txt", "-stdout", url.path]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = FileHandle.nullDevice
        do { try proc.run() } catch { return nil }

        // Watchdog: SIGTERM a textutil that hasn't finished in time, which closes the pipe
        // and unblocks the read below.
        let killer = DispatchWorkItem { if proc.isRunning { proc.terminate() } }
        DispatchQueue.global().asyncAfter(deadline: .now() + textutilTimeout, execute: killer)

        let handle = pipe.fileHandleForReading
        var data = Data()
        while data.count < maxBytes {
            let chunk = handle.availableData  // returns empty at EOF (incl. after terminate)
            if chunk.isEmpty { break }
            data.append(chunk)
        }
        try? handle.close()      // SIGPIPE the child if it's still writing past our cap
        killer.cancel()
        proc.terminate()         // no-op if already exited
        proc.waitUntilExit()
        guard !data.isEmpty else { return nil }
        return String(data: data.prefix(maxBytes), encoding: .utf8)
    }
}
