import SwiftUI
import AppKit
import SwiftData

@main
struct FileIDApp: App {
    // Hide the standard window background to allow full liquid glass effect
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    
    var body: some Scene {
        WindowGroup {
            MainWindowView()
                .frame(minWidth: 1200, minHeight: 800)
                .background(VisualEffectView(material: .hudWindow, blendingMode: .behindWindow))
                .ignoresSafeArea()
                .modelContainer(for: [FileRecord.self, PersonRecord.self])
        }
        .windowStyle(.hiddenTitleBar)
        .commands {
            SidebarCommands()
            CommandGroup(after: .appInfo) {
                Button("About FileID Professional") { }
            }
            CommandGroup(replacing: .newItem) { }
            CommandMenu("File") {
                Button("Open Folder…") {
                    NotificationCenter.default.post(name: .fileIDOpenFolder, object: nil)
                }
                .keyboardShortcut("o", modifiers: .command)
                Button("Rescan Current Folder") {
                    NotificationCenter.default.post(name: .fileIDRescan, object: nil)
                }
                .keyboardShortcut("r", modifiers: .command)
            }
        }
    }
}

// Visual Effect View wrapper to bring AppKit's native vibrant materials to SwiftUI
struct VisualEffectView: NSViewRepresentable {
    var material: NSVisualEffectView.Material
    var blendingMode: NSVisualEffectView.BlendingMode
    
    func makeNSView(context: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = .active
        return view
    }
    
    func updateNSView(_ nsView: NSVisualEffectView, context: Context) {
        nsView.material = material
        nsView.blendingMode = blendingMode
    }
}

class AppDelegate: NSObject, NSApplicationDelegate {
    var appViewModel: AppViewModel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        if let window = NSApplication.shared.windows.first {
            window.isOpaque = false
            window.backgroundColor = .clear
            window.titlebarAppearsTransparent = true
            window.titleVisibility = .hidden
            window.styleMask.insert(.fullSizeContentView)
        }
    }
}

// MARK: - Notification Names

extension Notification.Name {
    static let fileIDOpenFolder = Notification.Name("fileIDOpenFolder")
    static let fileIDRescan     = Notification.Name("fileIDRescan")
}
