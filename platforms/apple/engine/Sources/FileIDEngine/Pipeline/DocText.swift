// Document text extraction for the restructure document-content pass (feeds BGETextService).
// macOS-native readers: `textutil` for Word/RTF/HTML, `unzip` + tag-extraction for OOXML
// presentations/spreadsheets (pptx/xlsx — textutil can't read those), PDFKit for PDF, a
// bounded read for plain text. Only the document's beginning matters — BGE tokenizes the
// first 256 tokens — so every reader is BOUNDED in time and size: a stuck file (unresponsive
// mount, zip-bomb, locked file) must never hang the plan, and a giant document must never
// OOM it. The Windows engine extracts the same content via `doc_extract` (which mines the
// same `a:t`/`t` runs from pptx/xlsx); the readers differ per-platform, so a doc-heavy
// library round-trips to a near-identical, not bit-identical, plan.

import Foundation
import PDFKit

enum DocText {
    private static let maxBytes = 16_384
    private static let procTimeout: TimeInterval = 8

    /// Extract up to `maxChars` of a document's text, or nil if unsupported / empty /
    /// unreadable. Bounded in time + size.
    static func extract(path: String, maxChars: Int = 4000) -> String? {
        let url = URL(fileURLWithPath: path)
        let ext = url.pathExtension.lowercased()
        let raw: String?
        switch ext {
        case "txt", "md", "markdown", "csv", "log", "text":
            raw = boundedRead(url)
        case "pdf":
            raw = pdfText(url, maxChars: maxChars)
        case "docx", "doc", "rtf", "rtfd", "html", "htm", "odt", "wordml":
            raw = textutil(url)
        case "pptx":
            raw = officeXML(url, member: "ppt/slides/slide*.xml", tag: "a:t")
        case "xlsx":
            raw = officeXML(url, member: "xl/sharedStrings.xml", tag: "t")
        case "epub":
            raw = epubText(url)
        default:
            // Source code + prose markup → read as UTF-8 (BGE clusters by content).
            raw = FileTypes.code.contains(ext) ? boundedRead(url) : nil
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

    /// Extract PDF text page-by-page, stopping once `maxChars` is reached.
    private static func pdfText(_ url: URL, maxChars: Int) -> String? {
        guard let pdf = PDFDocument(url: url) else { return nil }
        var acc = ""
        for i in 0..<pdf.pageCount {
            if acc.count >= maxChars { break }
            if let s = pdf.page(at: i)?.string { acc += s + "\n" }
        }
        return acc.isEmpty ? nil : acc
    }

    /// Word/RTF/HTML → plain text via the system `textutil`.
    private static func textutil(_ url: URL) -> String? {
        guard let data = runBounded("/usr/bin/textutil", ["-convert", "txt", "-stdout", url.path]) else {
            return nil
        }
        return String(data: data, encoding: .utf8)
    }

    /// pptx/xlsx → the text inside `<tag>…</tag>` runs of a zipped OOXML member, via
    /// `unzip -p`. Mirrors the Windows `doc_extract` (which reads the same `a:t`/`t` runs).
    private static func officeXML(_ url: URL, member: String, tag: String) -> String? {
        guard let data = runBounded("/usr/bin/unzip", ["-p", url.path, member]),
              let xml = String(data: data, encoding: .utf8) else { return nil }
        // Pull the text content of each <tag ...>…</tag>; cheap regex is fine for a snippet.
        guard let re = try? NSRegularExpression(pattern: "<\(tag)[^>]*>([^<]*)</\(tag)>") else { return nil }
        let ns = xml as NSString
        var parts: [String] = []
        for m in re.matches(in: xml, range: NSRange(location: 0, length: ns.length)) {
            if m.numberOfRanges > 1 { parts.append(ns.substring(with: m.range(at: 1))) }
            if parts.count > 4000 { break }
        }
        let joined = parts.joined(separator: " ")
        return joined.isEmpty ? nil : joined
    }

    /// EPUB → text: an EPUB is a zip of XHTML, so concatenate the content members (`unzip`'s
    /// member glob pulls them in one shot, bounded by runBounded) and strip the tags. Only
    /// the first ~256 tokens reach BGE, so the 16 KB read cap is plenty. Mirrors the Windows
    /// `doc_extract` EPUB path.
    private static func epubText(_ url: URL) -> String? {
        guard let data = runBounded("/usr/bin/unzip", ["-p", url.path, "*.xhtml", "*.html", "*.htm"]),
              let html = String(data: data, encoding: .utf8) else { return nil }
        let stripped = html.replacingOccurrences(of: "<[^>]+>", with: " ", options: .regularExpression)
        let collapsed = stripped.replacingOccurrences(of: "\\s+", with: " ", options: .regularExpression)
        let trimmed = collapsed.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    /// Run a converter subprocess with a watchdog (terminates a stuck child) + a `maxBytes`
    /// read cap, so neither a hang nor a pathologically large output can wedge/OOM the plan.
    private static func runBounded(_ exe: String, _ args: [String]) -> Data? {
        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: exe)
        proc.arguments = args
        let pipe = Pipe()
        proc.standardOutput = pipe
        proc.standardError = FileHandle.nullDevice
        do { try proc.run() } catch { return nil }
        let killer = DispatchWorkItem { if proc.isRunning { proc.terminate() } }
        DispatchQueue.global().asyncAfter(deadline: .now() + procTimeout, execute: killer)
        let handle = pipe.fileHandleForReading
        var data = Data()
        while data.count < maxBytes {
            let chunk = handle.availableData     // empty at EOF (incl. after terminate)
            if chunk.isEmpty { break }
            data.append(chunk)
        }
        try? handle.close()
        killer.cancel()
        proc.terminate()
        proc.waitUntilExit()
        return data.isEmpty ? nil : data.prefix(maxBytes)
    }
}
