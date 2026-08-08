import Foundation
import FileIDShared

struct ModelRemovalReport: Sendable {
    let removedCount: Int
    let failures: [String]

    var failureMessage: String? {
        failures.isEmpty ? nil : failures.joined(separator: "\n")
    }
}

enum ModelStorage {
    static var deepAnalyzeModelsRoot: URL? {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first?
            .appendingPathComponent("huggingface/models", isDirectory: true)
    }

    static func deepAnalyzeDirectory(
        for kind: AIModelKind,
        modelsRoot: URL? = deepAnalyzeModelsRoot
    ) -> URL? {
        modelsRoot?.appendingPathComponent(kind.sourceRepo, isDirectory: true)
    }

    static func removeDeepAnalyzeModel(
        _ kind: AIModelKind,
        modelsRoot: URL? = deepAnalyzeModelsRoot
    ) -> ModelRemovalReport {
        guard let directory = deepAnalyzeDirectory(for: kind, modelsRoot: modelsRoot) else {
            return ModelRemovalReport(
                removedCount: 0,
                failures: ["The Documents folder could not be located."]
            )
        }
        return removeDirectories([directory])
    }

    static func removeAllModels(
        appModelsRoot: URL = AppSupportPath.root
            .appendingPathComponent("FileID/Models", isDirectory: true),
        deepAnalyzeRoot: URL? = deepAnalyzeModelsRoot
    ) -> ModelRemovalReport {
        var directories = [appModelsRoot]
        if let deepAnalyzeRoot {
            directories.append(contentsOf: AIModelKind.allCases.compactMap {
                deepAnalyzeDirectory(for: $0, modelsRoot: deepAnalyzeRoot)
            })
        }
        return removeDirectories(Array(Set(directories)))
    }

    static func removeDirectories(_ directories: [URL]) -> ModelRemovalReport {
        let fileManager = FileManager.default
        var removedCount = 0
        var failures: [String] = []
        for directory in directories {
            guard fileManager.fileExists(atPath: directory.path) else { continue }
            do {
                try fileManager.removeItem(at: directory)
                removedCount += 1
            } catch {
                failures.append("Couldn't remove \(directory.lastPathComponent): \(error.localizedDescription)")
            }
        }
        return ModelRemovalReport(removedCount: removedCount, failures: failures)
    }
}
