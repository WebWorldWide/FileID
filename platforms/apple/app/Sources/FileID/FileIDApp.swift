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

    init() { BrandFonts.register() }
    @State private var engineStarted = false
    @State private var showWelcome = false
    @AppStorage("welcomeSheetSeen") private var welcomeSheetSeen: Bool = false

    var body: some Scene {
        WindowGroup("FileID") {
            MainWindow(engine: engine)
                .font(Theme.ChromeFont.body(13.5))
                .toggleStyle(DSCheckboxToggleStyle())
                .progressViewStyle(DSProgressViewStyle())
                .buttonStyle(DSSecondaryButtonStyle())
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
                    let engine = engine
                    ThumbnailService.shared.videoSource = { path, modifiedAt, maxPixelSize in
                        await engine.generateVideoThumbnail(path: path, modifiedAt: modifiedAt, maxPixelSize: maxPixelSize)
                    }
                    startEngineIfNeeded()
                    // The engine embeds search text, so the app only needs the
                    // model on disk; search stays on keywords until it is.
                    CLIPModelInstaller.shared.refreshStatus()
                    if case .installed = CLIPModelInstaller.shared.status {
                        CLIPModelInstaller.shared.markTextEncoderReady()
                    }
                    ModelInstaller.ramPlus.refreshStatus()
                    ModelInstaller.face.refreshStatus()
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
            CommandGroup(replacing: .newItem) {
                Button("Choose Folder…") { NotificationCenter.default.post(name: .fileIDChooseFolder, object: nil) }
                    .keyboardShortcut("o", modifiers: .command)
            }
            CommandGroup(replacing: .help) {
                Button("FileID Help") { NSWorkspace.shared.open(URL(string: "https://github.com/WebWorldWide/FileID#readme")!) }
                Button("Show Welcome Sheet…") { showWelcome = true }
                Divider()
                Button("Privacy on this Mac") {
                    let alert = NSAlert()
                    alert.messageText = "Everything stays on this Mac"
                    alert.informativeText = "FileID never sends telemetry. The only network use is a model download you start yourself, fetched from huggingface.co and verified against pinned hashes."
                    alert.alertStyle = .informational
                    alert.runModal()
                }
            }
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
                .toggleStyle(DSCheckboxToggleStyle())
                .progressViewStyle(DSProgressViewStyle())
                .buttonStyle(DSSecondaryButtonStyle())
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

extension Notification.Name {
    /// File ▸ Choose Folder… (⌘O): the main window opens its folder picker.
    static let fileIDChooseFolder = Notification.Name("FileID.chooseFolder")
}
