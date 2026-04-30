// Root window: hand-rolled HStack split over LavaLamp.
import SwiftUI
import FileIDShared

struct MainWindow: View {
    let engine: EngineClient
    @State private var store = ReadStore()
    @AppStorage("activeTabRawValue") private var activeTabRaw: String = Tab.library.rawValue
    @State private var pickedURL: URL?

    /// Computed wrapper around the persisted raw string so the rest of
    /// the code keeps working with `Tab`. Falls back to `.library` on
    /// any decoding failure.
    private var activeTabBinding: Binding<Tab> {
        Binding(
            get: { Tab(rawValue: activeTabRaw) ?? .library },
            set: { activeTabRaw = $0.rawValue }
        )
    }
    private var activeTab: Tab { Tab(rawValue: activeTabRaw) ?? .library }
    @State private var isDragHovering = false
    /// HStack split avoids NavigationSplitView's auto-inserted toolbar,
    /// which renders an unsuppressible white strip in full-screen mode.
    /// Persisted across launches.
    @AppStorage("sidebarVisible") private var sidebarVisible: Bool = true
    private let sidebarWidth: CGFloat = 260

    private static let pickedFolderBookmarkKey = "pickedFolderBookmark.v2"

    /// Order matches the workflow taught by the onboarding splash:
    /// Browse → Identify people → Dedupe → Caption → Reorganize → Settings.
    /// Review was folded into Settings → Advanced.
    enum Tab: String, CaseIterable, Identifiable {
        case library     = "Library"
        case people      = "People"
        case cleanup     = "Cleanup"
        case deep        = "Deep Analyze"
        case restructure = "Restructure"
        case settings    = "Settings"
        var id: String { rawValue }
        var systemImage: String {
            switch self {
            case .library:     return "photo.on.rectangle"
            case .people:      return "person.2.crop.square.stack"
            case .cleanup:     return "trash.slash"
            // text.below.photo signals "AI writes text about an image" —
            // distinct from the sparkles used by the People-tab "Suggest
            // merges" button + Restructure header.
            case .deep:        return "text.below.photo"
            case .restructure: return "rectangle.3.offgrid"
            case .settings:    return "gearshape"
            }
        }
    }

    var body: some View {
        ZStack {
            LavaLampBackground()
                .ignoresSafeArea()
            HStack(spacing: 0) {
                if sidebarVisible {
                    Sidebar(engine: engine, store: store,
                            activeTab: activeTabBinding,
                            pickedURL: $pickedURL,
                            sidebarVisible: $sidebarVisible)
                        .frame(width: sidebarWidth)
                        .preferredColorScheme(.dark)
                        .transition(.move(edge: .leading).combined(with: .opacity))
                    Divider()
                        .background(Color.white.opacity(0.08))
                }
                Detail(engine: engine, store: store, activeTab: activeTab,
                       pickedURL: $pickedURL,
                       sidebarVisible: $sidebarVisible,
                       onSwitchTab: { activeTabRaw = $0.rawValue })
                    .preferredColorScheme(.dark)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .accentColor(Theme.gold)
            .onAppear {
                store.openIfPossible()
                restorePickedFolderIfPossible()
            }
            .onChange(of: pickedURL) { _, newValue in
                persistPickedFolder(newValue)
            }
            // Auto-tab-switch — guide the user through the workflow as
            // each stage finishes. Uses `.task(id:)` instead of
            // `.onChange(of:)` because optional-keypath expressions
            // (`?.personCount ?? -1`) don't reliably trigger SwiftUI
            // onChange in the @Observable / Swift 6 strict-concurrency
            // setup — the keypath race-evaluates as nil on the first
            // observation cycle and the closure never fires. `.task(id:)`
            // is a hard signal that re-runs whenever the id transitions.
            .task(id: engine.lastFaceClustering?.personCount ?? -1) {
                guard let result = engine.lastFaceClustering,
                      result.personCount > 0,
                      activeTab == .library else { return }
                withAnimation(.easeInOut(duration: 0.25)) {
                    activeTabRaw = Tab.people.rawValue
                }
            }
            .task(id: engine.deepAnalyzeComplete?.processed ?? -1) {
                guard let done = engine.deepAnalyzeComplete,
                      done.processed > 0,
                      activeTab == .deep else { return }
                withAnimation(.easeInOut(duration: 0.25)) {
                    activeTabRaw = Tab.library.rawValue
                }
            }

            if isDragHovering {
                RoundedRectangle(cornerRadius: 20)
                    .stroke(Theme.gold, style: StrokeStyle(lineWidth: 3, dash: [12, 6]))
                    .background(Theme.gold.opacity(0.08))
                    .overlay(
                        VStack(spacing: 12) {
                            Image(systemName: "arrow.down.doc.fill")
                                .font(.system(size: 48))
                                .foregroundStyle(Theme.gold)
                            Text("Drop Folder to Scan")
                                .font(.title3.bold())
                                .foregroundStyle(Theme.gold)
                        }
                    )
                    .padding(20)
                    .allowsHitTesting(false)
            }
        }
        .onDrop(of: [.fileURL], isTargeted: $isDragHovering) { providers in
            guard let provider = providers.first else { return false }
            _ = provider.loadObject(ofClass: URL.self) { url, _ in
                guard let url else { return }
                var isDir: ObjCBool = false
                if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue {
                    DispatchQueue.main.async {
                        if pickedURL != url {
                            engine.clearProgress()
                        }
                        pickedURL = url
                    }
                }
            }
            return true
        }
    }

    // MARK: - Folder persistence

    private func persistPickedFolder(_ url: URL?) {
        guard let url else {
            UserDefaults.standard.removeObject(forKey: Self.pickedFolderBookmarkKey)
            return
        }
        // bookmarkData can do filesystem I/O on slow disks / network
        // volumes, hanging the main thread for seconds. Off-thread.
        let key = Self.pickedFolderBookmarkKey
        Task.detached(priority: .utility) {
            if let data = try? url.bookmarkData(options: [],
                                                  includingResourceValuesForKeys: nil,
                                                  relativeTo: nil) {
                await MainActor.run {
                    UserDefaults.standard.set(data, forKey: key)
                }
            }
        }
    }

    private func restorePickedFolderIfPossible() {
        guard pickedURL == nil,
              let data = UserDefaults.standard.data(forKey: Self.pickedFolderBookmarkKey)
        else { return }
        // Resolve off-thread too — URL(resolvingBookmarkData:) can
        // round-trip to disk (slow first-launch when the previous
        // folder lives on a sleeping NAS).
        let key = Self.pickedFolderBookmarkKey
        Task.detached(priority: .utility) {
            do {
                var stale = false
                let url = try URL(resolvingBookmarkData: data, options: [],
                                    relativeTo: nil, bookmarkDataIsStale: &stale)
                var isDir: ObjCBool = false
                let exists = FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir) && isDir.boolValue
                await MainActor.run {
                    if exists {
                        self.pickedURL = url
                    } else {
                        UserDefaults.standard.removeObject(forKey: key)
                    }
                }
            } catch {
                await MainActor.run {
                    UserDefaults.standard.removeObject(forKey: key)
                }
            }
        }
    }
}
