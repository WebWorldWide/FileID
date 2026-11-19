import Foundation
import Testing
@testable import FileID

@Suite("Model files in either layout")
struct ModelFileLayoutTests {
    private func temporaryRoot() throws -> URL {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("model-layout-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        return root
    }

    private func write(_ relative: String, in root: URL) throws -> URL {
        let url = root.appendingPathComponent(relative)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("model".utf8).write(to: url)
        return url
    }

    @Test("a Rust-layout copy counts as the Swift-layout model, and the reverse")
    func eitherLayoutIsFound() throws {
        let root = try temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let swiftFace = root.appendingPathComponent("face_recognition_sface_2021dec.onnx")
        let swiftClip = root.appendingPathComponent("mobileclip_image/clip_vitb32_image.onnx")
        #expect(ModelFileLayout.installedFile(swiftFace, in: root) == nil)
        #expect(ModelFileLayout.installedFile(swiftClip, in: root) == nil)

        let rustFace = try write("sface/face_recognition_sface_2021dec.onnx", in: root)
        let rustClip = try write("mobileclip/mobileclip_s2_image.onnx", in: root)
        #expect(ModelFileLayout.installedFile(swiftFace, in: root)?.path == rustFace.path)
        #expect(ModelFileLayout.installedFile(swiftClip, in: root)?.path == rustClip.path)
        #expect(ModelFileLayout.installedFile(rustFace, in: root)?.path == rustFace.path)

        _ = try write("face_recognition_sface_2021dec.onnx", in: root)
        try FileManager.default.removeItem(at: rustFace)
        #expect(ModelFileLayout.installedFile(rustFace, in: root)?.path == swiftFace.path)
    }

    @Test("files the engine does not alias are only found where they are")
    func unaliasedFilesStayExact() throws {
        let root = try temporaryRoot()
        defer { try? FileManager.default.removeItem(at: root) }
        let text = root.appendingPathComponent("clip_text/clip_text.onnx")
        _ = try write("sface/clip_text.onnx", in: root)
        #expect(ModelFileLayout.alternate(of: text, in: root) == nil)
        #expect(ModelFileLayout.installedFile(text, in: root) == nil)
    }

    @Test("the Settings rows' own files have the layout the engine's table lists")
    @MainActor func settingsFilesHaveAlternates() {
        let root = AppSupportPath.models
        let clip = CLIPModelInstaller.requiredFiles[0]
        #expect(ModelFileLayout.alternate(of: clip, in: CLIPModelInstaller.modelsRoot)?.path == root.appendingPathComponent("mobileclip/mobileclip_s2_image.onnx").path)
        let face = ModelInstaller.face.spec.primaryFile
        #expect(ModelFileLayout.alternate(of: face, in: root)?.path == root.appendingPathComponent("sface/face_recognition_sface_2021dec.onnx").path)
    }
}
