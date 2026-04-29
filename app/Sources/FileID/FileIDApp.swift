// SwiftUI app shell. Hidden title bar + full-size content view so the
// LavaLamp + materials extend to the top edge of the window in both
// normal and full-screen modes.
import SwiftUI
import AppKit
import FileIDShared

@main
struct FileIDApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @State private var engine = EngineClient()

    var body: some Scene {
        WindowGroup("FileID") {
            MainWindow(engine: engine)
                .frame(minWidth: 1200, minHeight: 800)
                .background(
                    VisualEffectView(material: .underWindowBackground,
                                     blendingMode: .behindWindow)
                        .ignoresSafeArea()
                )
                // Tab views own their top padding to clear the floating
                // traffic-light overlay.
                .ignoresSafeArea()
                .onAppear { engine.start() }
                .onDisappear { engine.shutdown() }
        }
        .windowStyle(.hiddenTitleBar)
        .commands {
            SidebarCommands()
            CommandGroup(replacing: .newItem) { }
            CommandGroup(after: .appInfo) {
                Button("About FileID") {
                    let info = Bundle.main.infoDictionary
                    let version = info?["CFBundleShortVersionString"] as? String ?? "dev"
                    let build = info?["CFBundleVersion"] as? String ?? "local"
                    let alert = NSAlert()
                    alert.messageText = "FileID"
                    alert.informativeText = "Version \(version) (build \(build))\nOn-device AI file organization for macOS.\n\nv2 split-process architecture."
                    alert.alertStyle = .informational
                    alert.addButton(withTitle: "OK")
                    alert.runModal()
                }
            }
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    /// Held for the app lifetime to keep AppNap from suspending the UI
    /// process during long scans / Deep Analyze runs.
    private var activityToken: NSObjectProtocol?

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.appearance = NSAppearance(named: .darkAqua)
        if let window = NSApplication.shared.windows.first {
            window.isOpaque = false
            window.backgroundColor = .clear
            window.appearance = NSAppearance(named: .darkAqua)
            window.styleMask.insert(.fullSizeContentView)
        }
        activityToken = ProcessInfo.processInfo.beginActivity(
            options: [.userInitiated, .idleSystemSleepDisabled, .latencyCritical],
            reason: "FileID is processing files"
        )
    }

    func applicationWillTerminate(_ notification: Notification) {
        if let token = activityToken {
            ProcessInfo.processInfo.endActivity(token)
            activityToken = nil
        }
    }
}
