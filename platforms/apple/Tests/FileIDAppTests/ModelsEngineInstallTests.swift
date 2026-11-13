import Foundation
import Testing
@testable import FileID

/// `shared/test-corpus/mac-model-layout.json`: the files each app installer
/// writes are the files the Rust engine installs for its `prewarmModel` kind.
@Suite("Model installers and the Rust engine share one layout")
@MainActor
struct ModelsEngineInstallTests {
    struct Layout: Decodable { let kinds: [String: [String]] }

    private func layout() throws -> Layout {
        var root = URL(fileURLWithPath: #filePath)
        for _ in 0..<5 { root.deleteLastPathComponent() }
        let data = try Data(contentsOf: root.appendingPathComponent("shared/test-corpus/mac-model-layout.json"))
        return try JSONDecoder().decode(Layout.self, from: data)
    }

    private func relative(_ url: URL) -> String {
        let base = AppSupportPath.models.standardizedFileURL.path + "/"
        let path = url.standardizedFileURL.path
        return path.hasPrefix(base) ? String(path.dropFirst(base.count)) : path
    }

    @Test("each ONNX installer names the engine kind whose files it writes")
    func installersMatchTheSharedLayout() throws {
        let kinds = try layout().kinds
        for installer in [ModelInstaller.ramPlus, .bge, .face, .faceMesh] {
            let spec = installer.spec
            #expect(spec.downloads.map { relative(spec.destination($0)) } == kinds[spec.engineKind], "\(spec.engineKind)")
        }
        let clip = CLIPModelInstaller.requiredFiles.map(relative)
        #expect(clip == (kinds["mobileclip_s2"] ?? []) + (kinds["clip_text"] ?? []))
    }

    @Test("without a Rust engine the app keeps installing models itself")
    func swiftEngineKeepsAppInstalls() {
        let saved = EngineModelInstall.engine
        defer { EngineModelInstall.engine = saved }
        EngineModelInstall.engine = nil
        #expect(!EngineModelInstall.active)
        let engine = EngineClient()
        EngineModelInstall.engine = engine
        #expect(!EngineModelInstall.active)
    }
}
