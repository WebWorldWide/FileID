// Sidebar: folder pick, tab nav, live Processing Control, queue, engine status.
import SwiftUI
import FileIDShared

struct Sidebar: View {
    let engine: EngineClient
    let store: ReadStore
    @Binding var activeTab: MainWindow.Tab
    @Binding var pickedURL: URL?
    /// Bound from MainWindow's HStack split.
    @Binding var sidebarVisible: Bool

    var body: some View {
        // Plain ScrollView + VStack: List + Section regressed to blank
        // sidebar in production, primitive layout is stable.
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                sidebarSection(pickedURL == nil ? "GET STARTED" : "FOLDER") {
                    folderRow.padding(.horizontal, 12)
                }

                // Tabs are obviously navigation — no header needed. The
                // "Pick a folder…" hint covers the disabled case.
                Group {
                    if pickedURL == nil {
                        Text("Pick a folder above to enable tabs")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, 12)
                    } else {
                        VStack(alignment: .leading, spacing: 2) {
                            ForEach(MainWindow.Tab.allCases) { tab in
                                navRow(tab)
                            }
                        }
                        .padding(.horizontal, 8)
                    }
                }

                if let url = pickedURL {
                    sidebarSection("SCAN CONTROL") {
                        ProcessingControl(engine: engine, store: store,
                                           pickedURL: url,
                                           changePickedURL: { pickedURL = $0 })
                            .padding(.horizontal, 12)
                    }
                }

                if !engine.queueState.pending.isEmpty {
                    sidebarSection("QUEUE") {
                        QueueListView(state: engine.queueState)
                            .padding(.horizontal, 12)
                    }
                }

                // Engine status pill is only worth showing when something
                // is wrong (crash, error). When everything is healthy the
                // sidebar stays clean — fewer dividers, less visual noise.
                if shouldShowEngineSection {
                    sidebarSection("ENGINE") {
                        VStack(alignment: .leading, spacing: 8) {
                            EngineStatusRow(state: engine.state)
                            if let err = engine.lastError {
                                engineErrorRow(err)
                            }
                        }
                        .padding(.horizontal, 12)
                    }
                }
            }
            .padding(.vertical, 16)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(Color.black.opacity(0.35))
        // 28 pt strip for traffic-light buttons + the in-sidebar collapse
        // toggle (replaces the system toolbar we removed for full-screen).
        .safeAreaInset(edge: .top, spacing: 0) {
            HStack(spacing: 0) {
                Spacer()
                Button {
                    withAnimation(.easeInOut(duration: 0.2)) {
                        sidebarVisible = false
                    }
                } label: {
                    Image(systemName: "sidebar.left")
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(.secondary)
                        .frame(width: 28, height: 28)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .help("Hide sidebar (⌃⌘S)")
                .keyboardShortcut("s", modifiers: [.control, .command])
                .padding(.trailing, 4)
            }
            .frame(height: 28)
        }
    }

    /// Show the engine pill only when there's something interesting:
    /// not yet ready (starting / crashed) or an error worth surfacing.
    /// "Engine: Ready" is noise — the running scan already proves it.
    private var shouldShowEngineSection: Bool {
        if engine.lastError != nil { return true }
        switch engine.state {
        case .starting, .crashed: return true
        case .ready:              return false
        }
    }

    @ViewBuilder
    private func sidebarSection<Content: View>(_ title: String,
                                                @ViewBuilder content: () -> Content)
        -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.system(size: 10, weight: .bold))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 12)
            content()
        }
    }

    @ViewBuilder
    private func navRow(_ tab: MainWindow.Tab) -> some View {
        let active = activeTab == tab
        Button { activeTab = tab } label: {
            Label(tab.rawValue, systemImage: tab.systemImage)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.vertical, 6)
                .padding(.horizontal, 12)
                .background(
                    RoundedRectangle(cornerRadius: 6)
                        .fill(active ? Theme.gold.opacity(0.18) : Color.clear)
                )
                .accessibilityLabel("\(tab.rawValue) tab")
                .accessibilityAddTraits(active ? [.isSelected, .isButton] : [.isButton])
                .accessibilityHint(active ? "Currently selected" : "Switches to \(tab.rawValue)")
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(active ? Theme.gold : Color.primary)
    }

    @ViewBuilder
    private var folderRow: some View {
        if let url = pickedURL {
            VStack(alignment: .leading, spacing: 4) {
                Text(url.lastPathComponent)
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1).truncationMode(.middle)
                    .foregroundStyle(Theme.gold)
                Text(url.deletingLastPathComponent().path)
                    .font(.system(size: 10))
                    .foregroundStyle(.secondary)
                    .lineLimit(1).truncationMode(.head)
                Button(action: pickFolder) {
                    Label("Change folder…", systemImage: "folder")
                        .font(.callout)
                }
                .buttonStyle(.plain)
                .foregroundStyle(.secondary)
                .padding(.top, 2)
            }
        } else {
            Button(action: pickFolder) {
                Label("Pick a folder…", systemImage: "folder.badge.plus")
                    .foregroundStyle(Theme.gold)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .keyboardShortcut("o", modifiers: .command)
        }
    }

    @ViewBuilder
    private func engineErrorRow(_ err: EngineError) -> some View {
        HStack(alignment: .top, spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
            VStack(alignment: .leading, spacing: 2) {
                Text(err.kind).font(.caption.bold())
                Text(err.message)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
            }
            Spacer(minLength: 4)
            Button { engine.clearLastError() } label: {
                Image(systemName: "xmark.circle.fill")
                    .foregroundStyle(.secondary)
            }
            .buttonStyle(.plain)
            .help("Dismiss")
        }
        .padding(8)
        .background(
            RoundedRectangle(cornerRadius: 6)
                .fill(Color.orange.opacity(0.08))
        )
    }

    private func pickFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.message = "Pick a folder to scan"
        if panel.runModal() == .OK, let url = panel.url {
            if pickedURL != url {
                engine.clearProgress()
            }
            pickedURL = url
        }
    }
}

// MARK: - Processing Control
// Three states: pre-scan / active / terminal (done/cancelled/failed).

private struct ProcessingControl: View {
    let engine: EngineClient
    let store: ReadStore
    let pickedURL: URL
    let changePickedURL: (URL?) -> Void

    /// Optimistic flag set when Start is clicked, cleared on the first
    /// engine phase event. Suppresses spam-click double-starts.
    @State private var startRequested = false

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            // Whole-pipeline indicator is always visible — even when
            // nothing is in flight — so the user can see at a glance
            // where they are in the workflow (e.g., post-scan they
            // know "now go name people, then run Deep Analyze").
            PipelineProgress(engine: engine, store: store)
                .padding(.vertical, 2)

            if let p = engine.lastProgress, p.phase != .idle {
                liveOrDone(p)
            } else {
                preScan
            }
        }
        .padding(.vertical, 4)
        .onChange(of: engine.lastProgress?.phase) { _, new in
            if new != nil && new != .idle {
                startRequested = false
            }
        }
    }

    // MARK: Pre-scan

    @ViewBuilder
    private var preScan: some View {
        HStack {
            Image(systemName: startRequested ? "hourglass" : "moon.zzz")
                .foregroundStyle(.secondary)
            Text(startRequested ? "Starting…" : "Idle")
                .font(.caption.bold()).foregroundStyle(.secondary)
            Spacer()
        }
        Text(startRequested
             ? "Sent to engine. The first phase event will appear shortly."
             : "Ready to scan this folder. Click Start when you're ready.")
            .font(.caption2)
            .foregroundStyle(.secondary)
        pillButton("Start Scan", color: Theme.gold, system: "play.fill",
                    filled: true, disabled: startRequested) {
            startRequested = true
            engine.startScan(rootURL: pickedURL)
        }
        .keyboardShortcut("r", modifiers: .command)
    }

    // MARK: Live or terminal

    @ViewBuilder
    private func liveOrDone(_ p: ScanProgress) -> some View {
        HStack {
            Image(systemName: phaseIcon(p.phase)).foregroundStyle(Theme.gold)
            BadgePill(label: p.phase.rawValue.capitalized)
            Spacer()
        }

        Text(statusText(p))
            .font(.caption.bold())
            .foregroundStyle(.primary)

        if p.phase == .tagging, p.total > 0 {
            ProgressView(value: Double(p.processed), total: Double(max(p.total, 1)))
                .tint(Theme.gold)
            HStack {
                Text("\(p.processed) / \(p.total)")
                    .font(.caption2.monospacedDigit())
                Spacer()
                let pct = Double(p.processed) / Double(max(p.total, 1)) * 100
                Text(String(format: "%.1f%%", pct))
                    .font(.caption2.bold().monospacedDigit())
                    .foregroundStyle(Theme.gold)
            }
        } else if p.phase == .discovering {
            ProgressView().controlSize(.small)
        }

        VStack(alignment: .leading, spacing: 4) {
            statRow(icon: "magnifyingglass",
                     label: "\(p.discovered.formatted()) found",
                     trailing: (p.etaSeconds.flatMap { $0 > 0 ? "\(formatETA($0)) left" : nil }),
                     trailingTint: Theme.gold)
            if p.processed > 0 {
                statRow(icon: "tag",
                         label: "\(p.processed.formatted()) tagged",
                         trailing: String(format: "%.1f/s", p.filesPerSecond),
                         trailingTint: Theme.gold)
            }
            statRow(icon: "memorychip",
                     label: "\(p.residentMB) MB used",
                     labelTint: p.residentMB > 1200 ? .orange : .secondary,
                     trailing: "\(p.availableMB) MB free",
                     trailingTint: .secondary)
            if p.failed > 0 {
                statRow(icon: "exclamationmark.triangle",
                         label: "\(p.failed) failed",
                         labelTint: .red,
                         trailing: nil,
                         trailingTint: .secondary)
            }
        }

        if engine.isPaused {
            HStack(spacing: 6) {
                Image(systemName: "pause.circle.fill").foregroundStyle(.orange)
                VStack(alignment: .leading, spacing: 1) {
                    Text("Paused")
                        .font(.caption.bold())
                        .foregroundStyle(.orange)
                    Text("Workers idle. Click Resume to continue or Cancel to stop.")
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                Spacer()
            }
            .padding(8)
            .background(Color.orange.opacity(0.10))
            .overlay(
                RoundedRectangle(cornerRadius: 6).stroke(Color.orange.opacity(0.5), lineWidth: 1)
            )
            .clipShape(RoundedRectangle(cornerRadius: 6))
        }
        // Engine holds an IOPMAssertion: lid-close on AC won't pause the scan.
        HStack(spacing: 4) {
            Image(systemName: "moon.zzz.fill").font(.caption2).foregroundStyle(.green.opacity(0.7))
            Text("System sleep blocked while scan runs (lid-closed safe on AC)")
                .font(.system(size: 9))
                .foregroundStyle(.secondary)
        }
        HStack(spacing: 8) {
            if engine.isPaused {
                pillButton("Resume", color: .green, system: "play.fill", filled: true) {
                    engine.resume()
                }
                pillButton("Cancel", color: .red, system: "xmark.circle.fill") {
                    engine.cancel()
                }
            } else if p.phase == .tagging || p.phase == .discovering || p.phase == .postScan {
                pillButton("Pause", color: .orange, system: "pause.fill") {
                    engine.pause()
                }
                pillButton("Cancel", color: .red, system: "xmark.circle.fill") {
                    engine.cancel()
                }
            } else {
                pillButton("Rescan", color: Theme.gold, system: "arrow.counterclockwise", filled: true) {
                    engine.startScan(rootURL: pickedURL)
                }
            }
        }
    }

    @ViewBuilder
    private func statRow(icon: String, label: String,
                          labelTint: Color = .secondary,
                          trailing: String?,
                          trailingTint: Color) -> some View {
        HStack(spacing: 6) {
            Image(systemName: icon)
                .font(.caption2)
                .foregroundStyle(labelTint)
                .frame(width: 14, alignment: .leading)
            Text(label)
                .font(.caption2.monospacedDigit())
                .foregroundStyle(labelTint)
            Spacer(minLength: 8)
            if let trailing {
                Text(trailing)
                    .font(.caption2.bold().monospacedDigit())
                    .foregroundStyle(trailingTint)
            }
        }
    }

    private func phaseIcon(_ phase: ScanPhase) -> String {
        switch phase {
        case .idle:        return "moon.zzz"
        case .discovering: return "magnifyingglass"
        case .tagging:     return "brain.head.profile"
        case .postScan:    return "wand.and.stars"
        case .completed:   return "checkmark.seal.fill"
        case .cancelled:   return "xmark.octagon"
        case .failed:      return "exclamationmark.triangle"
        }
    }

    private func statusText(_ p: ScanProgress) -> String {
        switch p.phase {
        case .idle:        return "Idle"
        case .discovering: return "Discovering files…"
        case .tagging:     return "Tagging files…"
        case .postScan:    return "Post-scan…"
        case .completed:   return "Done — \(p.processed) of \(p.total) tagged"
        case .cancelled:   return "Cancelled — \(p.processed) of \(p.total) tagged"
        case .failed:      return "Failed"
        }
    }

    private func formatETA(_ seconds: Double) -> String {
        let s = Int(seconds.rounded())
        let h = s / 3600
        let m = (s % 3600) / 60
        let sec = s % 60
        if h > 0 { return String(format: "%dh %dm", h, m) }
        if m > 0 { return String(format: "%dm %ds", m, sec) }
        return "\(sec)s"
    }

    @ViewBuilder
    private func pillButton(_ title: String, color: Color, system: String,
                             filled: Bool = false,
                             disabled: Bool = false,
                             action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Label(title, systemImage: system)
                .font(.caption.bold())
                .foregroundStyle(disabled ? color.opacity(0.5)
                                  : (filled ? Color.black : color))
                .frame(maxWidth: .infinity)
                .padding(.vertical, 6)
                .background(filled ? color.opacity(disabled ? 0.4 : 1.0)
                                   : color.opacity(0.18))
                .overlay(
                    RoundedRectangle(cornerRadius: 6)
                        .stroke(color.opacity(filled ? 1.0 : 0.6), lineWidth: 1)
                )
                .clipShape(RoundedRectangle(cornerRadius: 6))
        }
        .buttonStyle(.plain)
        .disabled(disabled)
    }
}

// MARK: - Pipeline dots

/// Whole-workflow indicator: Scan → Tag → People → Captions → Done.
/// Reads engine signals + cheap DB counters so it stays accurate even
/// across launches (i.e. when `engine.lastProgress` is nil but the DB
/// already has a clustered library from a prior session).
struct PipelineProgress: View {
    let engine: EngineClient
    let store: ReadStore

    enum Stage: Int, CaseIterable, Identifiable {
        case scan = 0, tag, people, captions, done
        var id: Int { rawValue }
        var label: String {
            switch self {
            case .scan:     return "Scan"
            case .tag:      return "Tag"
            case .people:   return "People"
            case .captions: return "Captions"
            case .done:     return "Done"
            }
        }
    }

    /// Where the user is in the workflow right now. Live signals win
    /// over DB-derived state so the bar tracks an in-flight stage.
    private var current: Stage {
        if let p = engine.lastProgress {
            switch p.phase {
            case .discovering: return .scan
            case .tagging:     return .tag
            case .postScan:    return .people
            case .completed, .cancelled, .failed, .idle: break
            }
        }
        if engine.faceClusteringInFlight { return .people }
        if engine.deepAnalyzeInFlight    { return .captions }

        // Nothing in flight — derive from the DB state.
        let scanned   = store.totalFiles > 0
        let clustered = store.totalFacePrints() > 0
        let named     = store.namedPersonCount() > 0
        let captioned = store.totalCaptioned() > 0
        if !scanned   { return .scan }
        if !clustered { return .people }   // clustering still pending
        if !named     { return .people }   // user hasn't named anyone yet
        if !captioned { return .captions } // Deep Analyze still pending
        return .done
    }

    private func state(for s: Stage) -> (filled: Bool, active: Bool) {
        let c = current
        // Done is "filled" only when current = done (everything's complete).
        // Otherwise every stage strictly before the current one is filled,
        // and the current stage itself is active.
        let filled = s.rawValue < c.rawValue || c == .done
        let active = s == c
        return (filled, active)
    }

    var body: some View {
        // 5 equal columns; each column has its dot centered above its
        // label so they always align vertically. Connector segments live
        // in the same column as the dot — left half + right half — so
        // they meet between adjacent dots without offsetting them.
        let stages = Stage.allCases
        HStack(spacing: 0) {
            ForEach(Array(stages.enumerated()), id: \.element.id) { idx, s in
                let st = state(for: s)
                let prevFilled = idx > 0 ? state(for: stages[idx - 1]).filled : false
                VStack(spacing: 4) {
                    ZStack {
                        // Left connector — only when not the first dot.
                        // Filled when the PREVIOUS stage is filled (the
                        // segment "leads into" this dot from the left).
                        if idx > 0 {
                            HStack(spacing: 0) {
                                Rectangle()
                                    .fill(prevFilled ? Theme.gold : Color.white.opacity(0.10))
                                    .frame(height: 1)
                                Spacer(minLength: 0)
                            }
                        }
                        // Right connector — only when not the last dot.
                        if idx < stages.count - 1 {
                            HStack(spacing: 0) {
                                Spacer(minLength: 0)
                                Rectangle()
                                    .fill(st.filled ? Theme.gold : Color.white.opacity(0.10))
                                    .frame(height: 1)
                            }
                        }
                        dotCell(state: st)
                    }
                    .frame(height: 14)
                    Text(s.label)
                        .font(.system(size: 8, weight: .semibold))
                        .foregroundStyle(st.active ? Theme.gold
                                          : (st.filled ? Color.primary : Color.secondary))
                }
                .frame(maxWidth: .infinity)
            }
        }
        .padding(.horizontal, 4)
    }

    @ViewBuilder
    private func dotCell(state st: (filled: Bool, active: Bool)) -> some View {
        // Active dot grows by frame, not scale, to keep layout stable.
        let size: CGFloat = st.active ? 12 : 8
        let fill: Color = st.filled
            ? Theme.gold
            : (st.active ? Theme.gold.opacity(0.6) : Color.white.opacity(0.12))
        let stroke: Color = st.active ? Theme.gold : Color.white.opacity(0.18)
        Circle()
            .fill(fill)
            .frame(width: size, height: size)
            .overlay(Circle().stroke(stroke, lineWidth: st.active ? 1.5 : 1))
            .shadow(color: st.active ? Theme.gold.opacity(0.55) : .clear,
                    radius: st.active ? 4 : 0)
    }
}

// MARK: - Job queue

struct QueueListView: View {
    let state: QueueState

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Image(systemName: "list.bullet.rectangle")
                    .foregroundStyle(Theme.gold)
                Text("\(state.pending.count) waiting")
                    .font(.caption.bold())
                Spacer()
                if let eta = state.totalEtaSeconds, eta > 0 {
                    Text(formatETA(eta))
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(Theme.gold)
                }
            }
            ForEach(state.pending) { job in
                HStack(spacing: 6) {
                    Image(systemName: icon(for: job.category))
                        .font(.system(size: 11))
                        .foregroundStyle(.secondary)
                        .frame(width: 14)
                    VStack(alignment: .leading, spacing: 1) {
                        Text(job.title)
                            .font(.caption)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        if let eta = job.etaSeconds, eta > 0 {
                            Text("ETA \(formatETA(eta))")
                                .font(.caption2.monospacedDigit())
                                .foregroundStyle(.tertiary)
                        }
                    }
                    Spacer()
                }
            }
        }
    }

    private func icon(for c: JobCategory) -> String {
        switch c {
        case .scan:         return "magnifyingglass"
        case .faceCluster:  return "person.2.crop.square.stack"
        case .deepAnalyze:  return "sparkles"
        }
    }

    private func formatETA(_ seconds: Double) -> String {
        let s = max(0, Int(seconds.rounded()))
        let h = s / 3600
        let m = (s % 3600) / 60
        let sec = s % 60
        if h > 0 { return String(format: "%dh %dm", h, m) }
        if m > 0 { return String(format: "%dm %ds", m, sec) }
        return "\(sec)s"
    }
}

// MARK: - Engine Status

private struct EngineStatusRow: View {
    let state: EngineClient.ConnectionState
    var body: some View {
        switch state {
        case .starting:
            Label("Starting…", systemImage: "hourglass")
                .font(.callout)
                .foregroundStyle(.secondary)
        case .ready(let info):
            Label("Ready", systemImage: "checkmark.circle.fill")
                .font(.callout.bold())
                .foregroundStyle(.green)
                .help("\(info.workerCap) workers · \(Int(info.physicalMemoryGB)) GB RAM · pid \(info.pid)")
        case .crashed(let reason):
            Label(reason, systemImage: "xmark.octagon.fill")
                .font(.caption)
                .foregroundStyle(.red)
                .lineLimit(2)
        }
    }
}
