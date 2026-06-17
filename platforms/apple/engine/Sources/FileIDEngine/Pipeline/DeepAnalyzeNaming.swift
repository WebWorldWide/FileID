// Deep Analyze metadata naming for non-rasterizable kinds (audio + 3D models).
//
// The VLM path needs a raster image; audio and `.obj` have none, but they carry their
// OWN descriptive metadata — audio embeds title/artist/album tags; a `.obj` embeds the
// modeler's object/group/material names. We name them from that: no VLM, no new model.
// The pure name-builders are byte-faithful with the Rust engine (deep_analyze.rs) so
// the same file gets the same name on either platform. (Video already works via the
// keyframe→VLM path; true AI audio/3D content understanding — Whisper/YAMNet, a 3D
// renderer→VLM — needs a new model, a future MODELS.md decision.)
import Foundation
import AVFoundation
import FileIDShared

enum DeepAnalyzeNaming {

    // MARK: - Pure name-builders (lockstep with Rust build_audio_name / build_obj_name)

    /// "Artist - Title" when both are present, else the title alone, else nil (artist-
    /// only isn't descriptive enough). Case-preserving + filesystem-safe.
    static func buildAudioName(title: String?, artist: String?) -> String? {
        let t = title?.trimmingCharacters(in: .whitespacesAndNewlines)
        let a = artist?.trimmingCharacters(in: .whitespacesAndNewlines)
        let titleOK = (t?.isEmpty == false) ? t : nil
        let artistOK = (a?.isEmpty == false) ? a : nil
        let raw: String
        switch (artistOK, titleOK) {
        case let (artist?, title?): raw = "\(artist) - \(title)"
        case let (nil, title?): raw = title
        default: return nil
        }
        let safe = FilesystemNameSafe.componentSafe(raw)
        return (safe.isEmpty || safe == "_") ? nil : safe
    }

    /// First meaningful object/group name, else first meaningful material, else nil.
    static func buildObjName(objects: [String], materials: [String]) -> String? {
        func pick(_ names: [String]) -> String? {
            names.map { $0.trimmingCharacters(in: .whitespaces) }.first { isMeaningfulModelName($0) }
        }
        guard let raw = pick(objects) ?? pick(materials) else { return nil }
        let safe = FilesystemNameSafe.componentSafe(raw)
        return (safe.isEmpty || safe == "_") ? nil : safe
    }

    /// A 3D object/material name that carries content signal — not a tool placeholder.
    static func isMeaningfulModelName(_ raw: String) -> Bool {
        let s = raw.trimmingCharacters(in: .whitespaces)
        if s.unicodeScalars.count < 2 { return false }
        if !s.contains(where: { $0.isLetter }) { return false } // pure numbers/punctuation
        let generic: Set<String> = [
            "default", "defaultobject", "none", "object", "obj", "mesh", "group",
            "model", "polysurface", "material", "untitled", "cube", "plane", "scene",
            "sphere", "cylinder", "node", "geometry", "shape",
        ]
        let lower = s.lowercased()
        for g in generic {
            if lower == g { return false }
            if lower.hasPrefix(g) {
                let rest = lower.dropFirst(g.count)
                if !rest.contains(where: { $0.isLetter }) { return false } // generic + numeric suffix
            }
        }
        return true
    }

    static func pushUnique(_ v: inout [String], _ s: String) {
        let t = s.trimmingCharacters(in: .whitespaces)
        if !t.isEmpty && !v.contains(t) { v.append(t) }
    }

    /// Caption for an audio file's metadata.
    static func audioDescription(title: String?, artist: String?, album: String?) -> String? {
        var parts: [String] = []
        if let t = title?.trimmingCharacters(in: .whitespaces), !t.isEmpty { parts.append("\u{201C}\(t)\u{201D}") }
        if let a = artist?.trimmingCharacters(in: .whitespaces), !a.isEmpty { parts.append("by \(a)") }
        if let al = album?.trimmingCharacters(in: .whitespaces), !al.isEmpty { parts.append("from \u{201C}\(al)\u{201D}") }
        return parts.isEmpty ? nil : "Audio: \(parts.joined(separator: " "))"
    }

    /// Caption for a 3D model's embedded names.
    static func objDescription(objects: [String], materials: [String]) -> String? {
        if objects.isEmpty && materials.isEmpty { return nil }
        var parts: [String] = []
        if !objects.isEmpty { parts.append("objects: \(objects.prefix(4).joined(separator: ", "))") }
        if !materials.isEmpty { parts.append("materials: \(materials.prefix(4).joined(separator: ", "))") }
        return "3D model \u{2014} \(parts.joined(separator: "; "))"
    }

    // MARK: - Parsing / extraction

    /// Scan a Wavefront `.obj` (+ its `.mtl`) for object/group + material names. Bounded
    /// read (the names live in the header/early body; we only need the distinct set) so
    /// a multi-GB mesh can't OOM. Order-preserving, deduped.
    static func parseObjNames(url: URL) -> (objects: [String], materials: [String]) {
        var objects: [String] = []
        var materials: [String] = []
        var mtllib: String?

        if let content = boundedText(url) {
            for line in content.split(separator: "\n", omittingEmptySubsequences: false) {
                let t = line.trimmingCharacters(in: .whitespaces)
                if t.hasPrefix("o ") || t.hasPrefix("g ") {
                    pushUnique(&objects, String(t.dropFirst(2)))
                } else if t.hasPrefix("usemtl ") {
                    pushUnique(&materials, String(t.dropFirst(7)))
                } else if t.hasPrefix("mtllib "), mtllib == nil {
                    mtllib = String(t.dropFirst(7)).trimmingCharacters(in: .whitespaces)
                }
            }
        }
        if let mtl = mtllib, !mtl.isEmpty {
            let mtlURL = url.deletingLastPathComponent().appendingPathComponent(mtl)
            if let content = boundedText(mtlURL) {
                for line in content.split(separator: "\n", omittingEmptySubsequences: false) {
                    let t = line.trimmingCharacters(in: .whitespaces)
                    if t.hasPrefix("newmtl ") { pushUnique(&materials, String(t.dropFirst(7))) }
                }
            }
        }
        return (objects, materials)
    }

    /// Read at most 8 MiB of a UTF-8 text file (a truncated tail just drops the last
    /// partial line). nil on open/decode failure.
    private static func boundedText(_ url: URL) -> String? {
        let maxBytes = 8 * 1024 * 1024
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        let data = (try? handle.read(upToCount: maxBytes)) ?? Data()
        return String(data: data, encoding: .utf8)
    }

    /// Embedded title/artist/album via AVFoundation common metadata (mp3 ID3, ogg/flac
    /// Vorbis, m4a, …). First-non-empty per field; best-effort.
    static func extractAudioTags(url: URL) async -> (title: String?, artist: String?, album: String?) {
        let asset = AVURLAsset(url: url)
        guard let items = try? await asset.load(.commonMetadata) else { return (nil, nil, nil) }
        var title: String?
        var artist: String?
        var album: String?
        for item in items {
            guard let key = item.commonKey else { continue }
            guard let v = (try? await item.load(.stringValue))?
                .trimmingCharacters(in: .whitespacesAndNewlines), !v.isEmpty else { continue }
            switch key {
            case .commonKeyTitle: if title == nil { title = v }
            case .commonKeyArtist: if artist == nil { artist = v }
            case .commonKeyAlbumName: if album == nil { album = v }
            default: break
            }
        }
        return (title, artist, album)
    }

    /// Produce a Deep Analyze result for a non-rasterizable kind from its metadata.
    /// Returns an empty (but non-failure) result for a metadata-less file. Mirrors the
    /// Rust `analyze_metadata_named_file`.
    static func metadataResult(url: URL, kind: DiscoveredFile.Kind) async -> DeepAnalyze.AnalysisResult {
        switch kind {
        case .audio:
            let (title, artist, album) = await extractAudioTags(url: url)
            var tags: [String] = []
            if let a = artist { pushUnique(&tags, a) }
            if let al = album { pushUnique(&tags, al) }
            return DeepAnalyze.AnalysisResult(
                description: audioDescription(title: title, artist: artist, album: album) ?? "",
                proposedName: buildAudioName(title: title, artist: artist),
                tags: tags)
        case .model:
            let (objects, materials) = parseObjNames(url: url)
            var tags: [String] = []
            for t in objects + materials { pushUnique(&tags, t) }
            if tags.count > 6 { tags = Array(tags.prefix(6)) }
            return DeepAnalyze.AnalysisResult(
                description: objDescription(objects: objects, materials: materials) ?? "",
                proposedName: buildObjName(objects: objects, materials: materials),
                tags: tags)
        default:
            return DeepAnalyze.AnalysisResult(description: "", proposedName: nil)
        }
    }
}
