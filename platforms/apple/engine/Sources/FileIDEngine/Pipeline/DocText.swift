// Bounded document text extraction for search, restructure, tagging, and Deep Analyze.
// Uses native PDFKit, system textutil/unzip, and capped plain-text reads. PowerPoint and
// Excel extract the same OOXML text runs as the Windows engine.

import Foundation
import PDFKit

enum DocText {
    private static let maxBytes = 16_384
    private static let procTimeout: TimeInterval = 8
    private static let deepAnalyzeQueue = DispatchQueue(
        label: "com.fileid.deep-analyze.document-text",
        qos: .userInitiated
    )
    private static let deepAnalyzeCircuit = DocumentTextExtractionCircuit()

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

    static func extractForDeepAnalyze(path: String, timeoutSeconds: TimeInterval = 10) async -> String? {
        guard deepAnalyzeCircuit.isOpen else { return nil }
        let state = DocumentTextExtractionState()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                guard state.install(continuation) else { return }
                deepAnalyzeQueue.async {
                    guard state.start() else { return }
                    state.finish(Self.extract(path: path))
                }
                DispatchQueue.global(qos: .userInitiated).asyncAfter(
                    deadline: .now() + max(0, timeoutSeconds)
                ) {
                    if state.finish(nil) {
                        Self.deepAnalyzeCircuit.trip()
                    }
                }
            }
        } onCancel: {
            if state.finish(nil) {
                Self.deepAnalyzeCircuit.trip()
            }
        }
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
            if m.numberOfRanges > 1 {
                parts.append(decodeXMLEntities(ns.substring(with: m.range(at: 1))))
            }
            if parts.count > 4000 { break }
        }
        let joined = parts.joined(separator: " ")
        return joined.isEmpty ? nil : joined
    }

    static func decodeXMLEntities(_ text: String) -> String {
        var result = text
        guard let regex = try? NSRegularExpression(pattern: #"&#(?:x([0-9A-Fa-f]+)|([0-9]+));"#) else {
            return result
        }
        let matches = regex.matches(
            in: result,
            range: NSRange(result.startIndex..., in: result)
        )
        for match in matches.reversed() {
            let source = result as NSString
            let hex = match.range(at: 1).location == NSNotFound
                ? nil : source.substring(with: match.range(at: 1))
            let decimal = match.range(at: 2).location == NSNotFound
                ? nil : source.substring(with: match.range(at: 2))
            let scalar = hex.flatMap { UInt32($0, radix: 16) }
                ?? decimal.flatMap { UInt32($0, radix: 10) }
            guard let scalar, let unicode = UnicodeScalar(scalar),
                  let range = Range(match.range, in: result) else { continue }
            result.replaceSubrange(range, with: String(unicode))
        }
        return result
            .replacingOccurrences(of: "&lt;", with: "<")
            .replacingOccurrences(of: "&gt;", with: ">")
            .replacingOccurrences(of: "&quot;", with: "\"")
            .replacingOccurrences(of: "&apos;", with: "'")
            .replacingOccurrences(of: "&amp;", with: "&")
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

private final class DocumentTextExtractionState: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<String?, Never>?
    private var finished = false
    private var started = false

    func install(_ continuation: CheckedContinuation<String?, Never>) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        guard !finished else {
            continuation.resume(returning: nil)
            return false
        }
        self.continuation = continuation
        return true
    }

    func start() -> Bool {
        lock.withLock {
            guard !finished else { return false }
            started = true
            return true
        }
    }

    @discardableResult
    func finish(_ value: String?) -> Bool {
        lock.lock()
        guard !finished else {
            lock.unlock()
            return false
        }
        finished = true
        let wasStarted = started
        let continuation = continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume(returning: value)
        return wasStarted
    }
}

private final class DocumentTextExtractionCircuit: @unchecked Sendable {
    private let lock = NSLock()
    private var tripped = false

    var isOpen: Bool {
        lock.withLock { !tripped }
    }

    func trip() {
        lock.withLock { tripped = true }
    }
}
