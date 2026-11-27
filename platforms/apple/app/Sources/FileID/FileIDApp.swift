// SwiftUI app shell. Hidden title bar + full-size content view so the
// LavaLamp + materials extend to the top edge of the window in both
// normal and full-screen modes.
import SwiftUI
import AppKit
import FileIDShared

/// Smallest window every page lays out in, and the larger size a first launch opens at.
enum WindowSizes {
    static let minimum = CGSize(width: 1000, height: 700)
    static let firstLaunch = CGSize(width: 1200, height: 800)
}

@main
struct FileIDApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate
    @State private var engine = EngineClient()
    @State private var toolsSession = ToolsSession()

    init() {
        // After a forced quit AppKit can stop in window restoration and show no
        // window. The app keeps its own tab, folder and window frame.
        UserDefaults.standard.register(defaults: ["ApplePersistenceIgnoreState": true])
        BrandFonts.register()
    }
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
                .frame(minWidth: WindowSizes.minimum.width, minHeight: WindowSizes.minimum.height)
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
                    EngineModelInstall.engine = engine
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
                    #if DEBUG
                    if SelfShot.isActive { return }
                    #endif
                    if !welcomeSheetSeen { showWelcome = true }
                }
                .sheet(isPresented: $showWelcome) {
                    WelcomeSheet(engine: engine)
                        .onDisappear { welcomeSheetSeen = true }
                }
                #if DEBUG
                .onReceive(NotificationCenter.default.publisher(for: .selfShotAction)) { note in
                    guard let action = note.object as? String else { return }
                    if action == "welcome-download" {
                        let kind = AIModelKind.safeDefaultFor(ramGB: Double(ProcessInfo.processInfo.physicalMemory) / 1_073_741_824.0, freeDiskBytes: nil)
                        SelfShot.welcomeDemoDownload = true
                        engine.selfShotSetModelProgress(ModelDownloadProgress(modelKind: kind.rawValue, fraction: 0.04, message: "Downloading \(kind.displayName)… 4%", bytesDone: 230_000_000, totalBytes: 5_800_000_000))
                        showWelcome = true
                    } else if action == "welcome" || action == "close" {
                        showWelcome = action == "welcome"
                    }
                }
                #endif
        }
        .windowStyle(.hiddenTitleBar)
        .defaultSize(width: WindowSizes.firstLaunch.width, height: WindowSizes.firstLaunch.height)
        .commands {
            SidebarCommands()
            CommandGroup(replacing: .newItem) {
                Button("Choose Folder…") { NotificationCenter.default.post(name: .fileIDChooseFolder, object: nil) }
                    .keyboardShortcut("o", modifiers: .command)
            }
            CommandGroup(after: .sidebar) {
                Button("Show Chat Beside Page") { NotificationCenter.default.post(name: .fileIDToggleChat, object: nil) }
                    .keyboardShortcut("k", modifiers: [.command, .shift])
                Button("Pause or Resume Lava Lamp") { NotificationCenter.default.post(name: .fileIDToggleMotion, object: nil) }
                    .keyboardShortcut("m", modifiers: [.command, .shift])
            }
            CommandGroup(after: .textEditing) {
                Button("Find") { NotificationCenter.default.post(name: .fileIDFindInPage, object: nil) }
                    .keyboardShortcut("f", modifiers: .command)
            }
            CommandMenu("Go") {
                Button("Search or Ask…") { NotificationCenter.default.post(name: .fileIDFocusSearch, object: nil) }
                    .keyboardShortcut("k", modifiers: .command)
                Divider()
                ForEach(Array(MainWindow.Tab.order.enumerated()), id: \.offset) { index, tab in
                    Button(tab.title) { NotificationCenter.default.post(name: .fileIDGoToTab, object: tab.rawValue) }
                        .keyboardShortcut(KeyEquivalent(Character(String(index + 1))), modifiers: .command)
                }
                Divider()
                Button(MainWindow.Tab.settings.title) { NotificationCenter.default.post(name: .fileIDGoToTab, object: MainWindow.Tab.settings.rawValue) }
                    .keyboardShortcut(",", modifiers: .command)
            }
            CommandGroup(replacing: .help) {
                Button("FileID Help") { NSWorkspace.shared.open(URL(string: "https://github.com/WebWorldWide/FileID#readme")!) }
                Button("Set Up AI Models…") { showWelcome = true }
                Divider()
                Button("Privacy on This Mac") {
                    let alert = NSAlert()
                    alert.messageText = "Everything stays on this Mac"
                    alert.informativeText = "FileID reads your files on this Mac and never sends them, or anything about how you use it, anywhere. The only time it uses the internet is when you choose to download an AI model.\n\nFile names, tags and descriptions also show up in Spotlight (⌘Space) on this Mac. To stop that, turn off “Show results in Spotlight” in Settings."
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
                    alert.informativeText = "Version \(version) (build \(build))\nOrganizes your photos, videos and documents with AI that runs on this Mac. Your files never leave it."
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
    static let fileIDGoToTab = Notification.Name("FileID.goToTab")
    static let fileIDToggleChat = Notification.Name("FileID.toggleChat")
    static let fileIDToggleMotion = Notification.Name("FileID.toggleMotion")
    static let fileIDFocusSearch = Notification.Name("FileID.focusSearch")
    /// Edit ▸ Find (⌘F): the visible page focuses its own search field.
    static let fileIDFindInPage = Notification.Name("FileID.findInPage")
}
