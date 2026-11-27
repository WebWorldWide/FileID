import Foundation
import Testing
@testable import FileIDModelInfo

@Suite("FileIDModelInfo")
struct ModelInfoTests {
    private static let sources = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .appendingPathComponent("shared/Sources/FileIDModelInfo", isDirectory: true)

    /// FileIDAppleKit links this target: it must stay free of download,
    /// network, ONNX Runtime and MLX code.
    @Test func importsOnlyFoundation() throws {
        let files = try FileManager.default.contentsOfDirectory(at: Self.sources, includingPropertiesForKeys: nil)
            .filter { $0.pathExtension == "swift" }
        #expect(!files.isEmpty)
        for file in files {
            let text = try String(contentsOf: file, encoding: .utf8)
            for line in text.split(separator: "\n") {
                let trimmed = line.trimmingCharacters(in: .whitespaces)
                guard trimmed.hasPrefix("import ") || trimmed.contains(" import ") && trimmed.hasPrefix("@") else { continue }
                #expect(trimmed == "import Foundation", "\(file.lastPathComponent): \(trimmed)")
            }
            for banned in ["URLSession", "URLRequest", "NWConnection", "Process()", "OrtSession", "import MLX"] {
                #expect(!text.contains(banned), "\(file.lastPathComponent) mentions \(banned)")
            }
        }
    }

    /// Every layout reads the same catalog: the compiled-in copy, byte for byte the
    /// file the Rust engine embeds, and it must parse (an empty catalog would make
    /// the engines disagree silently).
    @Test func compiledCatalogMatchesTheFileAndParses() throws {
        let file = try String(contentsOf: Self.sources.appendingPathComponent("Resources/model-catalog.json"), encoding: .utf8)
        #expect(ModelCatalogData.json + "\n" == file)
        #expect(MLXModelCatalog.entries.count > 100)
        let text = try String(contentsOf: Self.sources.appendingPathComponent("MLXModelCatalog.swift"), encoding: .utf8)
        #expect(!text.contains("Bundle.module"))
    }
}

@Suite("Face model install detection")
struct FaceModelInstallDetectionTests {
    @Test("the engine's sface/ layout counts as installed, the same as the top level")
    func engineLayoutCountsAsInstalled() throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("face-layout-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: root.appendingPathComponent("sface"), withIntermediateDirectories: true)
        #expect(!FaceEmbedderKind.sface.isInstalled(in: root))
        let file = root.appendingPathComponent("sface/face_recognition_sface_2021dec.onnx")
        try Data("m".utf8).write(to: file)
        #expect(FaceEmbedderKind.sface.isInstalled(in: root))
        #expect(FaceEmbedderKind.sface.installedFile(in: root)?.path == file.path)
    }
}
