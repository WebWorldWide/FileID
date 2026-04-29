import Foundation
import AppKit

enum UninstallService {

    struct Report {
        let removed: [URL]
        let failures: [(URL, Error)]
    }

    static func preview() -> [URL] {
        paths().filter { FileManager.default.fileExists(atPath: $0.path) }
    }

    static func totalBytes() -> Int64 {
        var sum: Int64 = 0
        for url in preview() {
            sum += directorySize(url)
        }
        return sum
    }

    static func perform() -> Report {
        var removed: [URL] = []
        var failures: [(URL, Error)] = []

        for url in paths() where FileManager.default.fileExists(atPath: url.path) {
            do {
                try FileManager.default.removeItem(at: url)
                removed.append(url)
            } catch {
                failures.append((url, error))
            }
        }

        if let bid = Bundle.main.bundleIdentifier {
            UserDefaults.standard.removePersistentDomain(forName: bid)
            UserDefaults.standard.synchronize()
        }

        return Report(removed: removed, failures: failures)
    }

    private static func paths() -> [URL] {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let bid = Bundle.main.bundleIdentifier ?? "FileID"
        return [
            home.appending(path: "Library/Application Support/FileID"),
            home.appending(path: "Documents/huggingface/models/mlx-community/Qwen2.5-VL-3B-Instruct-4bit"),
            home.appending(path: "Documents/huggingface/models/mlx-community/Qwen2-VL-2B-Instruct-4bit"),
            home.appending(path: "Library/Logs/FileID"),
            home.appending(path: "Library/Caches/\(bid)"),
        ]
    }

    private static func directorySize(_ url: URL) -> Int64 {
        var total: Int64 = 0
        let keys: Set<URLResourceKey> = [.totalFileAllocatedSizeKey, .isRegularFileKey]
        guard let enumerator = FileManager.default.enumerator(
            at: url,
            includingPropertiesForKeys: Array(keys),
            options: [.skipsHiddenFiles]
        ) else {
            if let attrs = try? FileManager.default.attributesOfItem(atPath: url.path),
               let size = attrs[.size] as? Int64 { return size }
            return 0
        }
        for case let fileURL as URL in enumerator {
            guard let values = try? fileURL.resourceValues(forKeys: keys),
                  values.isRegularFile == true else { continue }
            total += Int64(values.totalFileAllocatedSize ?? 0)
        }
        return total
    }
}
