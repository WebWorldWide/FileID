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
import Speech
import SoundAnalysis
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
            var name = buildAudioName(title: title, artist: artist)
            var desc = audioDescription(title: title, artist: artist, album: album)
            // No descriptive title (a voice memo / podcast / lecture) → transcribe the
            // speech on-device (Apple Speech) and name from the spoken content. Mirrors
            // the Windows whisper.cpp path. Best-effort — any failure keeps the metadata
            // result (so audio naming never regresses).
            if name == nil, let transcript = await transcribeAudio(url: url),
               let named = nameFromTranscript(transcript) {
                name = named.name
                if desc == nil { desc = named.description }
            }
            // Still unnamed (no metadata title, no speech) → classify the dominant SOUND
            // on-device (Apple SoundAnalysis — the macOS analogue of YAMNet) and name from
            // it: a field recording of rain → "Rain", a dog bark → "Dog Bark". Best-effort;
            // generic labels (speech/music/noise) and weak guesses keep the original name.
            if name == nil, let sound = await classifySound(url: url) {
                name = sound.name
                if desc == nil { desc = sound.description }
                pushUnique(&tags, sound.name)
            }
            return DeepAnalyze.AnalysisResult(
                description: desc ?? "",
                proposedName: name,
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

    // MARK: - Audio transcription (Apple Speech — on-device; mirrors Windows whisper.cpp)

    /// First ~8 transcript words → a filesystem-safe name; the lead 200 chars → a caption.
    /// Lockstep with the Rust `name_from_transcript`. nil for an empty/degenerate transcript.
    static func nameFromTranscript(_ transcript: String) -> (name: String, description: String)? {
        let t = transcript.trimmingCharacters(in: .whitespacesAndNewlines)
        if t.isEmpty { return nil }
        let raw = t.split(whereSeparator: { $0.isWhitespace }).prefix(8).joined(separator: " ")
        let name = FilesystemNameSafe.componentSafe(raw)
        if name.isEmpty || name == "_" { return nil }
        let snippet = String(t.prefix(200))
        return (name, "Audio transcript: \(snippet)")
    }

    /// Transcribe an audio file on-device via Apple's Speech framework — the macOS-native
    /// mirror of the Windows whisper.cpp subprocess (no model download, no cloud).
    /// `requiresOnDeviceRecognition` keeps the audio on the machine. Best-effort: returns
    /// nil on no-authorization / no on-device support / unsupported container / recognition
    /// error, so the caller falls back to metadata naming and audio naming never regresses.
    static func transcribeAudio(url: URL) async -> String? {
        guard await requestSpeechAuthorization() else { return nil }
        guard let recognizer = SFSpeechRecognizer(), recognizer.isAvailable,
              recognizer.supportsOnDeviceRecognition else { return nil }
        // Race against a 30-second timeout. SFSpeechRecognizer may never deliver isFinal
        // for silence-only files, unsupported codecs, or brief recordings — without a
        // timeout the withCheckedContinuation parks forever, stalling all subsequent
        // Deep Analyze work on the same serial queue.
        return await withTaskGroup(of: String?.self) { group in
            group.addTask {
                let request = SFSpeechURLRecognitionRequest(url: url)
                request.requiresOnDeviceRecognition = true
                request.shouldReportPartialResults = false
                let once = OnceFlag()
                return await withCheckedContinuation { (cont: CheckedContinuation<String?, Never>) in
                    recognizer.recognitionTask(with: request) { result, error in
                        if let result, result.isFinal {
                            if once.claim() { cont.resume(returning: result.bestTranscription.formattedString) }
                        } else if error != nil {
                            if once.claim() { cont.resume(returning: nil) }
                        }
                    }
                }
            }
            group.addTask {
                try? await Task.sleep(nanoseconds: 30_000_000_000)
                return nil
            }
            let first = await group.next() ?? nil
            group.cancelAll()
            return first ?? nil
        }
    }

    /// Request Speech authorization once; true only when fully authorized (a CLI engine
    /// with no TCC grant resolves to denied → the metadata fallback).
    private static func requestSpeechAuthorization() async -> Bool {
        if SFSpeechRecognizer.authorizationStatus() == .authorized { return true }
        return await withCheckedContinuation { (cont: CheckedContinuation<Bool, Never>) in
            SFSpeechRecognizer.requestAuthorization { cont.resume(returning: $0 == .authorized) }
        }
    }

    // MARK: - Sound-event classification (Apple SoundAnalysis — the macOS analogue of YAMNet)

    /// Lowercased SoundAnalysis class identifiers that carry no useful filename signal:
    /// speech/music are already handled by transcription + embedded tags, and the rest are
    /// non-descriptive. A file landing on one of these keeps its original name.
    private static let genericSoundLabels: Set<String> = [
        "speech", "music", "silence", "noise", "white_noise", "background_noise",
        "ambient_noise", "sound", "audio", "static", "humming",
    ]

    /// Humanize a SoundAnalysis class identifier ("dog_bark") into a descriptive,
    /// filesystem-safe name ("Dog Bark") + a caption, or nil for a generic/empty label.
    /// Pure + unit-tested (the framework call is runtime-verified on-device).
    static func nameFromSoundLabel(_ identifier: String) -> (name: String, description: String)? {
        let id = identifier.lowercased()
        if genericSoundLabels.contains(id) { return nil }
        let humanized = id
            .split(whereSeparator: { $0 == "_" || $0 == "-" || $0 == "." })
            .map { $0.prefix(1).uppercased() + $0.dropFirst() }
            .joined(separator: " ")
            .trimmingCharacters(in: .whitespaces)
        if humanized.isEmpty { return nil }
        let safe = FilesystemNameSafe.componentSafe(humanized)
        if safe.isEmpty || safe == "_" { return nil }
        return (safe, "Detected sound: \(humanized)")
    }

    /// Classify the dominant sound in an audio file on-device (no model download, no
    /// cloud) and name from it. Best-effort: nil on an unsupported container, an
    /// unavailable classifier, or a low-confidence / generic result → metadata fallback.
    static func classifySound(url: URL) async -> (name: String, description: String)? {
        guard let label = await dominantSoundIdentifier(url: url) else { return nil }
        return nameFromSoundLabel(label)
    }

    /// Run SoundAnalysis' built-in classifier over the file and return the highest-
    /// confidence class identifier seen across the clip (≥ a minimum confidence), or nil.
    private static func dominantSoundIdentifier(url: URL) async -> String? {
        await withCheckedContinuation { (cont: CheckedContinuation<String?, Never>) in
            // SNAudioFileAnalyzer.analyze() is synchronous (delivers results to the
            // observer on the calling thread), so run it off the executor.
            DispatchQueue.global(qos: .userInitiated).async {
                guard let analyzer = try? SNAudioFileAnalyzer(url: url),
                      let request = try? SNClassifySoundRequest(classifierIdentifier: .version1) else {
                    cont.resume(returning: nil)
                    return
                }
                let observer = SoundObserver()
                guard (try? analyzer.add(request, withObserver: observer)) != nil else {
                    cont.resume(returning: nil)
                    return
                }
                analyzer.analyze()
                // Require real confidence so a file is never named off a weak guess.
                if let best = observer.best, best.confidence >= 0.45 {
                    cont.resume(returning: best.label)
                } else {
                    cont.resume(returning: nil)
                }
            }
        }
    }
}

/// Collects the highest-confidence classification across an audio file's time windows.
/// Used only within the single synchronous `analyze()` call on one thread, so its mutable
/// state needs no lock (it never crosses an isolation boundary).
private final class SoundObserver: NSObject, SNResultsObserving, @unchecked Sendable {
    var best: (label: String, confidence: Double)?
    func request(_ request: SNRequest, didProduce result: SNResult) {
        guard let classification = result as? SNClassificationResult,
              let top = classification.classifications.first else { return }
        if best == nil || top.confidence > best!.confidence {
            best = (top.identifier, top.confidence)
        }
    }
}

/// One-shot guard so the recognition continuation is resumed at most once.
private final class OnceFlag: @unchecked Sendable {
    private let lock = NSLock()
    private var fired = false
    func claim() -> Bool {
        lock.lock(); defer { lock.unlock() }
        if fired { return false }
        fired = true
        return true
    }
}
