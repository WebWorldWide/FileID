// Deep Analyze metadata-naming parity: the macOS name-builders must behave identically
// to the Windows engine's deep_analyze.rs (build_audio_name / build_obj_name /
// is_meaningful_model_name / parse_obj_names) so the same audio/3D file gets the same
// descriptive name on either platform.
import Testing
import Foundation
@testable import FileIDEngine

@Suite("Deep Analyze metadata naming (audio + 3D)")
struct DeepAnalyzeNamingTests {

    @Test("buildAudioName: Artist - Title, title-only, else nil")
    func audioName() {
        #expect(DeepAnalyzeNaming.buildAudioName(title: "Hey Jude", artist: "The Beatles")
            == "The Beatles - Hey Jude")
        #expect(DeepAnalyzeNaming.buildAudioName(title: "Clair de Lune", artist: nil) == "Clair de Lune")
        #expect(DeepAnalyzeNaming.buildAudioName(title: nil, artist: "The Beatles") == nil)
        #expect(DeepAnalyzeNaming.buildAudioName(title: nil, artist: nil) == nil)
        // Illegal path chars sanitized, case preserved.
        #expect(DeepAnalyzeNaming.buildAudioName(title: "AC/DC: Live", artist: "Band")
            == "Band - AC_DC_ Live")
    }

    @Test("nameFromTranscript: leading words → name, lead → caption (lockstep with Rust)")
    func transcriptName() {
        let r = DeepAnalyzeNaming.nameFromTranscript(
            "  This is a quick meeting about the Q3 budget and headcount  ")
        #expect(r?.name == "This is a quick meeting about the Q3")
        #expect(r?.description.hasPrefix("Audio transcript: This is a quick meeting") == true)
        #expect(DeepAnalyzeNaming.nameFromTranscript("   \n  ") == nil)
        #expect(DeepAnalyzeNaming.nameFromTranscript("") == nil)
    }

    @Test("nameFromSoundLabel: humanizes events, drops generic labels")
    func soundLabel() {
        #expect(DeepAnalyzeNaming.nameFromSoundLabel("dog_bark")?.name == "Dog Bark")
        #expect(DeepAnalyzeNaming.nameFromSoundLabel("rain")?.name == "Rain")
        #expect(DeepAnalyzeNaming.nameFromSoundLabel("dog_bark")?.description
            == "Detected sound: Dog Bark")
        // Generic / content-free labels keep the original name (nil).
        #expect(DeepAnalyzeNaming.nameFromSoundLabel("speech") == nil)
        #expect(DeepAnalyzeNaming.nameFromSoundLabel("music") == nil)
        #expect(DeepAnalyzeNaming.nameFromSoundLabel("") == nil)
    }

    @Test("sound analysis obeys its wall-clock deadline")
    func soundAnalysisDeadline() async throws {
        let file = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileID-Sound-\(UUID().uuidString).wav")
        defer { try? FileManager.default.removeItem(at: file) }
        try Data(repeating: 0, count: 512).write(to: file)
        let clock = ContinuousClock()
        let started = clock.now
        let label = await DeepAnalyzeNaming.dominantSoundIdentifier(
            url: file,
            timeoutSeconds: 0
        )
        #expect(label == nil)
        #expect(started.duration(to: clock.now) < .seconds(1))
    }

    @Test("buildObjName: meaningful object, else material, else nil")
    func objName() {
        #expect(DeepAnalyzeNaming.buildObjName(objects: ["default", "Spaceship"], materials: [])
            == "Spaceship")
        #expect(DeepAnalyzeNaming.buildObjName(objects: ["Object", "mesh.001"], materials: ["BrushedSteel"])
            == "BrushedSteel")
        #expect(DeepAnalyzeNaming.buildObjName(objects: ["Cube", "001"], materials: ["default"]) == nil)
        #expect(DeepAnalyzeNaming.buildObjName(objects: [], materials: []) == nil)
    }

    @Test("isMeaningfulModelName filters placeholders, keeps real words")
    func meaningful() {
        #expect(DeepAnalyzeNaming.isMeaningfulModelName("Spaceship"))
        #expect(DeepAnalyzeNaming.isMeaningfulModelName("Brushed Steel"))
        #expect(DeepAnalyzeNaming.isMeaningfulModelName("Cubey")) // not the generic "Cube"
        #expect(!DeepAnalyzeNaming.isMeaningfulModelName("default"))
        #expect(!DeepAnalyzeNaming.isMeaningfulModelName("Object"))
        #expect(!DeepAnalyzeNaming.isMeaningfulModelName("object1"))
        #expect(!DeepAnalyzeNaming.isMeaningfulModelName("mesh.001"))
        #expect(!DeepAnalyzeNaming.isMeaningfulModelName("001"))
        #expect(!DeepAnalyzeNaming.isMeaningfulModelName("x"))
    }

    @Test("parseObjNames reads objects, materials, and the .mtl")
    func parseObj() throws {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDObj-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        try "newmtl Hull\nKd 0.5 0.5 0.5\nnewmtl Cockpit\n"
            .write(to: dir.appendingPathComponent("ship.mtl"), atomically: true, encoding: .utf8)
        try "# a tiny ship\nmtllib ship.mtl\no Spaceship\nv 0 0 0\nusemtl Hull\nf 1 1 1\ng Wing\nusemtl Hull\n"
            .write(to: dir.appendingPathComponent("ship.obj"), atomically: true, encoding: .utf8)

        let (objects, materials) = DeepAnalyzeNaming.parseObjNames(url: dir.appendingPathComponent("ship.obj"))
        #expect(objects == ["Spaceship", "Wing"])
        #expect(materials.contains("Hull"))
        #expect(materials.contains("Cockpit"))
        #expect(materials.filter { $0 == "Hull" }.count == 1) // usemtl Hull deduped
        #expect(DeepAnalyzeNaming.buildObjName(objects: objects, materials: materials) == "Spaceship")
    }

    @Test("OBJ material libraries stay inside the model folder and must be regular files")
    func materialLibraryContainment() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("FileIDObjSafety-\(UUID().uuidString)", isDirectory: true)
        let models = root.appendingPathComponent("models", isDirectory: true)
        try FileManager.default.createDirectory(at: models, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let outside = root.appendingPathComponent("outside.mtl")
        try "newmtl OutsideSecret\n".write(to: outside, atomically: true, encoding: .utf8)

        let traversal = models.appendingPathComponent("traversal.obj")
        try "mtllib ../outside.mtl\no SafeObject\n"
            .write(to: traversal, atomically: true, encoding: .utf8)
        let traversalNames = DeepAnalyzeNaming.parseObjNames(url: traversal)
        #expect(traversalNames.objects == ["SafeObject"])
        #expect(!traversalNames.materials.contains("OutsideSecret"))
        #expect(DeepAnalyzeNaming.safeMaterialLibraryURL(
            objURL: traversal,
            reference: "../outside.mtl"
        ) == nil)
        #expect(DeepAnalyzeNaming.safeMaterialLibraryURL(
            objURL: traversal,
            reference: outside.path
        ) == nil)
        #expect(DeepAnalyzeNaming.safeMaterialLibraryURL(
            objURL: traversal,
            reference: #"C:\outside.mtl"#
        ) == nil)

        let symlink = models.appendingPathComponent("linked.mtl")
        try FileManager.default.createSymbolicLink(at: symlink, withDestinationURL: outside)
        #expect(DeepAnalyzeNaming.safeMaterialLibraryURL(
            objURL: traversal,
            reference: "linked.mtl"
        ) == nil)

        let directory = models.appendingPathComponent("folder.mtl", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false)
        #expect(DeepAnalyzeNaming.safeMaterialLibraryURL(
            objURL: traversal,
            reference: "folder.mtl"
        ) == nil)

        let valid = models.appendingPathComponent("valid.mtl")
        try "newmtl BrushedSteel\n".write(to: valid, atomically: true, encoding: .utf8)
        #expect(DeepAnalyzeNaming.safeMaterialLibraryURL(
            objURL: traversal,
            reference: "valid.mtl"
        ) == valid.standardizedFileURL)
    }

    @Test("FileTypes classifies .obj as model and .mp3 as audio (scanned, not dropped)")
    func kindClassification() {
        #expect(FileTypes.kind(forExtension: "obj") == .model)
        #expect(FileTypes.kind(forExtension: "OBJ") == .model)
        #expect(FileTypes.kind(forExtension: "mp3") == .audio)
        #expect(DiscoveredFile.Kind.model.rawValue == "model") // DB string lockstep
        #expect(FileTypes.isTaggable("obj"), "an .obj must be scanned, not dropped")
        // Source code + e-books cluster by their extracted text (doc); 3D formats beyond
        // .obj group under 3D Models/. Lockstep with the Rust FileKind::from_extension.
        #expect(FileTypes.kind(forExtension: "py") == .doc)
        #expect(FileTypes.kind(forExtension: "RS") == .doc)
        #expect(FileTypes.kind(forExtension: "swift") == .doc)
        #expect(FileTypes.kind(forExtension: "epub") == .doc)
        #expect(FileTypes.kind(forExtension: "stl") == .model)
        #expect(FileTypes.kind(forExtension: "glb") == .model)
        #expect(FileTypes.kind(forExtension: "usdz") == .model)
        // Lockstep alignment with Windows (formats macOS frameworks also handle).
        #expect(FileTypes.kind(forExtension: "mts") == .video)
        #expect(FileTypes.kind(forExtension: "m2ts") == .video)
        #expect(FileTypes.kind(forExtension: "odt") == .doc)
        #expect(FileTypes.kind(forExtension: "bin") == .other, "an unknown binary stays other")
        #expect(FileTypes.isTaggable("py") && FileTypes.isTaggable("epub") && FileTypes.isTaggable("stl"))
    }
}
