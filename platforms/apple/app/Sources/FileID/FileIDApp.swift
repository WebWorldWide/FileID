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
    @State private var toolsSession = ToolsSession()
    @State private var engineStarted = false
    @State private var showWelcome = false
    @AppStorage("welcomeSheetSeen") private var welcomeSheetSeen: Bool = false

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
                .onAppear {
                    startEngineIfNeeded()
                    // Search falls back to keyword matching if CLIP
                    // isn't installed yet.
                    Task.detached {
                        if CLIPTextEncoder.shared.load() {
                            await CLIPModelInstaller.shared.markTextEncoderReady()
                        }
                    }
                    CLIPModelInstaller.shared.refreshStatus()
                    RamPlusModelInstaller.shared.refreshStatus()
                    ArcFaceModelInstaller.shared.refreshStatus()
                    if !welcomeSheetSeen { showWelcome = true }
                }
                .sheet(isPresented: $showWelcome) {
                    WelcomeSheet(engine: engine)
                        .onDisappear { welcomeSheetSeen = true }
                }
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
        Window("File Tools", id: "file-tools") {
            ToolsWorkbench(engine: engine, session: toolsSession)
                .background { LavaLampBackground().ignoresSafeArea() }
                .preferredColorScheme(.dark)
                .onAppear { startEngineIfNeeded() }
        }
        .defaultSize(width: 940, height: 720)
    }

    private func startEngineIfNeeded() {
        appDelegate.engine = engine
        guard !engineStarted else { return }
        engineStarted = true
        engine.start()
    }

}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    weak var engine: EngineClient?

    func applicationWillTerminate(_ notification: Notification) { engine?.shutdown() }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.appearance = NSAppearance(named: .darkAqua)
        if let window = NSApplication.shared.windows.first {
            window.isOpaque = false
            window.backgroundColor = .clear
            window.appearance = NSAppearance(named: .darkAqua)
            window.styleMask.insert(.fullSizeContentView)
        }
    }
}
