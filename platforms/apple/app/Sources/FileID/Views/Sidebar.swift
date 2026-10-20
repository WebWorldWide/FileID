// Sidebar: every page in one calm list (grouped Browse / Analyze / Act), then
// a compact scan card. Mirrors the old sidebar's simplicity in the v3 look.
import SwiftUI
import FileIDShared

struct Sidebar: View {
    let engine: EngineClient
    let store: ReadStore
    @Binding var activeTab: MainWindow.Tab
    @Binding var pickedURL: URL?
    var pickFolder: () -> Void

    @State private var shownArea: MainWindow.Area = .browse
    @Namespace private var areaPill

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            areaSwitcher
                .padding(.horizontal, 10).padding(.top, 12).padding(.bottom, 10)
            ScrollView(.vertical, showsIndicators: false) {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(shownArea.tabs) { tab in navRow(tab) }
                }
                .padding(.bottom, 10)
                .id(shownArea)
                .transition(.opacity)
            }
            .frame(maxHeight: .infinity, alignment: .top)

            if shouldShowEngineSection {
                VStack(alignment: .leading, spacing: 8) {
                    EngineStatusRow(state: engine.state)
                    if let err = engine.lastError { engineErrorRow(err) }
                }
                .padding(.horizontal, 12).padding(.bottom, 10)
            }

            if !engine.queueState.pending.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    SectionLabel("Queue")
                    QueueListView(state: engine.queueState)
                }
                .padding(.horizontal, 14).padding(.bottom, 10)
            }

            Group {
                if let url = pickedURL {
                    PipelineBlock(engine: engine, store: store, pickedURL: url)
                } else {
                    VStack(alignment: .leading, spacing: 8) {
                        Text("Pick a folder to start").font(Theme.Font.body(13, .semibold)).foregroundStyle(Theme.text1)
                        Button { pickFolder() } label: { Label("Pick a folder…", systemImage: "folder.badge.plus") }
                            .buttonStyle(DSPrimaryButtonStyle(height: 30))
                            .keyboardShortcut("o", modifiers: .command)
                        MetaText("Or drop a folder on the window.")
                    }
                }
            }
            .padding(12)
            .background(Color.white.opacity(0.035), in: RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.Radius.l, style: .continuous).stroke(Theme.hairline, lineWidth: 1))
            .padding(10)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear { shownArea = activeTab.area ?? .browse }
        .onChange(of: activeTab) { _, tab in if let area = tab.area, area != shownArea { withAnimation(Theme.quick) { shownArea = area } } }
        .background(Theme.bg1.opacity(0.55), ignoresSafeAreaEdges: [])
        .background(.ultraThinMaterial, ignoresSafeAreaEdges: [])
    }

    /// Browse / Analyze / Act: picks which pages the list shows and opens the
    /// page last used in that area.
    private var areaSwitcher: some View {
        HStack(spacing: 2) {
            ForEach(MainWindow.Area.allCases) { area in
                let on = shownArea == area
                Button {
                    withAnimation(Theme.spring) { shownArea = area }
                    activeTab = area.remembered(current: activeTab)
                } label: {
                    Text(area.rawValue)
                        .font(Theme.Font.body(12.5, on ? .semibold : .medium))
                        .foregroundStyle(on ? Theme.onGold : Theme.text2)
                        .frame(maxWidth: .infinity)
                        .frame(height: 26)
                        .background {
                            if on {
                                RoundedRectangle(cornerRadius: Theme.Radius.s, style: .continuous)
                                    .fill(Theme.gold)
                                    .matchedGeometryEffect(id: "pill", in: areaPill)
                            }
                        }
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("\(area.rawValue) pages (\(area.shortcutHint))")
                .accessibilityLabel("\(area.rawValue) pages")
                .accessibilityAddTraits(on ? [.isSelected, .isButton] : [.isButton])
            }
        }
        .padding(3)
        .background(Color.black.opacity(0.28), in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.hairlineStrong, lineWidth: 1))
    }

    private var shouldShowEngineSection: Bool {
        if engine.lastError != nil { return true }
        switch engine.state {
        case .starting, .crashed: return true
        case .ready:              return false
        }
    }

    /// Short trailing badge; nil when there is nothing worth saying.
    private func badge(for tab: MainWindow.Tab) -> (text: String, color: Color)? {
        switch tab {
        case .library:
            if let p = engine.lastProgress, p.phase == .discovering || p.phase == .tagging {
                let total = max(p.total, p.discovered)
                return (total > 0 ? "\(Int(Double(p.processed) / Double(total) * 100))%" : "…", Theme.gold)
            }
            return store.totalFiles > 0 ? (compact(store.totalFiles), Theme.text3) : nil
        case .people:
            if engine.faceClusteringInFlight { return ("…", Theme.ai) }
            if let r = engine.lastFaceClustering, r.personCount > 0 { return (compact(r.personCount), Theme.text3) }
            if store.totalFiles > 0, !isFaceModelInstalled { return ("Setup", Theme.warning) }
            return nil
        case .deep:
            if engine.deepAnalyzeInFlight, let d = engine.deepAnalyzeProgress, d.total > 0 {
                return ("\(Int(Double(d.processed) / Double(d.total) * 100))%", Theme.ai)
            }
            return nil
        case .moments:
            let running = engine.catalogJobs.filter { $0.state == "running" }.count
            return running > 0 ? ("\(running)", Theme.ai) : nil
        case .restructure:
            return engine.restructurePlan != nil ? ("Plan", Theme.gold) : nil
        default:
            return nil
        }
    }

    private var isFaceModelInstalled: Bool {
        if case .installed = ArcFaceModelInstaller.shared.status[.sface] ?? .unknown { return true }
        return false
    }

    private func compact(_ n: Int) -> String {
        n >= 10_000 ? String(format: "%.0fk", Double(n) / 1000) : n.formatted()
    }

    @ViewBuilder
    private func navRow(_ tab: MainWindow.Tab) -> some View {
        let on = activeTab == tab
        Button { activeTab = tab } label: {
            HStack(spacing: 10) {
                Image(systemName: tab.systemImage)
                    .font(.system(size: 13, weight: on ? .semibold : .regular))
                    .foregroundStyle(on ? Theme.gold : Theme.text2)
                    .frame(width: 18)
                Text(tab.rawValue)
                    .font(Theme.Font.body(13.5, on ? .semibold : .regular))
                    .foregroundStyle(on ? Theme.gold : Theme.text1)
                    .lineLimit(1)
                Spacer(minLength: 4)
                if let b = badge(for: tab) {
                    Text(b.text).font(Theme.Font.body(11.5, .medium)).foregroundStyle(b.color).monospacedDigit()
                }
            }
            .padding(.horizontal, 10)
            .frame(height: 32)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(on ? Theme.gold.opacity(0.12) : Color.clear, in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        }
        .buttonStyle(.dsRow)
        .padding(.horizontal, 8)
        .help(tab.shortcutIndex.map { "\(tab.rawValue) (⌘\($0))" } ?? tab.rawValue)
        .accessibilityLabel("\(tab.rawValue) page")
        .accessibilityAddTraits(on ? [.isSelected, .isButton] : [.isButton])
        .animation(Theme.quick, value: on)
    }

    @ViewBuilder
    private func engineErrorRow(_ err: EngineError) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(Theme.warning)
            VStack(alignment: .leading, spacing: 2) {
                Text(err.kind).font(Theme.Font.body(12, .semibold)).foregroundStyle(Theme.text1)
                Text(err.message).font(Theme.Font.body(11.5)).foregroundStyle(Theme.text3).lineLimit(3)
            }
            Spacer(minLength: 4)
            DSIconButton("Dismiss error", systemImage: "xmark", tint: Theme.text3) { engine.clearLastError() }
        }
        .padding(8)
        .background(Theme.warning.opacity(0.08), in: RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.m, style: .continuous).stroke(Theme.warning.opacity(0.35), lineWidth: 1))
    }
}
