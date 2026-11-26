import Foundation

enum FactoryReset {
    /// Deletes everything under `root` except the folders in `keeping` (and the folders that lead to them), so the
    /// multi-gigabyte description models survive a reset the way the confirmation says they do.
    static func erase(root: URL, keeping: [URL], fileManager: FileManager = .default) {
        let rootPath = root.standardizedFileURL.resolvingSymlinksInPath().path
        let kept = keeping.map { $0.standardizedFileURL.resolvingSymlinksInPath().path }
        func prune(_ folder: URL) {
            guard let children = try? fileManager.contentsOfDirectory(at: folder, includingPropertiesForKeys: nil) else { return }
            for child in children {
                let path = child.standardizedFileURL.resolvingSymlinksInPath().path
                if kept.contains(path) { continue }
                if kept.contains(where: { $0.hasPrefix(path + "/") }) { prune(child); continue }
                try? fileManager.removeItem(at: child)
            }
        }
        if kept.contains(where: { $0.hasPrefix(rootPath + "/") }) {
            prune(root)
        } else {
            try? fileManager.removeItem(at: root)
        }
    }

    /// Removes every key of the app's defaults domain, then the domain itself, so none survives in the preferences file.
    static func eraseDefaults(domain: String, defaults: UserDefaults = .standard) {
        for key in (defaults.persistentDomain(forName: domain) ?? [:]).keys {
            defaults.removeObject(forKey: key)
        }
        defaults.removePersistentDomain(forName: domain)
        defaults.synchronize()
    }
}
