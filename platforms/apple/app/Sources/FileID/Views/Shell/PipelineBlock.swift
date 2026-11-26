import SwiftUI
import FileIDShared

/// Sidebar footer: where the library is in the pipeline, one grained bar,
/// stage labels, and the scan controls.
struct PipelineBlock: View {
    let engine: EngineClient
    let store: ReadStore
    let pickedURL: URL
    var openPage: (MainWindow.Tab) -> Void = { _ in }
    @State private var startRequested = false
    /// The newest scan of this folder, when it stopped before it finished (quit, killed, or failed). Read from the catalog's session rows so it survives a relaunch.
    @State private var unfinishedScan: ReadStore.ScanSessionRow?

    /// Describe Files is optional, so it is not a step: a finished scan is Done.
    private enum Stage: Int, CaseIterable { case scan, tag, finish, faces, done
        var label: String { ["Find files", "Tag", "Finish up", "Faces", "Done"][rawValue] }
    }

    private var progress: ScanProgress? { engine.lastProgress }
    private var scanRunning: Bool {
        guard let p = progress else { return false }
        return p.phase == .discovering || p.phase == .tagging || p.phase == .postScan
    }
    private var captionsRunning: Bool { engine.deepAnalyzeInFlight }
    private var helperDown: Bool { HelperGate.isDown(engine.state) }

    private var stage: Stage {
        if let p = progress {
            switch p.phase {
            case .discovering: return .scan
            case .tagging: return .tag
            case .postScan: return .finish
            default: break
            }
        }
        if engine.faceClusteringInFlight { return .faces }
        if showsUnfinishedScan { return .scan }
        if store.totalFiles > 0 || progress?.phase == .completed { return .done }
        return .scan
    }

    private var locationOffline: Bool {
        OfflineRoots(engine.libraryRoots).isOffline(pickedURL.path) || OfflineRoots.allOffline(engine.libraryRoots)
    }

    private var showsUnfinishedScan: Bool {
        unfinishedScan != nil && !scanRunning && progress?.phase != .completed
    }

    /// The newest session of `root` when it is not `completed`. A `running` row means a scan is starting right now, not that one was interrupted.
    nonisolated static func unfinishedScan(in sessions: [ReadStore.ScanSessionRow], root: URL) -> ReadStore.ScanSessionRow? {
        let wanted = root.standardizedFileURL.resolvingSymlinksInPath().path
        guard let newest = sessions.first(where: {
            URL(fileURLWithPath: $0.rootPath).standardizedFileURL.resolvingSymlinksInPath().path == wanted
        }) else { return nil }
        return newest.status == "cancelled" || newest.status == "failed" ? newest : nil
    }

    nonisolated static func unfinishedScanTitle(filesRead: Int) -> String {
        filesRead > 0 ? "Tags aren’t finished yet" : "The scan didn’t finish"
    }

    nonisolated static func unfinishedScanDetail(filesRead: Int) -> String {
        filesRead > 0
            ? "\(filesRead.formatted()) file\(filesRead == 1 ? "" : "s") read and ready to browse. Press Scan again to finish adding tags."
            : "Press Scan again to read the rest."
    }

    nonisolated static func postScanDetail(_ work: PostScanWork, etaSeconds: Double?) -> String {
        let left = etaSeconds.flatMap { $0 > 0 ? " · about \(DurationText.short($0)) left" : nil } ?? ""
        return "\(work.done.formatted()) of \(Plural.noun(work.owed, "file", "files"))" + left
    }

    nonisolated static func stoppedDuringPostScanDetail(filesRead: Int, left: Int) -> String {
        "\(Plural.noun(filesRead, "file", "files")) read and ready to browse. \(Plural.noun(left, "file", "files")) still need tags. Press Scan again to finish; finished files are skipped."
    }

    static let cpuScanNotice = "Scanning on the CPU — slower than usual. Reinstall FileID if this persists."

    private struct Status {
        let title: String
        let fraction: Double
        let tone: ProgressTone
        let color: Color
        let sub: String
        var indeterminate = false
    }

    private var state: Status {
        if let p = progress, scanRunning {
            let frac = p.total > 0 ? Double(p.processed) / Double(p.total) : 0
            if engine.isPaused {
                return Status(title: "Scan paused", fraction: frac, tone: .paused, color: Theme.text3, sub: "\(p.processed.formatted()) of \(Plural.noun(max(p.total, p.discovered), "file", "files"))")
            }
            switch p.phase {
            case .discovering:
                return Status(title: "Counting files", fraction: 0, tone: .gold, color: Theme.gold, sub: "\(Plural.noun(p.discovered, "file", "files")) found so far", indeterminate: true)
            case .postScan:
                if let work = p.postScanWork {
                    return Status(title: "Tagging photos", fraction: work.fraction, tone: .gold, color: Theme.gold, sub: Self.postScanDetail(work, etaSeconds: p.etaSeconds))
                }
                return Status(title: "Finishing up", fraction: 0, tone: .gold, color: Theme.gold, sub: "Saving what FileID found", indeterminate: true)
            default:
                let left = p.etaSeconds.flatMap { $0 > 0 ? " · about \(DurationText.short($0)) left" : nil } ?? ""
                return Status(title: "Scanning", fraction: frac, tone: .gold, color: Theme.gold, sub: "\(p.processed.formatted()) of \(Plural.noun(max(p.total, p.discovered), "file", "files"))" + left)
            }
        }
        if captionsRunning, let d = engine.deepAnalyzeProgress {
            let frac = d.total > 0 ? Double(d.processed) / Double(d.total) : 0
            return Status(title: "Describing files", fraction: frac, tone: .ai, color: Theme.ai, sub: "\(d.processed.formatted()) of \(Plural.noun(d.total, "file", "files")) described")
        }
        if engine.faceClusteringInFlight {
            return Status(title: "Grouping faces…", fraction: 0, tone: .ai, color: Theme.ai, sub: "Finding the same person across photos", indeterminate: true)
        }
        if let done = engine.deepAnalyzeComplete, done.processed > 0 {
            return Status(title: "All done", fraction: 1, tone: .done, color: Theme.delight, sub: "\(Plural.noun(done.processed, "file", "files")) described")
        }
        if let p = progress, p.phase == .cancelled || p.phase == .failed,
           let baseline = engine.postScanBaseline, baseline.sessionID == p.sessionID,
           let work = p.postScanWork, work.left > 0 {
            return Status(title: p.phase == .cancelled ? "Scan stopped" : Self.unfinishedScanTitle(filesRead: baseline.filesRead),
                          fraction: work.fraction, tone: .paused,
                          color: p.phase == .cancelled ? Theme.text3 : Theme.warning,
                          sub: Self.stoppedDuringPostScanDetail(filesRead: baseline.filesRead, left: work.left))
        }
        if let p = progress, p.phase == .cancelled {
            return Status(title: "Scan stopped", fraction: p.total > 0 ? Double(p.processed) / Double(p.total) : 0, tone: .paused, color: Theme.text3, sub: "\(p.processed.formatted()) of \(Plural.noun(p.total, "file", "files")) read. Scan again to finish; finished files are skipped.")
        }
        if let p = progress, p.phase == .failed {
            return Status(title: "The scan didn’t finish", fraction: 0, tone: .paused, color: Theme.danger, sub: "Press Scan again. Files already read are skipped.")
        }
        if showsUnfinishedScan {
            return Status(title: Self.unfinishedScanTitle(filesRead: store.totalFiles), fraction: 0, tone: .paused, color: Theme.warning, sub: Self.unfinishedScanDetail(filesRead: store.totalFiles))
        }
        if store.totalFiles > 0 || progress?.phase == .completed {
            return Status(title: "Scan complete", fraction: 1, tone: .done, color: Theme.delight, sub: "\(Plural.noun(store.totalFiles, "file", "files")) ready to browse")
        }
        if !store.countersAvailable {
            return helperDown
                ? Status(title: "Library not open", fraction: 0, tone: .paused, color: Theme.text3, sub: "Restart FileID’s helper to read it.")
                : Status(title: "Opening your library…", fraction: 0, tone: .gold, color: Theme.text2, sub: "Counting the files FileID has already read.")
        }
        return Status(title: startRequested ? "Starting…" : "Ready to scan", fraction: 0, tone: .gold, color: Theme.text2, sub: "Reads every file in it. Nothing is moved or changed.")
    }

    private var stepText: String {
        if captionsRunning && !scanRunning { return "Optional · Describe Files" }
        if let phase = progress?.phase, phase == .cancelled || phase == .failed { return "Not finished" }
        if showsUnfinishedScan { return "Not finished" }
        return stage == .done ? "4 of 4 steps finished" : "Step \(stage.rawValue + 1) of 4 · \(stage.label)"
    }

    @State private var showDetails = false

    private var showsNextStep: Bool {
        !scanRunning && stage == .done && !captionsRunning && !engine.faceClusteringInFlight
            && (engine.deepAnalyzeComplete?.processed ?? 0) == 0 && store.totalFiles > 0
    }

    var body: some View {
        let s = state
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text(s.title).font(Theme.ChromeFont.body(13, .semibold)).foregroundStyle(Theme.text1).lineLimit(1)
                Spacer()
                if s.fraction > 0 && !s.indeterminate { Text("\(Int(s.fraction * 100))%").font(Theme.ChromeFont.body(12.5, .semibold)).monospacedDigit().foregroundStyle(s.color) }
            }
            if s.indeterminate {
                IndeterminateBar().frame(height: 6)
            } else {
                GrainProgressBar(progress: s.fraction, tone: s.tone, height: 6)
            }
            HStack(spacing: 4) {
                ForEach(Stage.allCases, id: \.rawValue) { st in
                    Capsule()
                        .fill(st.rawValue < stage.rawValue ? Theme.text2.opacity(0.7)
                              : st == stage ? (stage == .done ? Theme.delight : s.color) : Color.white.opacity(0.12))
                        .frame(height: 3)
                }
            }
            HStack(spacing: 4) {
                Text(stepText).font(Theme.ChromeFont.body(12)).foregroundStyle(Theme.text2)
                Spacer(minLength: 4)
                if let p = progress, p.phase != .idle {
                    Button { withAnimation(Theme.springStandard) { showDetails.toggle() } } label: {
                        Image(systemName: "chevron.down").font(.system(size: 10, weight: .semibold))
                            .rotationEffect(.degrees(showDetails ? 180 : 0))
                            .foregroundStyle(Theme.text3)
                            .frame(width: Theme.Metrics.hitTarget, height: Theme.Metrics.hitTarget)
                    }
                    .buttonStyle(.dsRow)
                    .help(showDetails ? "Hide scan details" : "Show scan details")
                    .accessibilityLabel(showDetails ? "Hide scan details" : "Show scan details")
                }
            }
            Text(s.sub).font(Theme.ChromeFont.body(12)).monospacedDigit().foregroundStyle(Theme.text3).lineLimit(2, reservesSpace: scanRunning || captionsRunning).fixedSize(horizontal: false, vertical: true)
            if engine.scanRunsOnCPU && scanRunning {
                Text(Self.cpuScanNotice).font(Theme.ChromeFont.body(12)).foregroundStyle(Theme.warning).fixedSize(horizontal: false, vertical: true)
            }
            if showsNextStep {
                Button { openPage(.deep) } label: {
                    HStack(spacing: 4) {
                        Text("Optional: describe your files")
                        Image(systemName: "arrow.right").font(.system(size: 10, weight: .semibold))
                    }
                    .font(Theme.ChromeFont.body(12, .medium))
                    .foregroundStyle(Theme.gold)
                    .frame(minHeight: 22)
                    .contentShape(Rectangle())
                }
                .buttonStyle(DSBareButtonStyle(radius: Theme.Radius.s))
                .help("Write a description and a clearer name for each file. You can skip this.")
            }
            if showDetails, let p = progress, p.phase != .idle {
                stats(p).transition(.opacity)
            }
            controls
        }
        .task(id: UnfinishedScanKey(version: store.version, phase: progress?.phase.rawValue, root: pickedURL.path, engineReady: engineReady)) {
            let readable = store
            let sessions = await Task.detached(priority: .utility) { readable.recentSessions(limit: 20) }.value
            unfinishedScan = Self.unfinishedScan(in: sessions, root: pickedURL)
        }
        .onChange(of: engine.lastProgress?.phase) { _, new in if new != nil && new != .idle { startRequested = false } }
        .onChange(of: engine.lastErrorSignal) { _, _ in startRequested = false }
        .onChange(of: engine.engineResetSignal) { _, _ in startRequested = false }
    }

    private struct UnfinishedScanKey: Hashable {
        let version: Int
        let phase: String?
        let root: String
        /// The engine settles a scan a crash left `running` as `failed` when it starts, so the sessions are read again once it is ready.
        let engineReady: Bool
    }

    private var engineReady: Bool {
        if case .ready = engine.state { return true }
        return false
    }

    /// Scan counters: found, read, speed and failures.
    @ViewBuilder
    private func stats(_ p: ScanProgress) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            statRow("Found", p.discovered.formatted(), trailing: p.etaSeconds.flatMap { $0 > 0 && scanRunning ? DurationText.short($0) + " left" : nil })
            statRow("Read", p.processed.formatted(), trailing: p.filesPerSecond > 0 ? String(format: "%.0f files a second", p.filesPerSecond) : nil)
            if p.failed > 0 { statRow("Failed", p.failed.formatted(), trailing: nil, danger: true) }
        }
        .padding(.vertical, 6)
        .overlay(alignment: .top) { Hairline(color: Theme.hairlineSoft) }
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
    }

    private func statRow(_ key: String, _ value: String, trailing: String?, warn: Bool = false, danger: Bool = false) -> some View {
        HStack(spacing: 6) {
            Text(key).font(Theme.ChromeFont.body(12)).foregroundStyle(Theme.text3).frame(width: 62, alignment: .leading)
            Text(value).font(Theme.ChromeFont.body(12, .medium)).monospacedDigit().foregroundStyle(danger ? Theme.danger : warn ? Theme.warning : Theme.text1)
            Spacer(minLength: 4)
            if let trailing { Text(trailing).font(Theme.ChromeFont.body(11.5)).monospacedDigit().foregroundStyle(Theme.text3) }
        }
    }

    @ViewBuilder
    private var controls: some View {
        HStack(spacing: 6) {
            if scanRunning {
                if engine.isPaused {
                    Button { engine.resume() } label: { Label("Resume", systemImage: "play.fill").frame(maxWidth: .infinity) }.buttonStyle(DSPrimaryButtonStyle(height: 28))
                } else {
                    Button { engine.pause() } label: { Label("Pause", systemImage: "pause.fill").frame(maxWidth: .infinity) }.buttonStyle(DSSecondaryButtonStyle(height: 28))
                }
                Button("Stop") { engine.cancelScan() }.buttonStyle(DSGhostButtonStyle(color: Theme.text3)).help("Stop scanning. Scan again later to pick up where it left off.")
            } else if store.totalFiles > 0 || progress != nil {
                Button { engine.startScan(rootURL: pickedURL) } label: { Label(progress?.phase == .cancelled || progress?.phase == .failed || showsUnfinishedScan ? "Scan again" : "Rescan", systemImage: "arrow.counterclockwise").lineLimit(1).frame(maxWidth: .infinity) }
                    .buttonStyle(DSSecondaryButtonStyle(height: 28))
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(HelperGate.rescanDisabled(startRequested: startRequested, captionsRunning: captionsRunning, faceClusteringInFlight: engine.faceClusteringInFlight, helperDown: helperDown, locationOffline: locationOffline))
                    .help(helperDown ? "FileID’s helper isn’t running. Restart it, then scan again." : locationOffline ? "This location is offline. Reconnect the drive to rescan." : captionsRunning || engine.faceClusteringInFlight ? "Wait for the current work to finish, then rescan." : "Look for new or changed files in \(pickedURL.lastPathComponent). Files already read are skipped.")
            } else {
                Button { startRequested = true; engine.startScan(rootURL: pickedURL) } label: { Label("Scan \(pickedURL.lastPathComponent)", systemImage: "play.fill").lineLimit(1).truncationMode(.middle).frame(maxWidth: .infinity) }
                    .buttonStyle(DSPrimaryButtonStyle(height: 28))
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(HelperGate.scanDisabled(startRequested: startRequested, helperDown: helperDown))
                    .help(helperDown ? "FileID’s helper isn’t running. Restart it, then scan." : "Read every file in this folder (⌘R). Nothing is moved or changed.")
            }
        }
        .frame(maxWidth: .infinity)
    }
}
