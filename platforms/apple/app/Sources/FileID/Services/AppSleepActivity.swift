import Foundation

enum AppSleepActivity {
    /// Scoped activity for a single awaited operation (model installers). The
    /// token is balanced by `defer`, so App Nap can't throttle the UI process
    /// while `operation` runs.
    @MainActor
    static func run(reason: String, operation: @MainActor () async -> Void) async {
        let token = begin(reason: reason)
        defer { end(token) }
        await operation()
    }

    /// Token-based activity for operations whose lifetime is event-driven
    /// (the engine scan / Deep Analyze span many IPC events, not one awaited
    /// call). The caller owns the token and MUST pair every `begin` with
    /// exactly one `end` on every terminal path — completion, cancellation,
    /// failure, and engine crash/exit. Same options the closure form uses, so
    /// the App-Nap policy lives in one place.
    static func begin(reason: String) -> NSObjectProtocol {
        ProcessInfo.processInfo.beginActivity(
            options: [.userInitiated, .idleSystemSleepDisabled],
            reason: reason)
    }

    static func end(_ token: NSObjectProtocol) {
        ProcessInfo.processInfo.endActivity(token)
    }
}
