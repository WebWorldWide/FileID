import Foundation

/// Decides what a relaunch does with the saved scan-folder bookmark. A folder on a drive that is unplugged right now
/// resolves to nothing; the bookmark must survive so the folder returns when the drive is mounted again.
enum PickedFolderRestore {
    static func accessibleFolder(
        defaults: UserDefaults,
        key: String,
        retain: (Data) -> URL?,
        isDirectory: (URL) -> Bool
    ) -> URL? {
        guard let data = defaults.data(forKey: key) else { return nil }
        guard let url = retain(data), isDirectory(url) else { return nil }
        return url
    }
}
