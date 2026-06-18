// Document text extraction for the restructure document-content pass (feeds BGETextService).
// macOS-native readers: `textutil` for Office/RTF/HTML, PDFKit for PDF, a direct read for
// plain text. Only the document's beginning matters — BGE tokenizes the first 256 tokens —
// so the output is capped. The Windows engine extracts the same content via `doc_extract`;
// the readers differ per-platform (the same class as the per-platform ML EPs), so a
// doc-heavy library round-trips to a near-identical, not bit-identical, plan.

import Foundation
import PDFKit

enum DocText {
    /// Extract up to `maxChars` of a document's text, or nil if unsupported / empty /
    /// unreadable. Enough text for the BGE 256-token window with margin.
    static func extract(path: String, maxChars: Int = 4000) -> String? {
        let url = URL(fileURLWithPath: path)
        let raw: String?
        switch url.pathExtension.lowercased() {
        case "txt", "md", "markdown", "csv", "log", "text":
            raw = try? String(contentsOf: url, encoding: .utf8)
        case "pdf":
            raw = PDFDocument(url: url)?.string
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

    /// Convert a rich document to plain text via the system `textutil` (handles Word, RTF,
    /// HTML, …). Returns nil on a non-zero exit / unreadable file.
    private static func textutil(_ url: URL) -> String? {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/textutil")
        proc.arguments = ["-convert", "txt", "-stdout", url.path]
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = FileHandle.nullDevice
        do { try proc.run() } catch { return nil }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        proc.waitUntilExit()
        guard proc.terminationStatus == 0 else { return nil }
        return String(data: data, encoding: .utf8)
    }
}
