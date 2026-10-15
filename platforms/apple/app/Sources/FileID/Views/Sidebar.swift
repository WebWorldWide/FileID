// Sidebar: the pages of the current area, then the pipeline block.
import SwiftUI
import FileIDShared

struct Sidebar: View {
    let engine: EngineClient
    let store: ReadStore
    @Binding var activeTab: MainWindow.Tab
    @Binding var pickedURL: URL?
    var pickFolder: () -> Void

    private var area: MainWindow.Area { activeTab.area ?? .browse }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            ScrollView(.vertical, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 0) {
                    ForEach(MainWindow.Area.allCases) { section in
                        HStack(alignment: .firstTextBaseline) {
                            SectionLabel(section.rawValue, color: section == area && activeTab != .settings ? Theme.gold : Theme.label)
                            Spacer()
                            MetaText(section.shortcutHint, size: 11)
                        }
                        .padding(.horizontal, 16).padding(.top, section == .browse ? 16 : 14).padding(.bottom, 8)
                        VStack(spacing: 0) {
                            ForEach(section.tabs) { tab in navRow(tab) }
                        }
                        .overlay(alignment: .top) { Hairline(color: Theme.hairlineSoft) }
                    }
                }
            }
            .frame(maxHeight: .infinity, alignment: .top)

            if shouldShowEngineSection {
                VStack(alignment: .leading, spacing: 8) {
                    EngineStatusRow(state: engine.state)
                    if let err = engine.lastError { engineErrorRow(err) }
                }
                .padding(.horizontal, 14).padding(.vertical, 12)
            }

            if !engine.queueState.pending.isEmpty {
                VStack(alignment: .leading, spacing: 8) {
                    SectionLabel("Queue")
                    QueueListView(state: engine.queueState)
                }
                .padding(.horizontal, 14).padding(.vertical, 12)
                .overlay(alignment: .top) { Hairline(color: Theme.hairlineSoft) }
            }

            VStack(alignment: .leading, spacing: 10) {
                if let url = pickedURL {
                    PipelineBlock(engine: engine, store: store, pickedURL: url)
                } else {
                    SectionLabel("Get started")
                    Button { pickFolder() } label: { Label("Pick a folder…", systemImage: "folder.badge.plus") }
                        .buttonStyle(.dsPrimary)
                        .keyboardShortcut("o", modifiers: .command)
                    MetaText("Or drop a folder on the window.")
                }
            }
            .padding(.horizontal, 16).padding(.top, 16).padding(.bottom, 18)
            .overlay(alignment: .top) { Hairline() }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Theme.bg1.opacity(0.5), ignoresSafeAreaEdges: [])
        .background(.ultraThinMaterial, ignoresSafeAreaEdges: [])
    }

    private var shouldShowEngineSection: Bool {
        if engine.lastError != nil { return true }
        switch engine.state {
        case .starting, .crashed: return true
        case .ready:              return false
        }
    }

    private func meta(for tab: MainWindow.Tab) -> (text: String, hot: Bool) {
        switch tab {
        case .library:
            if let p = engine.lastProgress, p.phase == .discovering || p.phase == .tagging {
                return ("\(p.processed.formatted()) of \(max(p.total, p.discovered).formatted()) files", true)
            }
            return (store.totalFiles > 0 ? "\(store.totalFiles.formatted()) files" : "Not scanned yet", false)
        case .people:
            if engine.faceClusteringInFlight { return ("Grouping faces", true) }
            if let r = engine.lastFaceClustering, r.personCount > 0 { return ("\(r.personCount) people", false) }
            if case .installed = ArcFaceModelInstaller.shared.status[.sface] ?? .unknown {
                return (store.totalFiles > 0 ? "Group faces to name people" : "Faces appear after a scan", false)
            }
            return (store.totalFiles > 0 ? "Face model not installed" : "Faces appear after a scan", store.totalFiles > 0)
        case .moments:
            let running = engine.catalogJobs.filter { $0.state == "running" }.count
            return running > 0 ? ("\(running) running", true) : ("Chapters and search inside videos", false)
        case .deep:
            if engine.deepAnalyzeInFlight, let d = engine.deepAnalyzeProgress, d.total > 0 {
                return ("\(Int(Double(d.processed) / Double(d.total) * 100))% · running", true)
            }
            if let done = engine.deepAnalyzeComplete, done.processed > 0 { return ("\(done.processed.formatted()) captioned", false) }
            return ("Captions and names", false)
        case .chat:        return ("Ask about your files", false)
        case .takes:       return ("Pick the best attempt", false)
        case .cleanup:     return ("Find extra copies", false)
        case .restructure: return (engine.restructurePlan == nil ? "Preview a layout" : "Plan ready", engine.restructurePlan != nil)
        case .tools:       return ("Convert, crop, export", false)
        case .settings:    return ("", false)
        }
    }

    @ViewBuilder
    private func navRow(_ tab: MainWindow.Tab) -> some View {
        let on = activeTab == tab
        let m = meta(for: tab)
        Button { activeTab = tab } label: {
            HStack(spacing: 12) {
                Rectangle().fill(on ? Theme.gold : Color.clear).frame(width: 3)
                VStack(alignment: .leading, spacing: 3) {
                    Text(tab.rawValue).font(Theme.Font.body(14, on ? .semibold : .medium)).foregroundStyle(on ? Theme.gold : Theme.text1)
                    Text(m.text).font(Theme.Font.meta).foregroundStyle(m.hot ? (tab == .deep ? Theme.ai : Theme.textOnPanel) : Theme.text3).lineLimit(1)
                }
                Spacer(minLength: 0)
                if let index = tab.shortcutIndex { MetaText("⌘\(index)", size: 11) }
            }
            .padding(.trailing, 14)
            .frame(height: 44)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(on ? Theme.goldWash : Color.clear, ignoresSafeAreaEdges: [])
            .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(tab.rawValue) page")
        .accessibilityAddTraits(on ? [.isSelected, .isButton] : [.isButton])
    }

    @ViewBuilder
    private func engineErrorRow(_ err: EngineError) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.warning)
            VStack(alignment: .leading, spacing: 2) {
                Text(err.kind).font(Theme.Font.metaStrong).foregroundStyle(Theme.text1)
                Text(err.message).font(Theme.Font.meta).foregroundStyle(Theme.text3).lineLimit(3)
            }
            Spacer(minLength: 4)
            Button { engine.clearLastError() } label: { Image(systemName: "xmark").foregroundStyle(Theme.text3) }
                .buttonStyle(.plain).help("Dismiss")
        }
        .padding(8)
        .background(Theme.warning.opacity(0.08), ignoresSafeAreaEdges: [])
        .overlay(Rectangle().stroke(Theme.warning.opacity(0.4), lineWidth: 1))
    }
}
