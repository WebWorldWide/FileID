// Deep Analyze's text transcription: when the scan flagged an image (or an
// image-only PDF) as text-bearing but its OCR is missing or mangled, the loaded
// VLM transcribes it into ocr_text with source 'vlm'. Mirrors the Rust engine's
// `pipeline/text_reading.rs` transcription path.
import Foundation
import CoreGraphics
import GRDB
import FileIDShared

extension DeepAnalyzeRunner {
    static func transcriptionCandidate(_ db: GRDB.Database, fileID: Int64) throws -> Bool {
        guard let row = try Row.fetchOne(db, sql: """
            SELECT f.kind, o.text AS ocr_text, o.source AS ocr_source,
                   EXISTS(SELECT 1 FROM doc_text d WHERE d.file_id=f.id AND length(trim(d.text))>0) AS has_doc
            FROM files f LEFT JOIN ocr_text o ON o.file_id=f.id
            WHERE f.id=? AND f.failed=0
            """, arguments: [fileID]) else { return false }
        let tags = try String.fetchAll(db, sql: """
            SELECT tag FROM tags WHERE file_id=? AND source IN ('auto','user','vlm')
            """, arguments: [fileID])
        let hasDoc: Bool = row["has_doc"]
        return TextReading.needsTranscription(
            kind: row["kind"], tags: tags, ocrText: row["ocr_text"], ocrSource: row["ocr_source"], hasDocText: hasDoc
        )
    }

    static func storeTranscription(_ db: GRDB.Database, fileID: Int64, text: String, handwritten: Bool) throws {
        try db.execute(sql: "DELETE FROM ocr_text WHERE file_id = ?", arguments: [fileID])
        try db.execute(sql: """
            INSERT INTO ocr_text (file_id, text, source, handwritten) VALUES (?, ?, 'vlm', ?)
            """, arguments: [fileID, text, handwritten ? 1 : 0])
        try db.execute(sql: "UPDATE files SET has_text = 1 WHERE id = ?", arguments: [fileID])
    }

    /// Runs after the file's caption persisted. Failures only log: the caption
    /// already succeeded, and the next Deep Analyze run retries the transcription.
    static func transcribeTextIfNeeded(
        analyzer: DeepAnalyze,
        database: Database,
        fileID: Int64,
        url: URL,
        kind: DiscoveredFile.Kind
    ) async {
        guard kind == .image || kind == .pdf,
              (try? await database.pool.read({ try transcriptionCandidate($0, fileID: fileID) })) == true else { return }
        let image: CGImage? = kind == .pdf
            ? DeepAnalyze.quickLookThumbnail(url: url, maxPixelSize: 1536)
            : await DeepAnalyze.loadCGImage(url: url, maxPixelSize: 1536)
        guard let image else { return }
        let started = CFAbsoluteTimeGetCurrent()
        let raw: String
        do {
            raw = try await analyzer.transcribeText(image)
        } catch {
            JSONLog.shared.warn(ev: "deep_analyze_transcribe_failed", error: "\(error)")
            return
        }
        guard let parsed = TextReading.parseTranscription(raw) else { return }
        do {
            try await database.pool.write { db in
                try storeTranscription(db, fileID: fileID, text: parsed.text, handwritten: parsed.handwritten)
            }
            JSONLog.shared.info(ev: "deep_analyze_transcribed", extra: [
                "ms": AnyCodable((CFAbsoluteTimeGetCurrent() - started) * 1000),
                "handwritten": AnyCodable(parsed.handwritten),
            ])
        } catch {
            JSONLog.shared.warn(ev: "deep_analyze_transcribe_persist_failed", error: "\(error)")
        }
    }
}
