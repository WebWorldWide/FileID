// Stage 5: document text for the scan, as the Swift engine's DocText.extract
// produces it: capped plain-text reads, PDFKit page text, and Word/RTF/
// OpenDocument through the Cocoa text system that /usr/bin/textutil uses, read
// in process (adapters launch no processes). PowerPoint, Excel, EPUB and HTML
// are not handled here; the Rust engine's extractor reads those.
import AppKit
import Foundation
import FileIDImaging
import PDFKit

enum ScanDocuments {
    static let maxBytes = 256 * 1024

    static let plainText: Set<String> = [
        "txt", "md", "markdown", "csv", "tsv", "log", "text", "json", "yaml", "yml", "toml", "xml",
    ]
    /// FileIDShared.FileTypes.code.
    static let code: Set<String> = [
        "swift", "py", "rb", "js", "jsx", "ts", "tsx", "java", "kt", "c", "h", "cpp",
        "cc", "cxx", "hpp", "hh", "cs", "go", "rs", "php", "sh", "bash", "zsh", "sql",
        "scala", "m", "mm", "r", "jl", "lua", "dart", "vue", "pl", "pm", "ps1",
        "tex", "bib", "rst", "org", "adoc",
    ]
    static let cocoaTypes: [String: NSAttributedString.DocumentType] = [
        "docx": .officeOpenXML, "doc": .docFormat, "rtf": .rtf, "rtfd": .rtfd,
        "odt": .openDocument, "wordml": .wordML,
    ]

    /// nil when this adapter does not read the type (the caller extracts it);
    /// .some(nil) when it does and finds no text.
    static func extract(path: String, maxChars: Int) -> String?? {
        let url = URL(fileURLWithPath: path)
        let ext = url.pathExtension.lowercased()
        let raw: String?
        if ["docx", "odt"].contains(ext), !hasZIPHeader(url) { return .some(nil) }
        if plainText.contains(ext) || code.contains(ext) {
            raw = boundedRead(url)
        } else if ext == "pdf" {
            raw = pdfText(url, maxChars: maxChars)
        } else if let type = cocoaTypes[ext] {
            raw = cocoaText(url, type: type)
        } else {
            return nil
        }
        guard let t = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !t.isEmpty else { return .some(nil) }
        return .some(String(t.prefix(maxChars)))
    }

    private static func hasZIPHeader(_ url: URL) -> Bool {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return false }
        defer { try? handle.close() }
        guard let header = try? handle.read(upToCount: 4), header.count == 4 else { return false }
        return [Data([0x50, 0x4b, 0x03, 0x04]), Data([0x50, 0x4b, 0x05, 0x06]), Data([0x50, 0x4b, 0x07, 0x08])].contains(header)
    }

    private static func boundedRead(_ url: URL) -> String? {
        guard let h = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? h.close() }
        guard let data = (try? h.read(upToCount: maxBytes)) ?? nil, !data.isEmpty else { return nil }
        return utf8Text(data)
    }

    /// `DocText.utf8Text`: a character cut by the cap is dropped, other invalid
    /// UTF-8 yields nil.
    static func utf8Text(_ data: Data) -> String? {
        let bytes = [UInt8](data.suffix(4))
        var keep = data.count
        if let lead = bytes.lastIndex(where: { $0 & 0xC0 != 0x80 }), bytes[lead] >= 0xC0 {
            let length = bytes[lead] >= 0xF0 ? 4 : bytes[lead] >= 0xE0 ? 3 : 2
            if bytes.count - lead < length { keep -= bytes.count - lead }
        }
        guard keep > 0 else { return nil }
        return String(data: data.prefix(keep), encoding: .utf8)
    }

    private static func pdfText(_ url: URL, maxChars: Int) -> String? {
        guard let pdf = PDFDocument(url: url) else { return nil }
        var acc = ""
        for i in 0..<pdf.pageCount {
            if acc.count >= maxChars { break }
            if let s = pdf.page(at: i)?.string { acc += s + "\n" }
        }
        return acc.isEmpty ? nil : acc
    }

    /// textutil -convert txt: the document's characters, attachments dropped,
    /// capped like the converter's output read.
    private static func cocoaText(_ url: URL, type: NSAttributedString.DocumentType) -> String? {
        guard let text = try? NSAttributedString(url: url, options: [.documentType: type], documentAttributes: nil) else { return nil }
        let plain = text.string.replacingOccurrences(of: "\u{FFFC}", with: "")
        let data = Data(plain.utf8)
        return utf8Text(data.count > maxBytes ? data.prefix(maxBytes) : data)
    }
}

@_cdecl("fileid_apple_document_text")
public func fileid_apple_document_text(
    _ path: UnsafePointer<CChar>?,
    _ maxChars: Int,
    _ outHandled: UnsafeMutablePointer<Bool>?
) -> UnsafeMutablePointer<CChar>? {
    outHandled?.pointee = false
    guard let path = scanString(path) else { return nil }
    return autoreleasepool {
        guard let result = ScanDocuments.extract(path: path, maxChars: max(1, maxChars)) else { return nil }
        outHandled?.pointee = true
        return result.flatMap { strdup($0) }
    }
}
