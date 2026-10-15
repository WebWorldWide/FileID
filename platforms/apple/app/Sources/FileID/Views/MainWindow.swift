// Root window: Prototype v3 shell. Title bar over a hairline-split
// sidebar + page, all on the LavaLamp.
import SwiftUI
import AppKit
import FileIDShared

struct MainWindow: View {
    let engine: EngineClient
    @State private var store = ReadStore()
    @State private var toolsSession = ToolsSession()
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var atmospherePaused = false
    @AppStorage("activeTabRawValue") private var activeTabRaw: String = Tab.library.rawValue
    @AppStorage("sidebarVisible") private var sidebarVisible: Bool = true
    @State private var pickedURL: URL?
    @State private var isDragHovering = false
    private let toasts = ToastCenter.shared

    /// Ten pages in three areas plus Settings. Raw values are persisted,
    /// so the legacy names stay stable.
    enum Tab: String, CaseIterable, Identifiable {
        case library     = "Library"
        case people      = "People"
        case moments     = "Search & Moments"
        case deep        = "Deep Analyze"
        case chat        = "Chat"
        case takes       = "Best Takes"
        case cleanup     = "Cleanup"
        case restructure = "Restructure"
        case tools       = "File Tools"
        case settings    = "Settings"
        var id: String { rawValue }

        static let order: [Tab] = [.library, .people, .moments, .deep, .chat, .takes, .cleanup, .restructure, .tools]

        var area: Area? { Area.allCases.first { $0.tabs.contains(self) } }
        var shortcutIndex: Int? { Self.order.firstIndex(of: self).map { $0 + 1 } }

        var systemImage: String {
            switch self {
            case .library:     return "photo.on.rectangle"
            case .people:      return "person.2.crop.square.stack"
            case .moments:     return "sparkle.magnifyingglass"
            case .deep:        return "text.below.photo"
            case .chat:        return "bubble.left.and.bubble.right"
            case .takes:       return "film.stack"
            case .cleanup:     return "trash.slash"
            case .restructure: return "rectangle.3.offgrid"
            case .tools:       return "wand.and.stars"
            case .settings:    return "gearshape"
            }
        }
    }

    enum Area: String, CaseIterable, Identifiable {
        case browse = "Browse", analyze = "Analyze", act = "Act"
        var id: String { rawValue }
        var tabs: [Tab] {
            switch self {
            case .browse:  return [.library, .people, .moments]
            case .analyze: return [.deep, .chat, .takes]
            case .act:     return [.cleanup, .restructure, .tools]
            }
        }
        /// Switching areas returns to the page last used in that area.
        func remembered(current: Tab) -> Tab {
            if tabs.contains(current) { return current }
            let key = "lastTab.\(rawValue)"
            if let raw = UserDefaults.standard.string(forKey: key), let tab = Tab(rawValue: raw), tabs.contains(tab) { return tab }
            return tabs[0]
        }
    }

    private var activeTabBinding: Binding<Tab> {
        Binding(get: { Tab(rawValue: activeTabRaw) ?? .library },
                set: { newValue in
                    activeTabRaw = newValue.rawValue
                    if let area = newValue.area { UserDefaults.standard.set(newValue.rawValue, forKey: "lastTab.\(area.rawValue)") }
                })
    }
    private var activeTab: Tab { Tab(rawValue: activeTabRaw) ?? .library }

    private var aiActive: Bool {
        engine.deepAnalyzeInFlight || engine.faceClusteringInFlight || engine.catalogJobs.contains { $0.state == "running" }
    }

    var body: some View {
        ZStack {
            LavaLampBackground(paused: atmospherePaused, aiActive: aiActive)
                .ignoresSafeArea()

            VStack(spacing: 0) {
                TitleBar(engine: engine, store: store, activeTab: activeTabBinding, pickedURL: $pickedURL,
                         sidebarVisible: $sidebarVisible, pickFolder: pickFolder)
                HStack(spacing: 0) {
                    if sidebarVisible {
                        Sidebar(engine: engine, store: store, activeTab: activeTabBinding, pickedURL: $pickedURL, pickFolder: pickFolder)
                            .frame(width: Theme.Metrics.sidebar)
                            .transition(.move(edge: .leading).combined(with: .opacity))
                        VHairline()
                    }
                    Detail(engine: engine, store: store, toolsSession: toolsSession, activeTab: activeTab,
                           pickedURL: $pickedURL, onSwitchTab: { activeTabBinding.wrappedValue = $0 }, pickFolder: pickFolder)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .preferredColorScheme(.dark)
            .animation(reduceMotion ? nil : .spring(response: 0.38, dampingFraction: 0.8), value: sidebarVisible)
            .accentColor(Theme.gold)
            .background { shortcutHost }
            .onAppear {
                store.openIfPossible()
                restorePickedFolderIfPossible()
            }
            .onChange(of: pickedURL) { _, newValue in persistPickedFolder(newValue) }
            // Guide the user through the workflow as each stage finishes.
            .task(id: engine.lastFaceClustering?.personCount ?? -1) {
                guard let result = engine.lastFaceClustering, result.personCount > 0, activeTab == .library else { return }
                toasts.show("Scan complete · \(result.personCount) people grouped", actionLabel: "Open People", duration: 7) {
                    activeTabBinding.wrappedValue = .people
                }
            }
            .task(id: engine.deepAnalyzeComplete?.processed ?? -1) {
                guard let done = engine.deepAnalyzeComplete, done.processed > 0, activeTab == .deep else { return }
                toasts.show("Deep Analyze finished · \(done.processed.formatted()) files", actionLabel: "Open Library", duration: 7) {
                    activeTabBinding.wrappedValue = .library
                }
            }

            if let toast = toasts.current {
                VStack { Spacer(); ToastView(toast: toast).padding(.bottom, 22) }
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .allowsHitTesting(true)
            }

            if isDragHovering {
                Rectangle()
                    .stroke(Theme.gold, style: StrokeStyle(lineWidth: 2, dash: [10, 6]))
                    .background(Theme.gold.opacity(0.08))
                    .overlay(
                        VStack(spacing: 12) {
                            Image(systemName: "arrow.down.doc.fill").font(.system(size: 40)).foregroundStyle(Theme.gold)
                            Text("Drop folder to scan").font(Theme.Font.display(28)).foregroundStyle(Theme.gold)
                        }
                    )
                    .padding(14)
                    .allowsHitTesting(false)
            }
        }
        .animation(reduceMotion ? nil : .easeOut(duration: 0.2), value: toasts.current?.id)
        .environment(\.atmospherePaused, atmospherePaused)
        .onDrop(of: [.fileURL], isTargeted: $isDragHovering) { providers in
            guard let provider = providers.first else { return false }
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                guard let url else { return }
                var isDir: ObjCBool = false
                guard FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue else { return }
                guard FileManager.default.isReadableFile(atPath: url.path) else {
                    let folderName = url.lastPathComponent
                    DispatchQueue.main.async {
                        let alert = NSAlert()
                        alert.messageText = "Folder isn't readable"
                        alert.informativeText = "\"\(folderName)\" can't be read. If it's a network share, mount it and try again. If it's a system folder, pick a folder you own."
                        alert.alertStyle = .warning
                        alert.runModal()
                    }
                    return
                }
                DispatchQueue.main.async {
                    if pickedURL != url { engine.clearProgress() }
                    pickedURL = url
                }
            }
            return true
        }
    }

    /// ⌘1…⌘9 jump to pages; invisible buttons carry the shortcuts.
    private var shortcutHost: some View {
        Group {
            ForEach(Array(Tab.order.enumerated()), id: \.offset) { index, tab in
                Button("") { activeTabBinding.wrappedValue = tab }
                    .keyboardShortcut(KeyEquivalent(Character(String(index + 1))), modifiers: .command)
            }
            Button("") { atmospherePaused.toggle() }.keyboardShortcut("m", modifiers: [.command, .shift])
        }
        .opacity(0)
        .frame(width: 0, height: 0)
        .accessibilityHidden(true)
    }

    // MARK: - Folder picking

    func pickFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Pick a folder to scan"
        if panel.runModal() == .OK, let url = panel.url {
            guard FileManager.default.isReadableFile(atPath: url.path) else {
                let alert = NSAlert()
                alert.messageText = "Can't read \(url.lastPathComponent)"
                alert.informativeText = "FileID doesn't have permission to read this folder. Pick a different folder or grant Full Disk Access in System Settings → Privacy & Security."
                alert.alertStyle = .warning
                alert.addButton(withTitle: "OK")
                alert.runModal()
                return
            }
            if pickedURL != url { engine.clearProgress() }
            pickedURL = url
        }
    }

    // MARK: - Folder persistence

    private func persistPickedFolder(_ url: URL?) {
        guard let url else {
            UserDefaults.standard.removeObject(forKey: SecurityScopedBookmark.pickedFolderDefaultsKey)
            SecurityScopedBookmark.releaseRetainedAccess()
            return
        }
        let key = SecurityScopedBookmark.pickedFolderDefaultsKey
        Task.detached(priority: .utility) {
            if let data = try? SecurityScopedBookmark.make(for: url) {
                guard SecurityScopedBookmark.retainAccess(for: data) != nil else { return }
                await MainActor.run { UserDefaults.standard.set(data, forKey: key) }
            }
        }
    }

    private func restorePickedFolderIfPossible() {
        guard pickedURL == nil,
              let data = UserDefaults.standard.data(forKey: SecurityScopedBookmark.pickedFolderDefaultsKey)
        else { return }
        let key = SecurityScopedBookmark.pickedFolderDefaultsKey
        Task.detached(priority: .utility) {
            guard let accessibleURL = SecurityScopedBookmark.retainAccess(for: data) else {
                await MainActor.run {
                    guard self.pickedURL == nil else { return }
                    UserDefaults.standard.removeObject(forKey: key)
                }
                return
            }
            var isDir: ObjCBool = false
            let exists = FileManager.default.fileExists(atPath: accessibleURL.path, isDirectory: &isDir) && isDir.boolValue
            await MainActor.run {
                guard self.pickedURL == nil else { return }
                if exists { self.pickedURL = accessibleURL } else { UserDefaults.standard.removeObject(forKey: key) }
            }
        }
    }
}
