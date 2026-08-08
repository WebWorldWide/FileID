import Foundation
import FileIDShared
import Testing
@testable import FileID

@Suite("Model storage removal", .serialized)
struct ModelStorageTests {
    @Test("remove all deletes only FileID model directories")
    func removeAllModelsPreservesUnrelatedDownloads() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-model-removal-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let appModels = root.appendingPathComponent("app-models")
        let deepModels = root.appendingPathComponent("huggingface-models")
        let unrelated = deepModels.appendingPathComponent("someone-else/unrelated-model")
        try FileManager.default.createDirectory(at: appModels, withIntermediateDirectories: true)
        try Data("model".utf8).write(to: appModels.appendingPathComponent("weight.bin"))
        try FileManager.default.createDirectory(at: unrelated, withIntermediateDirectories: true)
        try Data("keep".utf8).write(to: unrelated.appendingPathComponent("weight.bin"))
        for kind in AIModelKind.allCases.prefix(2) {
            let directory = try #require(ModelStorage.deepAnalyzeDirectory(
                for: kind, modelsRoot: deepModels))
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            try Data("model".utf8).write(to: directory.appendingPathComponent("weight.bin"))
        }

        let report = ModelStorage.removeAllModels(
            appModelsRoot: appModels,
            deepAnalyzeRoot: deepModels
        )

        #expect(report.failures.isEmpty)
        #expect(!FileManager.default.fileExists(atPath: appModels.path))
        #expect(FileManager.default.fileExists(atPath: unrelated.path))
        for kind in AIModelKind.allCases.prefix(2) {
            let directory = try #require(ModelStorage.deepAnalyzeDirectory(
                for: kind, modelsRoot: deepModels))
            #expect(!FileManager.default.fileExists(atPath: directory.path))
        }
    }

    @Test("single LLM removal leaves every neighboring model intact")
    func removeOneDeepAnalyzeModel() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("fileid-single-model-removal-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: root) }
        let kinds = Array(AIModelKind.allCases.prefix(2))
        let first = try #require(kinds.first)
        let second = try #require(kinds.last)
        for kind in kinds {
            let directory = try #require(ModelStorage.deepAnalyzeDirectory(
                for: kind, modelsRoot: root))
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            try Data("model".utf8).write(to: directory.appendingPathComponent("weight.bin"))
        }

        let report = ModelStorage.removeDeepAnalyzeModel(first, modelsRoot: root)

        #expect(report.failures.isEmpty)
        let firstDirectory = try #require(ModelStorage.deepAnalyzeDirectory(
            for: first, modelsRoot: root))
        let secondDirectory = try #require(ModelStorage.deepAnalyzeDirectory(
            for: second, modelsRoot: root))
        #expect(!FileManager.default.fileExists(atPath: firstDirectory.path))
        #expect(FileManager.default.fileExists(atPath: secondDirectory.path))
    }
}
