import SwiftUI
import FileIDShared

/// Sidebar footer: where the library is in the pipeline, one grained bar,
/// stage labels, and the scan controls.
struct PipelineBlock: View {
    let engine: EngineClient
    let store: ReadStore
    let pickedURL: URL
    @State private var startRequested = false

    private enum Stage: Int, CaseIterable { case scan, tag, faces, captions, done
        var label: String { ["Scan", "Tag", "Faces", "Captions", "Done"][rawValue] }
    }

    private var progress: ScanProgress? { engine.lastProgress }
    private var scanRunning: Bool {
        guard let p = progress else { return false }
        return p.phase == .discovering || p.phase == .tagging || p.phase == .postScan
    }
    private var captionsRunning: Bool { engine.deepAnalyzeInFlight }

    private var stage: Stage {
        if let p = progress {
            switch p.phase {
            case .discovering: return .scan
            case .tagging: return .tag
            case .postScan: return .faces
            default: break
            }
        }
        if engine.faceClusteringInFlight { return .faces }
        if captionsRunning { return .captions }
        if let done = engine.deepAnalyzeComplete, done.processed > 0 { return .done }
        if store.totalFiles > 0 || progress?.phase == .completed { return .captions }
        return .scan
    }

    private var state: (title: String, fraction: Double, tone: ProgressTone, color: Color, sub: String) {
        if let p = progress, scanRunning {
            let frac = p.total > 0 ? Double(p.processed) / Double(p.total) : 0
            if engine.isPaused {
                return ("Scan paused", frac, .paused, Theme.text3, "\(p.processed.formatted()) of \(max(p.total, p.discovered).formatted()) files")
            }
            let title = p.phase == .discovering ? "Counting files" : p.phase == .postScan ? "Grouping faces" : "Scanning"
            let sub = p.phase == .discovering ? "\(p.discovered.formatted()) found" : "\(p.processed.formatted()) of \(max(p.total, p.discovered).formatted()) files" + (p.filesPerSecond > 0 ? String(format: " · %.0f/s", p.filesPerSecond) : "")
            return (title, p.phase == .discovering ? 0.02 : frac, .gold, Theme.gold, sub)
        }
        if captionsRunning, let d = engine.deepAnalyzeProgress {
            let frac = d.total > 0 ? Double(d.processed) / Double(d.total) : 0
            return ("Writing captions", frac, .ai, Theme.ai, "\(d.processed.formatted()) of \(d.total.formatted()) captioned")
        }
        if engine.faceClusteringInFlight {
            return ("Grouping faces", 0.5, .ai, Theme.ai, "Comparing face prints")
        }
        if let done = engine.deepAnalyzeComplete, done.processed > 0 {
            return ("Everything analyzed", 1, .done, Theme.delight, "\(done.processed.formatted()) files captioned")
        }
        if let p = progress, p.phase == .cancelled {
            return ("Scan cancelled", p.total > 0 ? Double(p.processed) / Double(p.total) : 0, .paused, Theme.text3, "\(p.processed.formatted()) of \(p.total.formatted()) tagged")
        }
        if let p = progress, p.phase == .failed {
            return ("Scan failed", 0, .paused, Theme.danger, "Check the engine log in Settings")
        }
        if store.totalFiles > 0 || progress?.phase == .completed {
            return ("Scan complete", 1, .gold, Theme.gold, "\(store.totalFiles.formatted()) files · captions not started")
        }
        return (startRequested ? "Starting" : "Not scanned yet", 0, .gold, Theme.text2, "\(pickedURL.lastPathComponent) is ready to scan")
    }

    @State private var showDetails = false

    var body: some View {
        let s = state
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline) {
                Text(s.title).font(Theme.Font.body(13, .semibold)).foregroundStyle(Theme.text1).lineLimit(1)
                Spacer()
                Text("\(Int(s.fraction * 100))%").font(Theme.Font.body(12.5, .semibold)).monospacedDigit().foregroundStyle(s.color)
            }
            GrainProgressBar(progress: s.fraction, tone: s.tone, height: 6)
            HStack(spacing: 4) {
                ForEach(Stage.allCases, id: \.rawValue) { st in
                    Capsule()
                        .fill(st.rawValue < stage.rawValue ? Theme.text2.opacity(0.7)
                              : st == stage ? (stage == .captions && captionsRunning ? Theme.ai : s.color) : Color.white.opacity(0.12))
                        .frame(height: 3)
                }
            }
            HStack(spacing: 4) {
                Text("Step \(stage.rawValue + 1) of 5 · \(stage.label)").font(Theme.Font.body(12)).foregroundStyle(Theme.text2)
                Spacer(minLength: 4)
                if let p = progress, p.phase != .idle {
                    Button { withAnimation(Theme.spring) { showDetails.toggle() } } label: {
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
            Text(s.sub).font(Theme.Font.body(12)).foregroundStyle(Theme.text3).lineLimit(2).fixedSize(horizontal: false, vertical: true)
            if showDetails, let p = progress, p.phase != .idle {
                stats(p).transition(.opacity)
            }
            controls
        }
        .onChange(of: engine.lastProgress?.phase) { _, new in if new != nil && new != .idle { startRequested = false } }
        .onChange(of: engine.lastErrorSignal) { _, _ in startRequested = false }
        .onChange(of: engine.engineResetSignal) { _, _ in startRequested = false }
    }

    /// The scan counters the previous sidebar showed: found, tagged, rate,
    /// engine memory and failures.
    @ViewBuilder
    private func stats(_ p: ScanProgress) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            statRow("Found", p.discovered.formatted(), trailing: p.etaSeconds.flatMap { $0 > 0 && scanRunning ? ProcessingControl.formatETA($0) + " left" : nil })
            statRow("Tagged", p.processed.formatted(), trailing: p.filesPerSecond > 0 ? String(format: "%.1f/s", p.filesPerSecond) : nil)
            statRow("Memory", "\(p.residentMB) MB", trailing: "\(p.availableMB) MB free", warn: p.residentMB > 1200)
            if p.failed > 0 { statRow("Failed", p.failed.formatted(), trailing: nil, danger: true) }
        }
        .padding(.vertical, 6)
        .overlay(alignment: .top) { Hairline(color: Theme.hairlineSoft) }
        .overlay(alignment: .bottom) { Hairline(color: Theme.hairlineSoft) }
    }

    private func statRow(_ key: String, _ value: String, trailing: String?, warn: Bool = false, danger: Bool = false) -> some View {
        HStack(spacing: 6) {
            Text(key).font(Theme.Font.body(12)).foregroundStyle(Theme.text3).frame(width: 62, alignment: .leading)
            Text(value).font(Theme.Font.body(12, .medium)).monospacedDigit().foregroundStyle(danger ? Theme.danger : warn ? Theme.warning : Theme.text1)
            Spacer(minLength: 4)
            if let trailing { Text(trailing).font(Theme.Font.body(11.5)).monospacedDigit().foregroundStyle(Theme.text3) }
        }
    }

    @ViewBuilder
    private var controls: some View {
        HStack(spacing: 6) {
            if scanRunning {
                if engine.isPaused {
                    Button { engine.resume() } label: { Label("Resume", systemImage: "play.fill") }.buttonStyle(DSPrimaryButtonStyle(height: 28))
                } else {
                    Button { engine.pause() } label: { Label("Pause", systemImage: "pause.fill") }.buttonStyle(DSSecondaryButtonStyle(height: 28))
                }
                Button("Cancel") { engine.cancel() }.buttonStyle(DSGhostButtonStyle(color: Theme.text3))
            } else if store.totalFiles > 0 || progress != nil {
                Button { engine.startScan(rootURL: pickedURL) } label: { Label("Rescan", systemImage: "arrow.counterclockwise") }
                    .buttonStyle(DSSecondaryButtonStyle(height: 28))
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(startRequested || captionsRunning || engine.faceClusteringInFlight)
            } else {
                Button { startRequested = true; engine.startScan(rootURL: pickedURL) } label: { Label("Scan \(pickedURL.lastPathComponent)", systemImage: "play.fill") }
                    .buttonStyle(DSPrimaryButtonStyle(height: 28))
                    .keyboardShortcut("r", modifiers: .command)
                    .disabled(startRequested)
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}
