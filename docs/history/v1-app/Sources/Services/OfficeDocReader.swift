import Foundation
import AppKit

// MARK: - OfficeDocReader

// Extracts searchable text from productivity-document formats.
// - Office zip (.docx/.xlsx/.pptx and legacy .doc/.xls/.ppt tried as zip too):
//   shell to /usr/bin/unzip and SAX-parse the relevant XML parts.
// - OpenDocument (.odt/.ods/.odp): same zip path, targets content.xml.
// - iWork (.pages/.numbers/.key): opportunistic — modern iWork uses binary
//   .iwa protobuf we don't decode; legacy '08 content.xml is extracted if present.
// - RTF/RTFD: NSAttributedString.
// - Plain text (txt/md/csv/json/xml/html/…): raw read, encoding auto-detect.

enum OfficeDocReader {

    static func extractText(from url: URL) -> String {
        let ext = url.pathExtension.lowercased()

        if FileTypes.officeWord.contains(ext) || FileTypes.iWorkPages.contains(ext) {
            let s = extractOfficeWord(from: url, ext: ext)
            if !s.isEmpty { return s }
        }
        if FileTypes.officeSheet.contains(ext) || FileTypes.iWorkNumbers.contains(ext) {
            let s = extractOfficeSheet(from: url, ext: ext)
            if !s.isEmpty { return s }
        }
        if FileTypes.officeSlides.contains(ext) || FileTypes.iWorkKeynote.contains(ext) {
            let s = extractOfficeSlides(from: url, ext: ext)
            if !s.isEmpty { return s }
        }
        if FileTypes.openDocText.contains(ext) { return extractODF(from: url, tag: "p") }
        if FileTypes.openDocSheet.contains(ext) { return extractODF(from: url, tag: "p") }
        if FileTypes.openDocSlides.contains(ext) { return extractODF(from: url, tag: "p") }
        if FileTypes.richText.contains(ext) { return extractRTF(from: url) }
        if FileTypes.plainText.contains(ext) { return extractPlainText(from: url) }
        return ""
    }

    // MARK: - Unzip helper

    private static func withUnzipped(_ url: URL, body: (URL) -> String) -> String {
        let tmp = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: tmp) }

        let proc = Process()
        proc.executableURL = URL(fileURLWithPath: "/usr/bin/unzip")
        proc.arguments     = ["-o", "-q", url.path, "-d", tmp.path]
        proc.standardOutput = FileHandle.nullDevice
        proc.standardError  = FileHandle.nullDevice
        do    { try proc.run(); proc.waitUntilExit() }
        catch { return "" }

        return body(tmp)
    }

    // MARK: - Office (.docx/.xlsx/.pptx) + iWork packages via unzip

    private static func extractOfficeWord(from url: URL, ext: String) -> String {
        withUnzipped(url) { root in
            // Microsoft Word
            let docURL = root.appendingPathComponent("word/document.xml")
            if let data = try? Data(contentsOf: docURL) {
                return xmlText(data, tag: "t").joined(separator: " ").truncated(to: 10_000)
            }
            // iWork Pages '08 legacy — modern Pages uses binary .iwa.
            let iworkURL = root.appendingPathComponent("index.xml")
            if let data = try? Data(contentsOf: iworkURL) {
                return xmlText(data, tag: "p").joined(separator: " ").truncated(to: 10_000)
            }
            return ""
        }
    }

    private static func extractOfficeSheet(from url: URL, ext: String) -> String {
        withUnzipped(url) { root in
            var texts: [String] = []

            let ssURL = root.appendingPathComponent("xl/sharedStrings.xml")
            if let data = try? Data(contentsOf: ssURL) {
                texts += xmlText(data, tag: "t")
            }

            let sheetsDir = root.appendingPathComponent("xl/worksheets")
            if let sheets = try? FileManager.default.contentsOfDirectory(
                at: sheetsDir, includingPropertiesForKeys: nil
            ).filter({ $0.pathExtension == "xml" }) {
                for sheet in sheets {
                    if let data = try? Data(contentsOf: sheet) {
                        texts += xmlText(data, tag: "v")
                    }
                    if texts.joined(separator: " ").count > 10_000 { break }
                }
            }
            if !texts.isEmpty {
                return texts.joined(separator: " ").truncated(to: 10_000)
            }
            // Numbers '08 legacy
            let iworkURL = root.appendingPathComponent("index.xml")
            if let data = try? Data(contentsOf: iworkURL) {
                return xmlText(data, tag: "p").joined(separator: " ").truncated(to: 10_000)
            }
            return ""
        }
    }

    private static func extractOfficeSlides(from url: URL, ext: String) -> String {
        withUnzipped(url) { root in
            let slidesDir = root.appendingPathComponent("ppt/slides")
            if let slides = try? FileManager.default.contentsOfDirectory(
                at: slidesDir, includingPropertiesForKeys: nil
            ).filter({ $0.pathExtension == "xml" })
              .sorted(by: { $0.lastPathComponent < $1.lastPathComponent }),
              !slides.isEmpty {
                var texts: [String] = []
                for slide in slides {
                    if let data = try? Data(contentsOf: slide) { texts += xmlText(data, tag: "t") }
                    if texts.joined(separator: " ").count > 10_000 { break }
                }
                return texts.joined(separator: " ").truncated(to: 10_000)
            }
            // Keynote '08 legacy
            let iworkURL = root.appendingPathComponent("index.apxl")
            if let data = try? Data(contentsOf: iworkURL) {
                return xmlText(data, tag: "span").joined(separator: " ").truncated(to: 10_000)
            }
            return ""
        }
    }

    // MARK: - OpenDocument (.odt/.ods/.odp)

    private static func extractODF(from url: URL, tag: String) -> String {
        withUnzipped(url) { root in
            let contentURL = root.appendingPathComponent("content.xml")
            guard let data = try? Data(contentsOf: contentURL) else { return "" }
            return xmlText(data, tag: tag).joined(separator: " ").truncated(to: 10_000)
        }
    }

    // MARK: - RTF / RTFD

    private static func extractRTF(from url: URL) -> String {
        guard let attr = try? NSAttributedString(
            url: url,
            options: [.documentType: NSAttributedString.DocumentType.rtf],
            documentAttributes: nil
        ) else { return "" }
        return attr.string.truncated(to: 10_000)
    }

    // MARK: - Plain text

    private static func extractPlainText(from url: URL) -> String {
        // Try UTF-8 first (covers the common case), then fall back to encoding
        // auto-detection via NSString for Latin-1 / UTF-16 / etc.
        if let s = try? String(contentsOf: url, encoding: .utf8), !s.isEmpty {
            return s.truncated(to: 10_000)
        }
        var used: UInt = 0
        let ns = try? NSString(
            contentsOfFile: url.path,
            usedEncoding: &used
        )
        return (ns as String?).map { $0.truncated(to: 10_000) } ?? ""
    }

    // MARK: - SAX text extraction

    private static func xmlText(_ data: Data, tag: String) -> [String] {
        let delegate = XMLTextParser(targetTag: tag)
        let parser   = XMLParser(data: data)
        parser.delegate = delegate
        parser.parse()
        return delegate.texts
    }
}

// MARK: - Parser delegate

private final class XMLTextParser: NSObject, XMLParserDelegate {
    let targetTag: String
    var texts:     [String] = []
    private var inTarget    = false
    private var current     = ""

    init(targetTag: String) { self.targetTag = targetTag }

    func parser(_ parser: XMLParser, didStartElement name: String,
                namespaceURI: String?, qualifiedName: String?, attributes: [String: String]) {
        // Match bare tag name and any namespace-prefixed variant (e.g. "w:t", "text:p").
        inTarget = name == targetTag || name.hasSuffix(":\(targetTag)")
        if inTarget { current = "" }
    }

    func parser(_ parser: XMLParser, foundCharacters string: String) {
        if inTarget { current += string }
    }

    func parser(_ parser: XMLParser, didEndElement name: String,
                namespaceURI: String?, qualifiedName: String?) {
        let matches = name == targetTag || name.hasSuffix(":\(targetTag)")
        if inTarget && matches {
            let s = current.trimmingCharacters(in: .whitespacesAndNewlines)
            if !s.isEmpty { texts.append(s) }
            inTarget = false
        }
    }
}

private extension String {
    func truncated(to length: Int) -> String {
        count > length ? String(prefix(length)) : self
    }
}
