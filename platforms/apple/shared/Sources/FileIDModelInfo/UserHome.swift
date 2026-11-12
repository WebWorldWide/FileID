import Foundation

/// The user's real home folder. Inside the App Sandbox `HOME`, `NSHomeDirectory()` and
/// `homeDirectoryForCurrentUser` all point at the app's container, so Downloads, Desktop
/// and the Trash resolve to empty folders there; the password database still names the
/// real one.
public enum UserHome {
    public static var path: String {
        let environment = ProcessInfo.processInfo.environment["HOME"] ?? ""
        if !environment.isEmpty, !isContainer(environment) { return environment }
        if let entry = getpwuid(getuid()), let dir = entry.pointee.pw_dir {
            let real = String(cString: dir)
            if !real.isEmpty { return real }
        }
        return environment.isEmpty ? NSHomeDirectory() : environment
    }

    public static var url: URL { URL(fileURLWithPath: path, isDirectory: true) }

    public static var trashURL: URL { url.appendingPathComponent(".Trash", isDirectory: true) }

    static func isContainer(_ path: String) -> Bool {
        path.contains("/Library/Containers/")
    }
}
