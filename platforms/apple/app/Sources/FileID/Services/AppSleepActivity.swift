import Foundation

enum AppSleepActivity {
    /// Keeps App Nap off while `operation` runs; the token is balanced by `defer`.
    @MainActor
    static func run(reason: String, operation: @MainActor () async -> Void) async {
        let token = begin(reason: reason)
        defer { end(token) }
        await operation()
    }

    /// The caller must pair every `begin` with exactly one `end` on every terminal path, including engine crash or exit.
    static func begin(reason: String) -> NSObjectProtocol {
        ProcessInfo.processInfo.beginActivity(
            options: [.userInitiated, .idleSystemSleepDisabled],
            reason: reason)
    }

    static func end(_ token: NSObjectProtocol) {
        ProcessInfo.processInfo.endActivity(token)
    }
}
